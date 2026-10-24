//! tmux window/pane options, multi-pane urgency aggregation and the working spinner.

use super::{AgentState, AgentUpdate, OutputAdapter, state_icon_color, urgency_score};
use crate::config::{Spinner, ThemeConfig};
use crate::status_palette::{self, Mode};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

async fn set_tmux_window_state(pane_id: &str, icon: &str, raw: &str, color: &str) {
    let out = crate::tmux_cmd::async_command()
        .args([
            "set-option",
            "-w",
            "-t",
            pane_id,
            "@ai_agent_state",
            icon,
            ";",
            "set-option",
            "-w",
            "-t",
            pane_id,
            "@ai_agent_state_raw",
            raw,
            ";",
            "set-option",
            "-w",
            "-t",
            pane_id,
            "@ai_agent_state_color",
            color,
            ";",
            "refresh-client",
            "-S",
        ])
        .output()
        .await;
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::warn!(
            "Tmux set_tmux_window_state failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

async fn unset_tmux_window_state(pane_id: &str) {
    let out = crate::tmux_cmd::async_command()
        .args([
            "set-option",
            "-w",
            "-u",
            "-t",
            pane_id,
            "@ai_agent_state",
            ";",
            "set-option",
            "-w",
            "-u",
            "-t",
            pane_id,
            "@ai_agent_state_raw",
            ";",
            "set-option",
            "-w",
            "-u",
            "-t",
            pane_id,
            "@ai_agent_state_color",
            ";",
            "refresh-client",
            "-S",
        ])
        .output()
        .await;
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::warn!(
            "Tmux unset_tmux_window_state failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

async fn set_tmux_pane_state(pane_id: &str, raw: &str) {
    let out = crate::tmux_cmd::async_command()
        .args([
            "set-option",
            "-p",
            "-t",
            pane_id,
            "@ai_agent_state_raw",
            raw,
        ])
        .output()
        .await;
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::debug!(
            "Tmux set_tmux_pane_state failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

async fn unset_tmux_pane_state(pane_id: &str) {
    let out = crate::tmux_cmd::async_command()
        .args([
            "set-option",
            "-p",
            "-u",
            "-t",
            pane_id,
            "@ai_agent_state_raw",
        ])
        .output()
        .await;
    if let Ok(o) = out
        && !o.status.success()
    {
        tracing::debug!(
            "Tmux unset_tmux_pane_state failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

pub struct TmuxAdapter {
    spinners: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    pane_states: Arc<Mutex<HashMap<String, AgentState>>>,
    pane_windows: Arc<Mutex<HashMap<String, String>>>,
    window_panes: Arc<Mutex<HashMap<String, HashMap<String, AgentState>>>>,
    display: std::sync::RwLock<Display>,
}

/// The settings a config reload replaces.
struct Display {
    spinner_frames: Vec<String>,
    spinner_interval: u64,
    theme: Option<ThemeConfig>,
    mode: Mode,
}

impl Display {
    fn new(theme: Option<ThemeConfig>, active_spinner: Option<Spinner>) -> Self {
        let (spinner_frames, spinner_interval) = match active_spinner {
            Some(s) if !s.frames.is_empty() => (s.frames, s.interval),
            _ => (vec![], 0),
        };
        Self {
            spinner_frames,
            spinner_interval,
            theme,
            mode: status_palette::detect_mode(),
        }
    }

    fn icon_color(&self, state: &str) -> (String, String) {
        state_icon_color(self.theme.as_ref(), state, self.mode)
    }
}

impl TmuxAdapter {
    pub fn new(theme: Option<ThemeConfig>, active_spinner: Option<Spinner>) -> Self {
        Self {
            spinners: Arc::new(Mutex::new(HashMap::new())),
            pane_states: Arc::new(Mutex::new(HashMap::new())),
            pane_windows: Arc::new(Mutex::new(HashMap::new())),
            window_panes: Arc::new(Mutex::new(HashMap::new())),
            display: std::sync::RwLock::new(Display::new(theme, active_spinner)),
        }
    }

    async fn resolve_window_id(&self, pane_id: &str) -> String {
        {
            let pw = self.pane_windows.lock().await;
            if let Some(w) = pw.get(pane_id) {
                return w.clone();
            }
        }
        let out = crate::tmux_cmd::async_command()
            .args(["display-message", "-t", pane_id, "-p", "#{window_id}"])
            .output()
            .await;
        let win = match out {
            Ok(o) if o.status.success() => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if !s.is_empty() {
                    s
                } else {
                    pane_id.to_string()
                }
            }
            _ => pane_id.to_string(),
        };
        self.pane_windows
            .lock()
            .await
            .insert(pane_id.to_string(), win.clone());
        win
    }

    #[cfg(test)]
    pub async fn set_pane_window_mapping(&self, pane_id: &str, window_id: &str) {
        self.pane_windows
            .lock()
            .await
            .insert(pane_id.to_string(), window_id.to_string());
    }

    #[cfg(test)]
    pub async fn get_window_effective_state(&self, window_id: &str) -> Option<AgentState> {
        let wp = self.window_panes.lock().await;
        wp.get(window_id).and_then(|panes| {
            panes
                .values()
                .filter(|s| **s != AgentState::Closed)
                .max_by_key(|s| urgency_score(s))
                .cloned()
        })
    }

    async fn sync_window_state(&self, window_id: &str) {
        let effective = {
            let wp = self.window_panes.lock().await;
            wp.get(window_id).and_then(|panes| {
                panes
                    .values()
                    .filter(|s| **s != AgentState::Closed)
                    .max_by_key(|s| urgency_score(s))
                    .cloned()
            })
        };

        match effective {
            None => {
                self.stop_spinner(window_id).await;
                unset_tmux_window_state(window_id).await;
                self.window_panes.lock().await.remove(window_id);
            }
            Some(AgentState::Working) => {
                self.start_spinner(window_id.to_string()).await;
            }
            Some(state) => {
                self.stop_spinner(window_id).await;
                let raw = match state {
                    AgentState::Idle => "idle",
                    AgentState::AwaitingInput => "question",
                    AgentState::Permission => "permission",
                    AgentState::Stalled => "stalled",
                    AgentState::Error => "error",
                    _ => "",
                };
                let (icon, color) = if raw.is_empty() {
                    (String::new(), String::new())
                } else {
                    super::read(&self.display).icon_color(raw)
                };
                set_tmux_window_state(window_id, &icon, raw, &color).await;
            }
        }
    }

    async fn stop_spinner(&self, target_id: &str) {
        let mut spinners = self.spinners.lock().await;
        if let Some(task) = spinners.remove(target_id) {
            task.abort();
        }
    }

    async fn start_spinner(&self, target_id: String) {
        {
            let spinners = self.spinners.lock().await;
            if spinners.contains_key(&target_id) {
                return;
            }
        }

        let target_clone = target_id.clone();
        let (frames, interval, (icon, color)) = {
            let display = super::read(&self.display);
            (
                display.spinner_frames.clone(),
                display.spinner_interval,
                display.icon_color("busy"),
            )
        };

        let task = tokio::spawn(async move {
            if frames.is_empty() {
                set_tmux_window_state(&target_clone, &icon, "busy", &color).await;
                return;
            }

            if frames.len() == 1 {
                set_tmux_window_state(&target_clone, &frames[0], "busy", &color).await;
                return;
            }

            let mut i = 0;

            loop {
                let frame = &frames[i % frames.len()];

                set_tmux_window_state(&target_clone, frame, "busy", &color).await;

                i += 1;
                tokio::time::sleep(tokio::time::Duration::from_millis(interval)).await;
            }
        });

        self.spinners.lock().await.insert(target_id, task);
    }

    pub fn should_notify_clients(clients_output: &str, session: &str, window_id: &str) -> bool {
        let non_empty_lines: Vec<_> = clients_output
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();

        if non_empty_lines.is_empty() {
            // Detached: no clients attached to tmux server -> notify
            return true;
        }

        let active_on_this_window = non_empty_lines.iter().any(|line| {
            let mut iter = line.split_whitespace();
            if let (Some(c_sess), Some(c_wid)) = (iter.next(), iter.next()) {
                c_sess == session && c_wid == window_id
            } else {
                false
            }
        });

        !active_on_this_window
    }

    async fn trigger_bell(&self, pane_id: &str, action: &str, force: bool) {
        // Fetch session and window properties for the pane
        let output = crate::tmux_cmd::async_command()
            .args([
                "display-message",
                "-t",
                pane_id,
                "-p",
                "#S|#{window_id}|#I|#W",
            ])
            .output()
            .await;

        if let Ok(out) = output {
            let props = String::from_utf8_lossy(&out.stdout);
            let parts: Vec<&str> = props.trim().split('|').collect();
            if parts.len() == 4 {
                let session = parts[0];
                let window_id = parts[1];
                let window_idx = parts[2];
                let window_name = parts[3];

                // Check attached clients: if user is actively focused on this window in the session, suppress bell.
                // If the session/window is detached (no clients attached or looking elsewhere), notify must remain true!
                let clients_out = crate::tmux_cmd::async_command()
                    .args(["list-clients", "-F", "#{client_session} #{window_id}"])
                    .output()
                    .await;

                let notify = if force {
                    true
                } else if let Ok(cout) = clients_out {
                    let clients_str = String::from_utf8_lossy(&cout.stdout);
                    Self::should_notify_clients(&clients_str, session, window_id)
                } else {
                    true
                };

                if notify {
                    let _ = crate::tmux_cmd::async_command()
                        .args(["set", "-g", "@ai_agent_last_bell", pane_id])
                        .output()
                        .await;
                    let msg = format!(
                        "  #[fg=cyan]{}:{} › {} #[fg=yellow](i)#[fg=default]",
                        window_idx, window_name, action
                    );
                    let _ = crate::tmux_cmd::async_command()
                        .args(["set", "-g", "@ai_agent_bell", &msg])
                        .output()
                        .await;
                    let _ = crate::tmux_cmd::async_command()
                        .args(["refresh-client", "-S"])
                        .output()
                        .await;

                    // Clear after 7 seconds
                    tokio::spawn(async move {
                        tokio::time::sleep(tokio::time::Duration::from_secs(7)).await;
                        let _ = crate::tmux_cmd::async_command()
                            .args(["set", "-g", "@ai_agent_bell", ""])
                            .output()
                            .await;
                        let _ = crate::tmux_cmd::async_command()
                            .args(["refresh-client", "-S"])
                            .output()
                            .await;
                    });
                }
            }
        }
    }
}

#[async_trait]
impl OutputAdapter for TmuxAdapter {
    /// Running spinners keep their frames; the next spinner a window starts uses the new ones.
    fn reload(&self, config: &crate::config::Config) {
        let (_, spinner) = config.active_spinner();
        super::write(&self.display, Display::new(config.theme.clone(), spinner));
    }

    async fn update(&self, update: &AgentUpdate) -> anyhow::Result<()> {
        let window_id = self.resolve_window_id(&update.pane_id).await;

        // State deduplication per pane: ignore redundant updates for the same state on the same pane
        let is_redundant = {
            let mut states = self.pane_states.lock().await;
            if let Some(prev) = states.get(&update.pane_id)
                && prev == &update.state
            {
                true
            } else {
                if update.state == AgentState::Closed {
                    states.remove(&update.pane_id);
                } else {
                    states.insert(update.pane_id.clone(), update.state.clone());
                }
                false
            }
        };

        if is_redundant {
            return Ok(());
        }

        // Set or clear pane-specific state option in tmux (@ai_agent_state_raw)
        if update.state == AgentState::Closed {
            unset_tmux_pane_state(&update.pane_id).await;
            self.pane_windows.lock().await.remove(&update.pane_id);
        } else {
            let pane_raw = match &update.state {
                AgentState::Working => "busy",
                AgentState::Idle => "idle",
                AgentState::AwaitingInput => "question",
                AgentState::Permission => "permission",
                AgentState::Stalled => "stalled",
                AgentState::Error => "error",
                AgentState::Closed => "",
            };
            set_tmux_pane_state(&update.pane_id, pane_raw).await;
        }

        // Update window-panes mapping
        {
            let mut wp = self.window_panes.lock().await;
            let panes = wp.entry(window_id.clone()).or_default();
            if update.state == AgentState::Closed {
                panes.remove(&update.pane_id);
            } else {
                panes.insert(update.pane_id.clone(), update.state.clone());
            }
        }

        // Recalculate and sync aggregated window state
        self.sync_window_state(&window_id).await;

        // Trigger bell if this specific pane requested attention
        let bell_state = match update.state {
            AgentState::AwaitingInput => Some("question"),
            AgentState::Permission => Some("permission"),
            AgentState::Stalled => Some("stalled"),
            _ => None,
        };

        if let Some(state) = bell_state {
            let (icon, _) = super::read(&self.display).icon_color(state);
            let msg = format!("{icon} {state}");
            self.trigger_bell(&update.pane_id, &msg, true).await;
        }

        tracing::info!(
            "TmuxAdapter: Updated pane {} (win {}) to {:?}",
            update.pane_id,
            window_id,
            update.state
        );
        Ok(())
    }

    async fn rehydrate(&self, update: &AgentUpdate) -> anyhow::Result<()> {
        let window_id = self.resolve_window_id(&update.pane_id).await;
        {
            let mut states = self.pane_states.lock().await;
            states.insert(update.pane_id.clone(), update.state.clone());
        }
        {
            let mut wp = self.window_panes.lock().await;
            let panes = wp.entry(window_id.clone()).or_default();
            panes.insert(update.pane_id.clone(), update.state.clone());
        }
        self.sync_window_state(&window_id).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentStateTheme, ThemeConfig};
    use std::collections::HashMap;

    #[test]
    fn test_reload_replaces_theme_and_spinner() {
        let adapter = TmuxAdapter::new(None, None);
        let config: crate::config::Config = toml::from_str(
            r##"
            [theme]
            active_spinner = "line"
            [theme.states.idle]
            icon = "I"
            color = "#112233"
            [spinners]
            line = { interval = 100, frames = ["|", "/"] }
            "##,
        )
        .unwrap();
        adapter.reload(&config);
        let display = super::super::read(&adapter.display);
        assert_eq!(display.spinner_frames, ["|", "/"]);
        assert_eq!(display.spinner_interval, 100);
        assert_eq!(display.icon_color("idle").0, "I");
    }

    #[tokio::test]
    async fn test_tmux_adapter_updates() {
        let mut states = HashMap::new();
        states.insert(
            "idle".to_string(),
            AgentStateTheme {
                icon: "󱥂".to_string(),
                color: "#94e2d5".to_string(),
            },
        );
        states.insert(
            "busy".to_string(),
            AgentStateTheme {
                icon: "󰝲".to_string(),
                color: "#f9e2af".to_string(),
            },
        );
        let theme = ThemeConfig {
            active_spinner: "dot".to_string(),
            states,
        };
        let adapter = TmuxAdapter::new(Some(theme), None);

        // Test non-closed state update
        let update_idle = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Idle,
            message: None,
        };
        assert!(adapter.update(&update_idle).await.is_ok());

        // Test closed state update
        let update_closed = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Closed,
            message: None,
        };
        assert!(adapter.update(&update_closed).await.is_ok());

        // Test working (spinner) state update
        let update_working = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Working,
            message: None,
        };
        assert!(adapter.update(&update_working).await.is_ok());
        adapter.stop_spinner("%1").await;
    }

    #[tokio::test]
    async fn test_tmux_adapter_multi_pane_urgency_aggregation() {
        let adapter = TmuxAdapter::new(None, None);

        // Manually bind %1 and %2 to the same window @10
        adapter.set_pane_window_mapping("%1", "@10").await;
        adapter.set_pane_window_mapping("%2", "@10").await;

        // 1. Pane 1 asks permission (urgency = 6)
        adapter
            .update(&AgentUpdate {
                pane_id: "%1".to_string(),
                state: AgentState::Permission,
                message: None,
            })
            .await
            .unwrap();

        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Permission),
            "Window @10 must have state Permission"
        );

        // 2. Pane 2 finishes background task and enters Idle (urgency = 1)
        adapter
            .update(&AgentUpdate {
                pane_id: "%2".to_string(),
                state: AgentState::Idle,
                message: None,
            })
            .await
            .unwrap();

        // Pane 2 entering Idle must NOT downgrade window @10 from Permission to Idle!
        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Permission),
            "Window @10 must maintain Permission even when Pane 2 goes Idle"
        );

        // 3. Pane 2 starts working (urgency = 2)
        adapter
            .update(&AgentUpdate {
                pane_id: "%2".to_string(),
                state: AgentState::Working,
                message: None,
            })
            .await
            .unwrap();

        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Permission),
            "Window @10 must still prioritize Permission over Working"
        );

        // 4. User replies to Pane 1, resolving Permission -> Idle
        adapter
            .update(&AgentUpdate {
                pane_id: "%1".to_string(),
                state: AgentState::Idle,
                message: None,
            })
            .await
            .unwrap();

        // Now Pane 2 is Working and Pane 1 is Idle. Max urgency is Working!
        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Working),
            "Window @10 must switch to Working since Pane 2 is still working"
        );

        // 5. Pane 2 also becomes Idle
        adapter
            .update(&AgentUpdate {
                pane_id: "%2".to_string(),
                state: AgentState::Idle,
                message: None,
            })
            .await
            .unwrap();

        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Idle),
            "Window @10 must become Idle when all panes are Idle"
        );

        // 6. Pane 1 closes
        adapter
            .update(&AgentUpdate {
                pane_id: "%1".to_string(),
                state: AgentState::Closed,
                message: None,
            })
            .await
            .unwrap();

        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            Some(AgentState::Idle),
            "Window @10 must remain Idle because Pane 2 is still open"
        );

        // 7. Pane 2 closes
        adapter
            .update(&AgentUpdate {
                pane_id: "%2".to_string(),
                state: AgentState::Closed,
                message: None,
            })
            .await
            .unwrap();

        assert_eq!(
            adapter.get_window_effective_state("@10").await,
            None,
            "Window @10 should have no effective state when all panes close"
        );
    }

    #[test]
    fn test_should_notify_clients_detached_and_attached() {
        // 1. Completely detached (empty clients list) -> must notify
        assert!(
            TmuxAdapter::should_notify_clients("", "work", "@1"),
            "Empty clients output (detached) must notify"
        );
        assert!(
            TmuxAdapter::should_notify_clients("   \n\n  ", "work", "@1"),
            "Whitespace clients output (detached) must notify"
        );

        // 2. Active client is focused on the exact same session and window -> suppress bell (false)
        let attached_same_window = "work @1\n";
        assert!(
            !TmuxAdapter::should_notify_clients(attached_same_window, "work", "@1"),
            "Client focused on same window must suppress notification"
        );

        // 3. Active client is focused on a different window in the same session -> must notify
        let attached_other_window = "work @2\n";
        assert!(
            TmuxAdapter::should_notify_clients(attached_other_window, "work", "@1"),
            "Client focused on different window must notify"
        );

        // 4. Active client is focused on another session -> must notify
        let attached_other_session = "personal @1\n";
        assert!(
            TmuxAdapter::should_notify_clients(attached_other_session, "work", "@1"),
            "Client focused on different session must notify"
        );
    }
}
