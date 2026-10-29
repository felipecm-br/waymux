//! The phone surface: a page at `/m` that lists the agents and answers held prompts.
//!
//! It is meant for a phone on the same tailnet (`tailscale serve` in front of the daemon). The page
//! itself carries no data and needs no token. It reads the token from its URL fragment, which a
//! browser never sends to a server, and calls two methods with it: `phone/snapshot` and
//! `approval/answer`. A phone token can do nothing else: no typing into panes, no queue, no file
//! paths from the client (the checklist is TUIOS's three advisories of 26 Oct 2026).
//!
//! One token at a time, minted with `waymux phone-token` (daemon token only) and kept in
//! `$XDG_STATE_HOME/waymux/phone-token` (0600), so pairing survives a daemon restart.
//! `--rotate` replaces it and unpairs every device that held the old one.

use super::ApiState;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::RwLock;

/// The methods a phone token may call.
pub const METHODS: [&str; 2] = ["phone/snapshot", "approval/answer"];

pub const PAGE: &str = include_str!("phone.html");

/// `Default` keeps the token in memory only (tests); the daemon uses [`PhoneToken::load`].
#[derive(Debug, Default)]
pub struct PhoneToken {
    token: RwLock<Option<String>>,
    path: Option<PathBuf>,
}

fn token_path() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .map(|s| s.join("waymux").join("phone-token"))
}

impl PhoneToken {
    /// The token saved by an earlier daemon, if any.
    pub fn load() -> Self {
        let path = token_path();
        let saved = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());
        Self {
            token: RwLock::new(saved),
            path,
        }
    }

    /// The current token, or a new one when there is none or `rotate` asks for it.
    pub fn issue(&self, rotate: bool) -> Option<String> {
        let mut token = self
            .token
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(current) = token.as_ref().filter(|_| !rotate) {
            return Some(current.clone());
        }
        let new = crate::auth::random_token().ok()?;
        if let Some(path) = &self.path {
            crate::auth::write_token_file(path, &new).ok()?;
        }
        *token = Some(new.clone());
        Some(new)
    }

    pub fn matches(&self, bearer: &str) -> bool {
        self.token
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_deref()
            .is_some_and(|t| super::constant_time_eq(t.as_bytes(), bearer.as_bytes()))
    }
}

/// Whether a request with the phone token may go through: one JSON-RPC call to one of [`METHODS`].
pub fn allowed(path: &str, body: &Value) -> bool {
    path == "/rpc"
        && body["method"]
            .as_str()
            .is_some_and(|m| METHODS.contains(&m))
}

/// Waiting agents first (a held prompt, then any prompt or error), then working, then the rest;
/// the longest wait first within each group.
fn rank(state: &str, held: bool) -> u8 {
    match (held, state) {
        (true, _) => 0,
        (_, "permission" | "question" | "awaiting_input") => 1,
        (_, "error" | "stalled") => 2,
        (_, "working") => 3,
        _ => 4,
    }
}

/// Every agent pane tmux still knows, with its label, state, held prompt and recap.
pub async fn snapshot(state: &ApiState) -> Value {
    let labels: std::collections::HashMap<String, String> = crate::tmux_cmd::async_command()
        .args(["list-panes", "-a", "-F", "#{pane_id}\t#S:#I #W"])
        .output()
        .await
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.split_once('\t'))
                .map(|(p, l)| (p.to_string(), l.trim().to_string()))
                .collect()
        })
        .unwrap_or_default();
    let panes: Vec<(String, String, Option<String>, u64)> = state
        .pane_states
        .lock()
        .await
        .iter()
        .filter(|(pane, _)| labels.contains_key(*pane))
        .map(|(pane, info)| {
            (
                pane.clone(),
                info.state.as_str().to_string(),
                info.message.clone(),
                info.changed_at,
            )
        })
        .collect();
    let mut agents = Vec::new();
    for (pane, agent_state, message, changed_at) in panes {
        let held = super::approvals::get(state, &pane).await;
        let recap = state.activity.lock().await.get(&pane).map(|a| json!(a));
        agents.push(json!({
            "pane_id": pane,
            "label": labels[&pane],
            "state": agent_state,
            "message": message,
            "changed_at": changed_at,
            "held": held,
            "recap": recap,
        }));
    }
    agents.sort_by_key(|a| {
        (
            rank(a["state"].as_str().unwrap_or(""), !a["held"].is_null()),
            a["changed_at"].as_u64().unwrap_or(u64::MAX),
        )
    });
    json!({ "agents": agents, "now": crate::sys::unix_ms() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_only_snapshot_and_answer() {
        assert!(allowed("/rpc", &json!({ "method": "phone/snapshot" })));
        assert!(allowed(
            "/rpc",
            &json!({ "method": "approval/answer", "params": {} })
        ));
        for method in [
            "queue/push",
            "tmux.send_keys",
            "phone/token",
            "hook/token",
            "agentState/update",
        ] {
            assert!(!allowed("/rpc", &json!({ "method": method })), "{method}");
        }
        assert!(!allowed(
            "/api/status",
            &json!({ "method": "phone/snapshot" })
        ));
        assert!(!allowed("/rpc", &json!([{ "method": "phone/snapshot" }])));
    }

    #[test]
    fn test_rank_puts_held_prompts_first() {
        assert!(rank("working", true) < rank("permission", false));
        assert!(rank("permission", false) < rank("error", false));
        assert!(rank("error", false) < rank("working", false));
        assert!(rank("working", false) < rank("idle", false));
    }
}
