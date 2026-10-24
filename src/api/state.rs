//! Per-pane agent state: debounced dispatch to the output adapters, rehydration from tmux,
//! the stale-pane reaper and the stall detector.

use crate::adapters::{AgentState, AgentUpdate, OutputAdapter};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct PaneStateInfo {
    pub last_timestamp: u64,
    pub seq_id: u64,
    pub state: AgentState,
    pub message: Option<String>,
    /// Epoch ms of the last working → idle transition seen live; 0 when unknown (e.g. panes
    /// rehydrated after a restart), so a restart never looks like freshly finished work.
    pub finished_at: u64,
    /// Epoch ms when the pane entered `state`; 0 when unknown (rehydrated), so guards that
    /// wait for a state to settle do not hold back a pane the daemon only learnt about.
    pub changed_at: u64,
}

#[derive(Debug, Clone)]
pub struct StallRecord {
    pub last_output_snapshot: String,
    pub last_cpu_ticks: u64,
    pub last_activity_instant: tokio::time::Instant,
}

#[derive(Clone)]
pub struct ApiState {
    pub adapters: Arc<Vec<Box<dyn OutputAdapter>>>,
    pub pending_idles:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Settings a config reload (SIGHUP) replaces.
    pub settings: Arc<std::sync::RwLock<Settings>>,
    pub pane_states: Arc<tokio::sync::Mutex<std::collections::HashMap<String, PaneStateInfo>>>,
    pub next_seq: Arc<std::sync::atomic::AtomicU64>,
    pub state_broadcast: tokio::sync::broadcast::Sender<AgentUpdate>,
    pub stall_records: Arc<tokio::sync::Mutex<std::collections::HashMap<String, StallRecord>>>,
    /// Prompts held for each pane until its agent is at rest (see `super::queue`).
    pub queues: Arc<tokio::sync::Mutex<super::queue::Queues>>,
    pub next_queue_id: Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Settings {
    pub idle_debounce_ms: u64,
    /// Program run on agent flow events (`flow_log_hook` in the config).
    pub flow_log_hook: Option<Arc<str>>,
}

impl Settings {
    pub fn from_config(config: &crate::config::Config) -> Self {
        Self {
            idle_debounce_ms: config.idle_debounce_ms.unwrap_or(650),
            flow_log_hook: config
                .flow_log_hook
                .as_deref()
                .map(|hook| Arc::from(crate::sys::expand_home(hook))),
        }
    }
}

impl ApiState {
    pub fn new(adapters: Vec<Box<dyn OutputAdapter>>, idle_debounce_ms: u64) -> Self {
        let (state_broadcast, _) = tokio::sync::broadcast::channel(128);
        Self {
            adapters: Arc::new(adapters),
            pending_idles: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            settings: Arc::new(std::sync::RwLock::new(Settings {
                idle_debounce_ms,
                flow_log_hook: None,
            })),
            pane_states: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            next_seq: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            state_broadcast,
            stall_records: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            queues: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            next_queue_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    pub fn settings(&self) -> Settings {
        crate::adapters::read(&self.settings).clone()
    }

    /// Applies a (re)loaded config: daemon settings here, theme/spinner/sound in each adapter.
    pub fn reload(&self, config: &crate::config::Config) {
        crate::adapters::write(&self.settings, Settings::from_config(config));
        for adapter in self.adapters.iter() {
            adapter.reload(config);
        }
    }

    pub async fn clean_stale_panes(&self) {
        let registered_panes: Vec<String> = {
            let states = self.pane_states.lock().await;
            states.keys().cloned().collect()
        };

        if registered_panes.is_empty() {
            return;
        }

        let output = match crate::tmux_cmd::async_command()
            .args(["list-panes", "-a", "-F", "#{pane_id}"])
            .output()
            .await
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => String::new(),
        };

        let active_panes: std::collections::HashSet<&str> = output.lines().collect();

        for pane_id in registered_panes {
            if pane_id.starts_with('%') && !active_panes.contains(pane_id.as_str()) {
                tracing::info!("Cleaning up dead pane {}", pane_id);
                let update = AgentUpdate {
                    pane_id: pane_id.clone(),
                    state: AgentState::Closed,
                    message: Some("pane closed".into()),
                };

                {
                    let mut pending = self.pending_idles.lock().await;
                    if let Some(task) = pending.remove(&pane_id) {
                        task.abort();
                    }
                }

                for adapter in self.adapters.iter() {
                    if let Err(e) = adapter.update(&update).await {
                        tracing::error!("Adapter error on dead pane cleanup: {}", e);
                    }
                }

                let _ = self.state_broadcast.send(update);

                {
                    let mut states = self.pane_states.lock().await;
                    states.remove(&pane_id);
                }
                {
                    let mut stalls = self.stall_records.lock().await;
                    stalls.remove(&pane_id);
                }
            }
        }
    }

    pub async fn rehydrate_from_raw_lines(&self, lines: &str, now_secs: u64) {
        for line in lines.lines() {
            let mut parts = line.split('\t');
            let pane_id = match parts.next() {
                Some(id) if !id.trim().is_empty() => id.trim(),
                _ => continue,
            };
            let Some(raw_state) = parts.next() else {
                continue;
            };
            // Completion time persisted on the pane by a previous daemon, if any.
            let finished_at = parts
                .next()
                .and_then(|t| t.trim().parse::<u64>().ok())
                .unwrap_or(0);

            if let Some(state) = AgentState::from_tmux_raw(raw_state) {
                let seq = self
                    .next_seq
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

                {
                    let mut states = self.pane_states.lock().await;
                    states.insert(
                        pane_id.to_string(),
                        PaneStateInfo {
                            last_timestamp: now_secs,
                            seq_id: seq,
                            state: state.clone(),
                            message: None,
                            finished_at,
                            changed_at: 0,
                        },
                    );
                }

                let update = AgentUpdate {
                    pane_id: pane_id.to_string(),
                    state,
                    message: None,
                };

                for adapter in self.adapters.iter() {
                    if let Err(e) = adapter.rehydrate(&update).await {
                        tracing::warn!("Adapter rehydrate error for pane {}: {}", pane_id, e);
                    }
                }
                let _ = self.state_broadcast.send(update.clone());
                tracing::info!("Rehydrated pane {} with state {:?}", pane_id, update.state);
            }
        }
    }

    pub async fn rehydrate_from_tmux(&self) {
        let output = match crate::tmux_cmd::async_command()
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{pane_id}\t#{@ai_agent_state_raw}\t#{@ai_agent_finished_at}",
            ])
            .output()
            .await
        {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
            _ => return,
        };

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());

