//! Waybar custom-module state file.

use super::{AgentState, AgentUpdate, OutputAdapter, state_icon_color, urgency_score};
use crate::config::ThemeConfig;
use crate::status_palette::{self, Mode};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::Mutex;

fn get_waybar_state_path() -> String {
    let xdg_runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    format!("{}/acpd-waybar-state.json", xdg_runtime)
}

pub struct WaybarAdapter {
    /// Theme and light/dark mode, replaced on a config reload.
    display: std::sync::RwLock<(Option<ThemeConfig>, Mode)>,
    pane_updates: Arc<Mutex<HashMap<String, AgentUpdate>>>,
}

impl WaybarAdapter {
    pub fn new(theme: Option<ThemeConfig>) -> Self {
        Self {
            display: std::sync::RwLock::new((theme, status_palette::detect_mode())),
            pane_updates: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl OutputAdapter for WaybarAdapter {
    fn reload(&self, config: &crate::config::Config) {
        super::write(
            &self.display,
            (config.theme.clone(), status_palette::detect_mode()),
        );
    }

    async fn update(&self, update: &AgentUpdate) -> anyhow::Result<()> {
        let active_update = {
            let mut map = self.pane_updates.lock().await;
            if update.state == AgentState::Closed {
                map.remove(&update.pane_id);
            } else {
                map.insert(update.pane_id.clone(), update.clone());
            }

            map.values()
                .filter(|u| u.state != AgentState::Closed && u.state != AgentState::Idle)
                .max_by_key(|u| urgency_score(&u.state))
                .cloned()
        };

        if let Some(active) = active_update {
            let (raw_state, tooltip) = match active.state {
                AgentState::Idle => ("idle", "Agent Idle"),
                AgentState::Working => ("busy", "Agent Working"),
                AgentState::AwaitingInput => ("question", "Awaiting Input"),
                AgentState::Permission => ("permission", "Permission Required"),
                AgentState::Stalled => ("stalled", "Agent Stalled / Hanging"),
                AgentState::Error => ("error", "Agent Error"),
                AgentState::Closed => ("idle", "Agent Closed"),
            };

            let (icon, color) = {
                let (theme, mode) = &*super::read(&self.display);
                state_icon_color(theme.as_ref(), raw_state, *mode)
            };

            let json_payload = serde_json::json!({
                "text": icon,
                "tooltip": tooltip,
                "class": raw_state,
                "color": color
            });

            let content = json_payload.to_string();

            let path = get_waybar_state_path();
            if let Err(e) = tokio::fs::write(&path, content).await {
                tracing::error!("Failed to write waybar state file: {}", e);
            }
            tracing::info!("WaybarAdapter updated to: {}", raw_state);
        } else {
            let path = get_waybar_state_path();
            let _ = tokio::fs::remove_file(&path).await;
            tracing::info!("WaybarAdapter: All agents idle/closed, cleared waybar state");
        }

        // Pkill waybar to trigger custom module update
        if let Err(e) = Command::new("pkill")
            .args(["-RTMIN+13", "waybar"])
            .output()
            .await
        {
            tracing::error!("Failed to send signal to waybar: {}", e);
        }

        Ok(())
    }

    async fn rehydrate(&self, update: &AgentUpdate) -> anyhow::Result<()> {
        self.update(update).await
    }
}
