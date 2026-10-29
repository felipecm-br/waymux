//! Pane-scoped hook tokens, for agents in a sandbox.
//!
//! An agent in `ai-jail` cannot read the daemon token (the jail has no `$XDG_RUNTIME_DIR`), so its
//! hooks never reach the daemon and its pane gets no state, held approvals or checkpoints. Giving it
//! the daemon token would be worse: `tmux.*` and `inject` type into every pane, the unjailed ones
//! included. A hook token is minted for one pane (`hook/token`, daemon token only) and allows only
//! what a hook does for that pane: report its state (`/api/status`) and hold its permission prompt
//! (`approval/hold`). The worst an agent can do with its own is misreport its own pane.
//!
//! Tokens are saved in `$XDG_STATE_HOME/waymux/hook-tokens` (0600), so a restarted daemon still
//! knows the jailed agents already running. The file names the tmux server it was written for: a new
//! server numbers its panes from `%0` again, so its panes would inherit old tokens, and a file from
//! another server is ignored.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// `Default` keeps the tokens in memory only (tests); the daemon uses [`HookTokens::load`].
#[derive(Debug, Default)]
pub struct HookTokens {
    /// Token → pane. One token per pane: minting again replaces the old one.
    by_token: Mutex<HashMap<String, String>>,
    path: Option<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    /// `#{pid}` of the tmux server the panes belong to.
    tmux_pid: String,
    tokens: HashMap<String, String>,
}

fn tmux_pid() -> Option<String> {
    crate::tmux_cmd::tmux_out(&["display-message", "-p", "#{pid}"]).filter(|p| !p.is_empty())
}

impl HookTokens {
    /// The tokens saved by an earlier daemon for the running tmux server, if any.
    pub fn load() -> Self {
        let path = crate::auth::state_file("hook-tokens");
        let by_token = path
            .as_ref()
            .zip(tmux_pid())
            .map(|(p, pid)| saved_for(p, &pid))
            .unwrap_or_default();
        Self {
            by_token: Mutex::new(by_token),
            path,
        }
    }

    /// Writes the tokens for the running tmux server; without one there is nothing to keep.
    fn save(&self, by_token: &HashMap<String, String>) {
        let (Some(path), Some(pid)) = (&self.path, tmux_pid()) else {
            return;
        };
        let saved = Saved {
            tmux_pid: pid,
            tokens: by_token.clone(),
        };
        if let Ok(json) = serde_json::to_string(&saved)
            && let Err(e) = crate::auth::write_token_file(path, &json)
        {
            tracing::warn!("cannot save hook tokens to {}: {e}", path.display());
        }
    }

    /// A new token for `pane`, or `None` when `pane` is not a tmux pane id or randomness fails.
    pub fn mint(&self, pane: &str) -> Option<String> {
        if !valid_pane(pane) {
            return None;
        }
        let token = crate::auth::random_token().ok()?;
        let mut by_token = self
            .by_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        by_token.retain(|_, p| p != pane);
        by_token.insert(token.clone(), pane.to_string());
        self.save(&by_token);
        Some(token)
    }

    /// The pane `token` was minted for. Every stored token is compared in constant time.
    pub fn pane_of(&self, token: &str) -> Option<String> {
        let by_token = self
            .by_token
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut found = None;
        for (t, pane) in by_token.iter() {
            if super::constant_time_eq(t.as_bytes(), token.as_bytes()) {
                found = Some(pane.clone());
            }
        }
        found
    }
}

/// The tokens in `path` when it was written for tmux server `pid`, with every pane id checked.
fn saved_for(path: &std::path::Path, pid: &str) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Saved>(&s).ok())
        .filter(|s| s.tmux_pid == pid)
        .map(|s| {
            s.tokens
                .into_iter()
                .filter(|(t, p)| !t.is_empty() && valid_pane(p))
                .collect()
        })
        .unwrap_or_default()
}

/// `%` followed by digits, as tmux names panes.
pub fn valid_pane(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether a request with a hook token for `pane` may go through: its own status, or holding
/// its own prompt. A batch, another method or another pane is refused.
pub fn allowed(path: &str, body: &Value, pane: &str) -> bool {
    match path {
        "/api/status" => body["pane_id"] == pane,
        "/rpc" => body["method"] == "approval/hold" && body["params"]["pane_id"] == pane,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_mint_binds_one_token_per_pane() {
        let tokens = HookTokens::default();
        let first = tokens.mint("%3").unwrap();
        assert_eq!(tokens.pane_of(&first).as_deref(), Some("%3"));
        let second = tokens.mint("%3").unwrap();
        assert_ne!(first, second);
        assert_eq!(tokens.pane_of(&first), None);
        assert_eq!(tokens.pane_of(&second).as_deref(), Some("%3"));
        assert_eq!(tokens.pane_of(""), None);
    }

    #[test]
    fn test_saved_tokens_hold_only_for_the_same_tmux_server() {
        let dir = std::env::temp_dir().join(format!("waymux-hook-tokens-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hook-tokens");
        let saved = json!({ "tmux_pid": "4242",
                            "tokens": { "abc": "%3", "def": "not-a-pane", "": "%4" } });
        std::fs::write(&path, saved.to_string()).unwrap();

        let same = saved_for(&path, "4242");
        assert_eq!(same.len(), 1);
        assert_eq!(same["abc"], "%3");
        assert!(saved_for(&path, "4243").is_empty());
        std::fs::write(&path, "not json").unwrap();
        assert!(saved_for(&path, "4242").is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_mint_refuses_what_is_not_a_pane() {
        let tokens = HookTokens::default();
        for pane in ["", "%", "3", "%3a", "%-1", "api:2"] {
            assert_eq!(tokens.mint(pane), None, "{pane}");
        }
    }

    #[test]
    fn test_allowed_only_for_the_own_pane() {
        let status = json!({ "pane_id": "%3", "state": "idle" });
        assert!(allowed("/api/status", &status, "%3"));
        assert!(!allowed("/api/status", &status, "%4"));

        let hold = json!({ "jsonrpc": "2.0", "id": 1, "method": "approval/hold",
                           "params": { "pane_id": "%3", "tool": "Bash", "line": "ls" } });
        assert!(allowed("/rpc", &hold, "%3"));
        assert!(!allowed("/rpc", &hold, "%4"));

        for method in [
            "approval/answer",
            "queue/push",
            "tmux.send_keys",
            "hook/token",
        ] {
            let call = json!({ "jsonrpc": "2.0", "id": 1, "method": method,
                               "params": { "pane_id": "%3" } });
            assert!(!allowed("/rpc", &call, "%3"), "{method}");
        }
        assert!(!allowed("/rpc", &json!([hold]), "%3"));
        assert!(!allowed("/health", &status, "%3"));
    }
}