        self.rehydrate_from_raw_lines(&output, now_secs).await;
    }

    pub async fn check_pane_stalls(&self) {
        // The heuristic needs per-process CPU from /proc. Without it (macOS) every quiet
        // agent would read as zero CPU and be flagged stalled, so detection stays off.
        if !std::path::Path::new("/proc/self/stat").exists() {
            return;
        }
        let target_panes: Vec<(String, AgentState)> = {
            let states = self.pane_states.lock().await;
            states
                .iter()
                .filter_map(|(pane_id, info)| {
                    if info.state == AgentState::Working || info.state == AgentState::Stalled {
                        Some((pane_id.clone(), info.state.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };

        if target_panes.is_empty() {
            return;
        }

        for (pane_id, current_state) in target_panes {
            let display_out = crate::tmux_cmd::async_command()
                .args([
                    "display-message",
                    "-p",
                    "-t",
                    &pane_id,
                    "#{pane_pid} #{cursor_x} #{cursor_y} #{history_size}",
                ])
                .output()
                .await;

            let (pane_pid, cursor_info) = match display_out {
                Ok(o) if o.status.success() => {
                    let out_str = String::from_utf8_lossy(&o.stdout);
                    let mut parts = out_str.split_whitespace();
                    let pid = parts
                        .next()
                        .and_then(|p| p.parse::<u32>().ok())
                        .unwrap_or(0);
                    let rest = parts.collect::<Vec<&str>>().join(":");
                    (pid, rest)
                }
                _ => continue,
            };

            let capture_out = crate::tmux_cmd::async_command()
                .args(["capture-pane", "-p", "-t", &pane_id, "-S", "-5"])
                .output()
                .await;

            let text_snapshot = match capture_out {
                Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
                _ => String::new(),
            };

            let current_snapshot = format!("{cursor_info}:{text_snapshot}");
            let current_cpu = if pane_pid > 0 {
                get_process_tree_cpu_ticks(pane_pid)
            } else {
                0
            };

            let mut to_dispatch: Option<IncomingUpdate> = None;

            {
                let mut stalls = self.stall_records.lock().await;
                let record = stalls
                    .entry(pane_id.clone())
                    .or_insert_with(|| StallRecord {
                        last_output_snapshot: current_snapshot.clone(),
                        last_cpu_ticks: current_cpu,
                        last_activity_instant: tokio::time::Instant::now(),
                    });

                let output_changed = record.last_output_snapshot != current_snapshot;
                let cpu_changed = current_cpu > record.last_cpu_ticks;

                if output_changed || cpu_changed {
                    record.last_output_snapshot = current_snapshot;
                    record.last_cpu_ticks = current_cpu;
                    record.last_activity_instant = tokio::time::Instant::now();

                    if current_state == AgentState::Stalled {
                        tracing::info!(
                            "Pane {} resumed activity; recovering from Stalled to Working",
                            pane_id
                        );
                        to_dispatch = Some(IncomingUpdate {
                            pane_id: pane_id.clone(),
                            state: AgentState::Working,
                            message: Some("recovered from stall".into()),
                            timestamp: None,
                        });
                    }
                } else if current_state == AgentState::Working
                    && record.last_activity_instant.elapsed() >= std::time::Duration::from_secs(20)
                {
                    tracing::warn!(
                        "Pane {} stalled (>20s silence and 0 CPU delta). Transitioning to Stalled.",
                        pane_id
                    );
                    to_dispatch = Some(IncomingUpdate {
                        pane_id: pane_id.clone(),
                        state: AgentState::Stalled,
                        message: Some("agent stalled (no output / CPU for 20s)".into()),
                        timestamp: None,
                    });
                }
            }

            if let Some(update) = to_dispatch {
                dispatch_update(self, update).await;
            }
        }
    }
}

fn get_process_tree_cpu_ticks(root_pid: u32) -> u64 {
    let mut ppid_map: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let mut ticks_map: std::collections::HashMap<u32, u64> = std::collections::HashMap::new();

    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let pid_str = match file_name.to_str() {
                Some(s) if s.chars().all(|c| c.is_ascii_digit()) => s,
                _ => continue,
            };
            let pid: u32 = match pid_str.parse() {
                Ok(p) => p,
                Err(_) => continue,
            };

            let stat_path = format!("/proc/{pid}/stat");
            if let Ok(content) = std::fs::read_to_string(stat_path)
                && let Some(rparen) = content.rfind(')')
            {
                let rest = &content[rparen + 1..];
                let fields: Vec<&str> = rest.split_whitespace().collect();
                if fields.len() > 13
                    && let (Ok(ppid), Ok(utime), Ok(stime)) = (
                        fields[1].parse::<u32>(),
                        fields[11].parse::<u64>(),
                        fields[12].parse::<u64>(),
                    )
                {
                    ppid_map.entry(ppid).or_default().push(pid);
                    ticks_map.insert(pid, utime.saturating_add(stime));
                }
            }
        }
    }

    let mut total_ticks: u64 = 0;
    let mut queue = vec![root_pid];
    let mut visited = std::collections::HashSet::new();

    while let Some(curr) = queue.pop() {
        if !visited.insert(curr) {
            continue;
        }
        if let Some(&ticks) = ticks_map.get(&curr) {
            total_ticks = total_ticks.saturating_add(ticks);
        }
        if let Some(children) = ppid_map.get(&curr) {
            for &child in children {
                queue.push(child);
            }
        }
    }

    total_ticks
}

/// Runs the user's `flow_log_hook` as `<hook> <event> <target> <extra>`, detached.
/// Without a configured hook nothing runs and nothing is written.
fn log_flow_event(hook: Option<&Arc<str>>, event: &str, target: &str, extra: &str) {
    let Some(hook) = hook.cloned() else {
        return;
    };
    let args = [event.to_string(), target.to_string(), extra.to_string()];
    tokio::spawn(async move {
        match tokio::process::Command::new(&*hook)
            .args(&args)
            .output()
            .await
        {
            Ok(out) if !out.status.success() => {
                tracing::debug!("flow_log_hook {} exited with {}", hook, out.status);
            }
            Err(e) => tracing::debug!("flow_log_hook {} failed to start: {}", hook, e),
            Ok(_) => {}
        }
    });
}

#[derive(Debug, Clone)]
pub(crate) struct IncomingUpdate {
    pub pane_id: String,
    pub state: AgentState,
    pub message: Option<String>,
    pub timestamp: Option<u64>,
}

fn flow_event_for_transition(prev: Option<&AgentState>, next: &AgentState) -> Option<&'static str> {
    match (prev, next) {
        (prev, next @ (AgentState::AwaitingInput | AgentState::Permission))
            if prev != Some(next) =>
        {
            Some("agent_question")
        }
        (
            Some(AgentState::AwaitingInput | AgentState::Permission),
            AgentState::Working | AgentState::Idle,
        ) => Some("human_reply"),
        _ => None,
    }
}

pub(crate) async fn dispatch_update(state: &ApiState, update: IncomingUpdate) {
    let seq = state
        .next_seq
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let (prev_state, should_proceed) = {
        let mut states = state.pane_states.lock().await;
        if let Some(existing) = states.get(&update.pane_id)
            && matches!(update.timestamp, Some(ts) if ts < existing.last_timestamp)
        {
            tracing::warn!(
                "Discarding stale state update for pane {}: incoming_ts {:?} < last_ts {}",
                update.pane_id,
                update.timestamp,
                existing.last_timestamp
            );
            (None, false)
        } else {
            let prev = states.get(&update.pane_id).map(|s| s.state.clone());
            let finished_at =
                if update.state == AgentState::Idle && prev == Some(AgentState::Working) {
                    let now_ms = crate::sys::unix_ms();
                    // Persist on the pane so a restarted daemon still knows (theme switches
                    // restart it); rehydrate_from_tmux reads it back.
                    let pane = update.pane_id.clone();
                    tokio::spawn(async move {
                        let _ = crate::tmux_cmd::async_command()
                            .args([
                                "set-option",
                                "-p",
                                "-t",
                                &pane,
                                "@ai_agent_finished_at",
                                &now_ms.to_string(),
                            ])
                            .output()
                            .await;
                    });
                    now_ms
                } else {
                    states.get(&update.pane_id).map_or(0, |s| s.finished_at)
                };
            let changed_at = match states.get(&update.pane_id) {
                Some(s) if s.state == update.state => s.changed_at,
                _ => crate::sys::unix_ms(),
            };
            states.insert(
                update.pane_id.clone(),
                PaneStateInfo {
                    last_timestamp: update.timestamp.unwrap_or(0),
                    seq_id: seq,
                    state: update.state.clone(),
                    message: update.message.clone(),
                    finished_at,
                    changed_at,
                },
            );
            (prev, true)
        }
    };

    if !should_proceed {
        return;
    }

    super::queue::on_state(state, &update.pane_id, &update.state).await;

    let settings = state.settings();
    let flow_log_hook = settings.flow_log_hook.clone();

    // Telemetry instrumentation: measure human response latency
    if let Some(event) = flow_event_for_transition(prev_state.as_ref(), &update.state) {
        log_flow_event(flow_log_hook.as_ref(), event, &update.pane_id, "");
    }

    if update.state == AgentState::Working {
        let mut stalls = state.stall_records.lock().await;
        stalls.insert(
            update.pane_id.clone(),
            StallRecord {
                last_output_snapshot: String::new(),
                last_cpu_ticks: 0,
                last_activity_instant: tokio::time::Instant::now(),
            },
        );
    } else if update.state != AgentState::Stalled {
        let mut stalls = state.stall_records.lock().await;
        stalls.remove(&update.pane_id);
    }

    if update.state == AgentState::Idle {
        let mut pending = state.pending_idles.lock().await;
        if let Some(task) = pending.remove(&update.pane_id) {
            task.abort();
        }
        let adapters = state.adapters.clone();
        let update_clone = AgentUpdate {
            pane_id: update.pane_id.clone(),
            state: update.state.clone(),
            message: update.message.clone(),
        };
        let pane_id = update.pane_id.clone();
        let pending_map = state.pending_idles.clone();
        let pane_states = state.pane_states.clone();
        let broadcast_tx = state.state_broadcast.clone();
        let delay_ms = settings.idle_debounce_ms;
        let prev_state_for_idle = prev_state.clone();

        let task = tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
            {
                let states = pane_states.lock().await;
                if states.get(&pane_id).is_some_and(|c| c.seq_id != seq) {
                    return;
                }
            }
            if prev_state_for_idle == Some(AgentState::Working) {
                log_flow_event(flow_log_hook.as_ref(), "agent_finished", &pane_id, "");
            }
            for adapter in adapters.iter() {
                if let Err(e) = adapter.update(&update_clone).await {
                    tracing::error!("Adapter error: {}", e);
                }
            }
            let _ = broadcast_tx.send(update_clone);
            let mut pending = pending_map.lock().await;
            pending.remove(&pane_id);
        });
        pending.insert(update.pane_id.clone(), task);
    } else {
        {
            let mut pending = state.pending_idles.lock().await;
            if let Some(task) = pending.remove(&update.pane_id) {
                task.abort();
            }
        }
        let agent_update = AgentUpdate {
            pane_id: update.pane_id,
            state: update.state,
            message: update.message,
        };
        for adapter in state.adapters.iter() {
            if let Err(e) = adapter.update(&agent_update).await {
                tracing::error!("Adapter error: {}", e);
            }
        }
        let _ = state.state_broadcast.send(agent_update);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::TestAdapter;
    use std::sync::Mutex as StdMutex;

    #[test]
    fn test_flow_event_for_transition() {
        assert_eq!(
            flow_event_for_transition(None, &AgentState::AwaitingInput),
            Some("agent_question")
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::Working), &AgentState::Permission),
            Some("agent_question")
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::AwaitingInput), &AgentState::AwaitingInput),
            None
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::Permission), &AgentState::Permission),
            None
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::AwaitingInput), &AgentState::Working),
            Some("human_reply")
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::Permission), &AgentState::Idle),
            Some("human_reply")
        );
        assert_eq!(
            flow_event_for_transition(Some(&AgentState::Working), &AgentState::Idle),
            None
        );
    }

    #[tokio::test]
    async fn test_flow_log_hook_gets_event_target_and_extra() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("waymux-flow-hook-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out");
        let hook = dir.join("hook.sh");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\nprintf '%s|' \"$@\" >> '{}'\n", out.display()),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let hook_str = hook.to_string_lossy().to_string();
        let state = ApiState::new(Vec::new(), 100);
        let cfg = crate::config::Config {
            idle_debounce_ms: Some(0),
            flow_log_hook: Some(hook_str),
            ..crate::config::Config::default()
        };
        state.reload(&cfg);
        assert_eq!(state.settings().idle_debounce_ms, 0);

        let update = |st: AgentState| IncomingUpdate {
            pane_id: "%3".into(),
            state: st,
            message: None,
            timestamp: None,
        };
        dispatch_update(&state, update(AgentState::Permission)).await;
        dispatch_update(&state, update(AgentState::Idle)).await;
        dispatch_update(&state, update(AgentState::Working)).await;
        dispatch_update(&state, update(AgentState::Idle)).await;

        let mut got = String::new();
        for _ in 0..100 {
            if let Ok(s) = std::fs::read_to_string(&out)
                && s.contains("agent_finished|%3||")
                && s.contains("agent_question|%3||")
                && s.contains("human_reply|%3||")
            {
                got = s;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            got.matches("agent_finished|%3||").count(),
            1,
            "only Working -> Idle logs agent_finished: {got}"
        );
    }

    #[tokio::test]
    async fn test_stale_update_discarded() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let adapter = TestAdapter {
            updates: updates.clone(),
        };
        let state = ApiState::new(vec![Box::new(adapter)], 0);

        // Newer update at t=2000
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Working,
                message: None,
                timestamp: Some(2000),
            },
        )
        .await;

        // Stale update at t=1000
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Idle,
                message: None,
                timestamp: Some(1000),
            },
        )
        .await;

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let recorded = updates.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].state, AgentState::Working);
    }

    #[tokio::test]
    async fn test_idle_superseded_by_working() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let adapter = TestAdapter {
            updates: updates.clone(),
        };
        let state = ApiState::new(vec![Box::new(adapter)], 80);

        // Idle update at t=1000 with 80ms debounce
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Idle,
                message: None,
                timestamp: Some(1000),
            },
        )
        .await;

        // Yield so an un-debounced Idle (0ms) would already have fired before Working arrives.
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
        assert!(updates.lock().unwrap().is_empty());

        // Working update at t=1050 arrives before 80ms debounce finishes
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Working,
                message: None,
                timestamp: Some(1050),
            },
        )
        .await;
        assert!(state.stall_records.lock().await.contains_key("%1"));

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        {
            let recorded = updates.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0].state, AgentState::Working);
        }

        // Stalled keeps the stall record; Idle clears it and delivers after debounce.
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Stalled,
                message: None,
                timestamp: Some(1100),
            },
        )
        .await;
        assert!(state.stall_records.lock().await.contains_key("%1"));

        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".into(),
                state: AgentState::Idle,
                message: None,
                timestamp: Some(1200),
            },
        )
        .await;
        assert!(!state.stall_records.lock().await.contains_key("%1"));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(
            updates.lock().unwrap().last().unwrap().state,
            AgentState::Idle
        );
    }

    #[tokio::test]
    async fn test_clean_stale_panes_removes_dead_pane() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let adapter = TestAdapter {
            updates: updates.clone(),
        };
        let state = ApiState::new(vec![Box::new(adapter)], 0);

        // Register a fake pane %999999 that definitely doesn't exist in tmux
        state.pane_states.lock().await.insert(
            "%999999".into(),
            PaneStateInfo {
                last_timestamp: 1000,
                seq_id: 1,
                state: AgentState::Working,
                message: None,
                finished_at: 0,
                changed_at: 0,
            },
        );

        state.clean_stale_panes().await;

        // Verify pane %999999 was removed from memory
        assert!(!state.pane_states.lock().await.contains_key("%999999"));

        // Verify closed update was sent to adapter
        let recorded = updates.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].pane_id, "%999999");
        assert_eq!(recorded[0].state, AgentState::Closed);
    }

    #[test]
    fn test_strict_agent_state_parsing() {
        assert_eq!("working".parse::<AgentState>(), Ok(AgentState::Working));
        assert_eq!("idle".parse::<AgentState>(), Ok(AgentState::Idle));
        assert_eq!(
            "permission".parse::<AgentState>(),
            Ok(AgentState::Permission)
        );
        let err = "workin".parse::<AgentState>().unwrap_err();
        assert!(err.contains("Unknown agent state 'workin'"));
        assert!(err.contains("Expected one of: working, idle"));
    }

    #[tokio::test]
    async fn test_rehydrate_from_raw_lines() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let adapter = TestAdapter {
            updates: updates.clone(),
        };
        let state = ApiState::new(vec![Box::new(adapter)], 0);

        let sample_tmux_output = "\tbusy\n  \tidle\n%4\tbusy\n%21\tidle\n%23\t\n%27\tquestion\n%30\tpermission\n%31\terror\n";
        state
            .rehydrate_from_raw_lines(sample_tmux_output, 12345)
            .await;

        let pane_states = state.pane_states.lock().await;
        assert_eq!(pane_states.len(), 5);
        assert_eq!(pane_states.get("%4").unwrap().state, AgentState::Working);
        assert_eq!(pane_states.get("%21").unwrap().state, AgentState::Idle);
        assert!(!pane_states.contains_key("%23"));
        assert_eq!(
            pane_states.get("%27").unwrap().state,
            AgentState::AwaitingInput
        );
        assert_eq!(
            pane_states.get("%30").unwrap().state,
            AgentState::Permission
        );
        assert_eq!(pane_states.get("%31").unwrap().state, AgentState::Error);
        // A restart must not look like freshly finished work.
        assert_eq!(pane_states.get("%21").unwrap().finished_at, 0);
        drop(pane_states);

        // ...but a completion time persisted on the pane survives it.
        state
            .rehydrate_from_raw_lines("%40\tidle\t1700000000000\n", 12345)
            .await;
        assert_eq!(
            state.pane_states.lock().await["%40"].finished_at,
            1_700_000_000_000
        );
    }

    #[tokio::test]
    async fn test_finished_at_set_only_by_live_working_to_idle() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let state = ApiState::new(
            vec![Box::new(TestAdapter {
                updates: updates.clone(),
            })],
            0,
        );
        let update = |st: AgentState| IncomingUpdate {
            pane_id: "%999998".into(),
            state: st,
            message: None,
            timestamp: None,
        };

        dispatch_update(&state, update(AgentState::AwaitingInput)).await;
        dispatch_update(&state, update(AgentState::Idle)).await;
        assert_eq!(state.pane_states.lock().await["%999998"].finished_at, 0);

        dispatch_update(&state, update(AgentState::Working)).await;
        dispatch_update(&state, update(AgentState::Idle)).await;
        let finished = state.pane_states.lock().await["%999998"].finished_at;
        assert!(finished > 0);

        // Later updates keep the completion time until the next finish.
        dispatch_update(&state, update(AgentState::Permission)).await;
        assert_eq!(
            state.pane_states.lock().await["%999998"].finished_at,
            finished
        );
    }

    #[test]
    fn test_from_tmux_raw() {
        assert_eq!(AgentState::from_tmux_raw("busy"), Some(AgentState::Working));
        assert_eq!(
            AgentState::from_tmux_raw("working"),
            Some(AgentState::Working)
        );
        assert_eq!(
            AgentState::from_tmux_raw("question"),
            Some(AgentState::AwaitingInput)
        );
        assert_eq!(
            AgentState::from_tmux_raw("awaiting_input"),
            Some(AgentState::AwaitingInput)
        );
        assert_eq!(
            AgentState::from_tmux_raw("permission"),
            Some(AgentState::Permission)
        );
        assert_eq!(
            AgentState::from_tmux_raw("stalled"),
            Some(AgentState::Stalled)
        );
        assert_eq!(
            AgentState::from_tmux_raw("hanging"),
            Some(AgentState::Stalled)
        );
        assert_eq!(AgentState::from_tmux_raw("error"), Some(AgentState::Error));
        assert_eq!(AgentState::from_tmux_raw("idle"), Some(AgentState::Idle));
        assert_eq!(AgentState::from_tmux_raw(""), None);
        assert_eq!(AgentState::from_tmux_raw("   "), None);
        assert_eq!(AgentState::from_tmux_raw("other_junk"), None);
    }

    mod properties {
        use super::*;
        use proptest::prelude::*;

        const STATES: [AgentState; 7] = [
            AgentState::Idle,
            AgentState::Working,
            AgentState::AwaitingInput,
            AgentState::Permission,
            AgentState::Stalled,
            AgentState::Error,
            AgentState::Closed,
        ];

        #[test]
        fn state_names_round_trip() {
            for state in STATES {
                assert_eq!(state.as_str().parse::<AgentState>(), Ok(state.clone()));
                if state != AgentState::Closed {
                    assert_eq!(AgentState::from_tmux_raw(state.as_str()), Some(state));
                }
            }
        }

        proptest! {
            /// `@ai_agent_state_raw` can be set by anything; rehydration takes only valid
            /// states and never panics.
            #[test]
            fn rehydration_accepts_any_listing(lines in "\\PC{0,200}", ts in any::<u64>()) {
                let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
                rt.block_on(async {
                    let state = ApiState::new(Vec::new(), 0);
                    state.rehydrate_from_raw_lines(&lines, ts).await;
                    for (pane, info) in state.pane_states.lock().await.iter() {
                        assert_ne!(pane, "");
                        assert!(STATES.contains(&info.state));
                    }
                });
            }
        }
    }
}
