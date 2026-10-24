//! Output sinks for agent state: tmux window/pane options, the Waybar status file and sounds.

mod sound;
mod tmux;
mod waybar;

pub use sound::SoundAdapter;
pub use tmux::TmuxAdapter;
pub use waybar::WaybarAdapter;

use crate::config::ThemeConfig;
use crate::status_palette::{self, Mode};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Working,
    AwaitingInput,
    Permission,
    Stalled,
    Error,
    Closed,
}

impl AgentState {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::AwaitingInput => "awaiting_input",
            AgentState::Permission => "permission",
            AgentState::Stalled => "stalled",
            AgentState::Error => "error",
            AgentState::Closed => "closed",
        }
    }

    pub fn from_tmux_raw(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "busy" | "working" => Some(AgentState::Working),
            "question" | "awaiting_input" => Some(AgentState::AwaitingInput),
            "permission" => Some(AgentState::Permission),
            "stalled" | "hanging" => Some(AgentState::Stalled),
            "error" => Some(AgentState::Error),
            "idle" => Some(AgentState::Idle),
            _ => None,
        }
    }
}

impl std::str::FromStr for AgentState {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "idle" => Ok(AgentState::Idle),
            "working" | "busy" => Ok(AgentState::Working),
            "awaiting_input" | "question" => Ok(AgentState::AwaitingInput),
            "permission" => Ok(AgentState::Permission),
            "stalled" | "hanging" => Ok(AgentState::Stalled),
            "error" => Ok(AgentState::Error),
            "closed" => Ok(AgentState::Closed),
            invalid => Err(format!(
                "Invalid params: Unknown agent state '{}'. Expected one of: working, idle, awaiting_input, permission, stalled, error, closed.",
                invalid
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUpdate {
    pub pane_id: String,
    pub state: AgentState,
    pub message: Option<String>,
}

#[async_trait]
#[allow(clippy::double_must_use)]
pub trait OutputAdapter: Send + Sync {
    async fn update(&self, update: &AgentUpdate) -> anyhow::Result<()>;
    async fn rehydrate(&self, _update: &AgentUpdate) -> anyhow::Result<()> {
        Ok(())
    }
    /// Takes the theme, spinner or sound settings of a reloaded config (SIGHUP).
    fn reload(&self, _config: &crate::config::Config) {}
}

/// Read a reloadable setting. A writer that panicked left a complete value behind (every
/// write replaces the whole struct), so a poisoned lock is still safe to read.
pub(crate) fn read<T>(lock: &std::sync::RwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn write<T>(lock: &std::sync::RwLock<T>, value: T) {
    *lock
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = value;
}

fn urgency_score(state: &AgentState) -> u8 {
    match state {
        AgentState::Permission => 6,
        AgentState::AwaitingInput => 5,
        AgentState::Stalled => 4,
        AgentState::Error => 3,
        AgentState::Working => 2,
        AgentState::Idle => 1,
        AgentState::Closed => 0,
    }
}

/// Icon from `[theme.states.<key>]` (else the registry's built-in) and the semantic state color.
fn state_icon_color(theme: Option<&ThemeConfig>, key: &str, mode: Mode) -> (String, String) {
    let icon = theme
        .and_then(|t| t.states.get(key))
        .map(|s| s.icon.as_str())
        .filter(|i| !i.is_empty())
        .or_else(|| crate::icons::default_state(key))
        .unwrap_or("?")
        .to_string();
    (icon, status_palette::state_color(theme, key, mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AgentStateTheme, ThemeConfig};
    use std::collections::HashMap;

    #[test]
    fn test_state_icon_color_falls_back_to_semantic_palette() {
        let mut states = HashMap::new();
        states.insert(
            "permission".to_string(),
            AgentStateTheme {
                icon: "󱅭".to_string(),
                color: String::new(),
            },
        );
        let theme = ThemeConfig {
            active_spinner: "dot".to_string(),
            states,
        };
        // Icon from config, color from the built-in palette when the config leaves it empty.
        assert_eq!(
            state_icon_color(Some(&theme), "permission", Mode::Dark),
            ("󱅭".to_string(), "#ff9e57".to_string())
        );
        // Missing state: built-in registry icon, semantic color.
        assert_eq!(
            state_icon_color(Some(&theme), "stalled", Mode::Light),
            ("󱥁".to_string(), "#a21caf".to_string())
        );
    }

    #[test]
    fn test_stalled_agent_state_parsing() {
        assert_eq!("stalled".parse::<AgentState>(), Ok(AgentState::Stalled));
        assert_eq!("hanging".parse::<AgentState>(), Ok(AgentState::Stalled));
        assert_eq!(
            AgentState::from_tmux_raw("stalled"),
            Some(AgentState::Stalled)
        );
        assert_eq!(
            AgentState::from_tmux_raw("hanging"),
            Some(AgentState::Stalled)
        );
        assert_eq!(AgentState::Stalled.as_str(), "stalled");
    }
}
