//! Event sounds: completion chimes deferred while you type, blocking alerts at once.

use super::{AgentState, AgentUpdate, OutputAdapter};
use crate::config::SoundConfig;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::Command;
use tokio::sync::Mutex;

/// A typing pause this long counts as a breakpoint for completion chimes: interruptions
/// delivered at breakpoints cost less than immediate ones (Iqbal & Bailey, CHI 2008).
const BREAKPOINT_IDLE_SECS: u64 = 2;
/// Completion chimes never wait longer than this for a pause.
const BREAKPOINT_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

async fn tmux_output(args: &[&str]) -> String {
    match crate::tmux_cmd::async_command().args(args).output().await {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

/// `pane_info` is `<pane_active><window_active>\t<session>` for the pane; `clients` has one
/// `<session>\t<flags>` line per client. True when the pane is what a focused client shows.
fn in_focused_view(pane_info: &str, clients: &str) -> bool {
    let Some((active, session)) = pane_info.trim_end().split_once('\t') else {
        return false;
    };
    active == "11"
        && clients.lines().any(|line| {
            line.split_once('\t')
                .is_some_and(|(s, flags)| s == session && flags.split(',').any(|f| f == "focused"))
        })
}

/// True when no client has had input for `BREAKPOINT_IDLE_SECS` (or nobody is attached).
/// `client_activity` has one epoch-seconds `#{client_activity}` per line.
fn at_typing_pause(client_activity: &str, now_secs: u64) -> bool {
    match client_activity
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .max()
    {
        Some(last_input) => now_secs.saturating_sub(last_input) >= BREAKPOINT_IDLE_SECS,
        None => true,
    }
}

async fn pane_in_focused_view(pane_id: &str) -> bool {
    let pane_info = tmux_output(&[
        "display-message",
        "-p",
        "-t",
        pane_id,
        "#{pane_active}#{window_active}\t#{session_name}",
    ])
    .await;
    let clients = tmux_output(&["list-clients", "-F", "#{client_session}\t#{client_flags}"]).await;
    in_focused_view(&pane_info, &clients)
}

async fn wait_for_typing_pause() {
    let start = tokio::time::Instant::now();
    loop {
        let activity = tmux_output(&["list-clients", "-F", "#{client_activity}"]).await;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if at_typing_pause(&activity, now) || start.elapsed() >= BREAKPOINT_MAX_WAIT {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

async fn play_file(event_type: &'static str, player: String, sound_path: String) {
    tracing::info!(
        "SoundAdapter: Playing {} sound from '{}' via {}",
        event_type,
        sound_path,
        player
    );
    let result = Command::new(&player).arg(&sound_path).output().await;

    if let Err(e) = result {
        tracing::warn!("SoundAdapter: Failed to execute player '{}': {}", player, e);
        if player == "pw-play" {
            let _ = Command::new("paplay").arg(&sound_path).output().await;
        }
    }
}

pub struct SoundAdapter {
    config: Option<SoundConfig>,
    pane_states: Arc<Mutex<HashMap<String, AgentState>>>,
    /// Set while a deferred completion chime waits for a typing pause.
    response_pending: Arc<AtomicBool>,
}

impl SoundAdapter {
    pub fn new(config: Option<SoundConfig>) -> Self {
        Self {
            config,
            pane_states: Arc::new(Mutex::new(HashMap::new())),
            response_pending: Arc::new(AtomicBool::new(false)),
        }
    }

    fn expand_home(path: &str) -> String {
        if path.starts_with("~/")
            && let Ok(home) = std::env::var("HOME")
        {
            return format!("{}{}", home, &path[1..]);
        }
        path.to_string()
    }

    /// Per-user config directories searched for sound overrides, in priority order.
    /// `acpd` is the legacy (pre-waymux) directory, kept as a read-only fallback.
    const SOUND_CONFIG_DIRS: [&'static str; 2] = ["waymux", "acpd"];

    /// Pure helper: ordered search paths for `(mute markers, state pointer files, direct audio files)`.
    /// Omarchy-managed files win, then `~/.config/waymux/sounds`, then legacy `~/.config/acpd/sounds`.
    fn sound_search_paths(home: &str, event_type: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
        let mut mute_markers = vec![format!("{}/.config/omarchy/sounds/muted", home)];
        let mut state_pointers = vec![format!(
            "{}/.config/omarchy/sounds/ai-{}.sound",
            home, event_type
        )];
        let mut direct_files = vec![
            format!("{}/.config/omarchy/sounds/ai-{}.wav", home, event_type),
            format!("{}/.config/omarchy/sounds/ai-{}.oga", home, event_type),
        ];

        for dir in Self::SOUND_CONFIG_DIRS {
            mute_markers.push(format!("{}/.config/{}/sounds/muted", home, dir));
            state_pointers.push(format!(
                "{}/.config/{}/sounds/{}.sound",
                home, dir, event_type
            ));
            direct_files.push(format!(
                "{}/.config/{}/sounds/{}.wav",
                home, dir, event_type
            ));
        }

        if event_type == "response" {
            state_pointers.push(format!("{}/.config/omarchy/sounds/ai-response.sound", home));
            for dir in Self::SOUND_CONFIG_DIRS {
                state_pointers.push(format!("{}/.config/{}/sounds/response.sound", home, dir));
            }
            direct_files.push(format!("{}/.config/omarchy/sounds/ai-response.wav", home));
        }

        (mute_markers, state_pointers, direct_files)
    }

    fn resolve_sound_path(&self, event_type: &str) -> Option<String> {
        let home = std::env::var("HOME").ok()?;
        let (mute_markers, state_pointers, direct_files) =
            Self::sound_search_paths(&home, event_type);

        // Check explicit mute marker files
        for marker in &mute_markers {
            if std::path::Path::new(marker).exists() {
                return None;
            }
        }

        // 1. Check runtime state pointer files first (allows instant hot-switching without restart)
        for pointer in &state_pointers {
            if let Ok(content) = std::fs::read_to_string(pointer) {
                let trimmed = content.trim();
                // Explicitly muted or disabled
                if trimmed.eq_ignore_ascii_case("none")
                    || trimmed.eq_ignore_ascii_case("off")
                    || trimmed.eq_ignore_ascii_case("disabled")
                    || trimmed.eq_ignore_ascii_case("muted")
                {
                    return None;
                }
                if !trimmed.is_empty() {
                    let expanded = Self::expand_home(trimmed);
                    if std::path::Path::new(&expanded).is_file() {
                        return Some(expanded);
                    }
                }
            }
        }

        // 2. Check direct symlinks/files in user config
        for path in &direct_files {
            if std::path::Path::new(path).is_file() {
                return Some(path.clone());
            }
        }

        // 3. Check explicit config from toml
        if let Some(sound_cfg) = &self.config {
            let custom_path = match event_type {
                "response" => sound_cfg.response.as_deref(),
                "question" => sound_cfg
                    .question
                    .as_deref()
                    .or(sound_cfg.response.as_deref()),
                "permission" => sound_cfg
                    .permission
                    .as_deref()
                    .or(sound_cfg.response.as_deref()),
                "error" => sound_cfg.error.as_deref(),
                _ => None,
            };

            if let Some(p) = custom_path {
                let expanded = Self::expand_home(p);
                if std::path::Path::new(&expanded).is_file() {
                    return Some(expanded);
                }
            }
        }

        // 4. Fallback defaults
        let default_sound = match event_type {
            "question" => format!("{}/.local/share/sounds/ai/02-gentle-ping.wav", home),
            "permission" => format!("{}/.local/share/sounds/ai/06-cyber-pulse.wav", home),
            "error" => format!("{}/.local/share/sounds/ai/10-arcade-blip.wav", home),
            _ => format!("{}/.local/share/sounds/ai/01-crystal-chime.wav", home),
        };

        let fallbacks = [
            default_sound,
            format!("{}/.local/share/sounds/ai/01-crystal-chime.wav", home),
            "/usr/share/sounds/freedesktop/stereo/complete.oga".to_string(),
            "/usr/share/sounds/freedesktop/stereo/message.oga".to_string(),
            "/usr/share/sounds/freedesktop/stereo/bell.oga".to_string(),
            "/System/Library/Sounds/Glass.aiff".to_string(),
        ];

        for fb in &fallbacks {
            if std::path::Path::new(fb).is_file() {
                return Some(fb.clone());
            }
        }

        None
    }

    /// Player and sound file for an event, or None when sound is off or muted.
    fn sound_for(&self, event_type: &'static str) -> Option<(String, String)> {
        if let Some(cfg) = &self.config
            && !cfg.enabled
        {
            return None;
        }

        let Some(sound_path) = self.resolve_sound_path(event_type) else {
            tracing::warn!(
                "SoundAdapter: No sound file resolved for event '{}'",
                event_type
            );
            return None;
        };

        let player = self
            .config
            .as_ref()
            .and_then(|c| c.player.clone())
            .unwrap_or_else(crate::sys::default_sound_player);
        Some((player, sound_path))
    }

    fn play(&self, event_type: &'static str) {
        if let Some((player, sound_path)) = self.sound_for(event_type) {
            tokio::spawn(play_file(event_type, player, sound_path));
        }
    }

    /// Completion chimes are the most frequent and least urgent cue: skip them for the pane a
    /// focused client is showing, otherwise wait for a typing pause. Agents finishing while a
    /// chime waits share it.
    async fn play_response(&self, pane_id: &str) {
        if pane_in_focused_view(pane_id).await {
            tracing::info!(
                "SoundAdapter: pane {} is in view; skipping completion chime",
                pane_id
            );
            return;
        }
        let Some((player, sound_path)) = self.sound_for("response") else {
            return;
        };
        if self.response_pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let pending = Arc::clone(&self.response_pending);
        tokio::spawn(async move {
            wait_for_typing_pause().await;
            pending.store(false, Ordering::SeqCst);
            play_file("response", player, sound_path).await;
        });
    }
}

#[async_trait]
impl OutputAdapter for SoundAdapter {
    async fn update(&self, update: &AgentUpdate) -> anyhow::Result<()> {
        let prev_state = {
            let mut states = self.pane_states.lock().await;
            let prev = states.get(&update.pane_id).cloned();
            if update.state == AgentState::Closed {
                states.remove(&update.pane_id);
            } else {
                states.insert(update.pane_id.clone(), update.state.clone());
            }
            prev
        };

        match (&prev_state, &update.state) {
            // When agent transitions from Working (or active state) to Idle -> AI response finished!
            (Some(AgentState::Working), AgentState::Idle)
            | (Some(AgentState::AwaitingInput), AgentState::Idle)
            | (Some(AgentState::Permission), AgentState::Idle)
            | (Some(AgentState::Stalled), AgentState::Idle) => {
                self.play_response(&update.pane_id).await;
            }
            // When agent transitions to AwaitingInput -> AI asking question / input
            (_, AgentState::AwaitingInput) if prev_state != Some(AgentState::AwaitingInput) => {
                self.play("question");
            }
            // When agent transitions to Permission -> AI asking permission
            (_, AgentState::Permission) if prev_state != Some(AgentState::Permission) => {
                self.play("permission");
            }
            // When agent transitions to Stalled
            (_, AgentState::Stalled) if prev_state != Some(AgentState::Stalled) => {
                self.play("error");
            }
            // When agent transitions to Error
            (_, AgentState::Error) if prev_state != Some(AgentState::Error) => {
                self.play("error");
            }
            _ => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_in_focused_view_needs_visible_pane_and_focused_client() {
        let clients = "nerdsight/main\tattached,focused,UTF-8\n_dotfiles/main\tattached,UTF-8\n";
        assert!(in_focused_view("11\tnerdsight/main\n", clients));
        // Visible pane, but its terminal is not focused.
        assert!(!in_focused_view("11\t_dotfiles/main\n", clients));
        // Focused session, but another pane or window is showing.
        assert!(!in_focused_view("01\tnerdsight/main\n", clients));
        assert!(!in_focused_view("10\tnerdsight/main\n", clients));
        // Nobody attached, or the pane is gone.
        assert!(!in_focused_view("11\tnerdsight/main\n", ""));
        assert!(!in_focused_view("", clients));
    }

    #[test]
    fn test_at_typing_pause_uses_most_recent_input() {
        assert!(at_typing_pause("1000\n", 1002));
        assert!(!at_typing_pause("1000\n", 1001));
        // The most recent client input decides.
        assert!(!at_typing_pause("900\n1000\n", 1001));
        // No attached client: nobody is typing.
        assert!(at_typing_pause("", 1001));
    }

    #[tokio::test]
    async fn test_sound_adapter_updates() {
        let sound_cfg = SoundConfig {
            enabled: false, // Disabled in test so no actual audio process runs
            player: Some("true".to_string()),
            response: Some("/dev/null".to_string()),
            question: Some("/dev/null".to_string()),
            permission: Some("/dev/null".to_string()),
            error: Some("/dev/null".to_string()),
        };
        let adapter = SoundAdapter::new(Some(sound_cfg));

        let update_working = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Working,
            message: None,
        };
        assert!(adapter.update(&update_working).await.is_ok());

        // Working -> Idle (should trigger response sound logic)
        let update_idle = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Idle,
            message: None,
        };
        assert!(adapter.update(&update_idle).await.is_ok());

        // Question
        let update_question = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::AwaitingInput,
            message: None,
        };
        assert!(adapter.update(&update_question).await.is_ok());

        // Closed
        let update_closed = AgentUpdate {
            pane_id: "%1".to_string(),
            state: AgentState::Closed,
            message: None,
        };
        assert!(adapter.update(&update_closed).await.is_ok());
    }

    #[test]
    fn test_resolve_sound_path_isolation() {
        let temp_dir = std::env::temp_dir().join(format!("acpd-sound-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let resp_file = temp_dir.join("resp.wav");
        let quest_file = temp_dir.join("quest.wav");
        std::fs::write(&resp_file, b"resp").unwrap();
        std::fs::write(&quest_file, b"quest").unwrap();

        let old_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &temp_dir);
        }

        let sound_cfg = SoundConfig {
            enabled: true,
            player: Some("pw-play".to_string()),
            response: Some(resp_file.to_string_lossy().to_string()),
            question: Some(quest_file.to_string_lossy().to_string()),
            permission: None,
            error: None,
        };
        let adapter = SoundAdapter::new(Some(sound_cfg));

        // When question is configured, it should resolve to quest_file, NOT fallback to response (resp_file)
        let resolved_quest = adapter.resolve_sound_path("question");
        let resolved_resp = adapter.resolve_sound_path("response");

        if let Some(h) = old_home {
            unsafe {
                std::env::set_var("HOME", h);
            }
        }

        assert_eq!(
            resolved_quest,
            Some(quest_file.to_string_lossy().to_string())
        );
        assert_eq!(resolved_resp, Some(resp_file.to_string_lossy().to_string()));

        let _ = std::fs::remove_dir_all(temp_dir);
    }

    #[test]
    fn test_sound_search_paths_prefer_waymux_over_legacy_acpd() {
        let (mute, pointers, direct) = SoundAdapter::sound_search_paths("/h", "question");

        let idx = |v: &[String], needle: &str| v.iter().position(|p| p == needle);

        // Mute markers: omarchy, waymux, then legacy acpd
        let m_omarchy = idx(&mute, "/h/.config/omarchy/sounds/muted").unwrap();
        let m_waymux = idx(&mute, "/h/.config/waymux/sounds/muted").unwrap();
        let m_acpd = idx(&mute, "/h/.config/acpd/sounds/muted").unwrap();
        assert!(m_omarchy < m_waymux && m_waymux < m_acpd);

        // State pointers: waymux must outrank legacy acpd
        let p_waymux = idx(&pointers, "/h/.config/waymux/sounds/question.sound").unwrap();
        let p_acpd = idx(&pointers, "/h/.config/acpd/sounds/question.sound").unwrap();
        assert!(p_waymux < p_acpd);

        // Direct files: waymux must outrank legacy acpd
        let d_waymux = idx(&direct, "/h/.config/waymux/sounds/question.wav").unwrap();
        let d_acpd = idx(&direct, "/h/.config/acpd/sounds/question.wav").unwrap();
        assert!(d_waymux < d_acpd);

        // The `response` event additionally exposes the shared response pointer in both dirs
        let (_, resp_ptrs, _) = SoundAdapter::sound_search_paths("/h", "response");
        assert!(idx(&resp_ptrs, "/h/.config/waymux/sounds/response.sound").is_some());
        assert!(idx(&resp_ptrs, "/h/.config/acpd/sounds/response.sound").is_some());
    }
}
