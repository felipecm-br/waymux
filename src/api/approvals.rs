//! Permission prompts held off the pane (`[approvals] enabled = true`).
//!
//! The harness's `PermissionRequest` hook (`waymux hook tmux PermissionRequest`) calls
//! `approval/hold` and waits. The Cockpit's `y`/`n` answer it with `approval/answer`, so no
//! keystroke reaches the pane; the hook prints the harness's own decision. Claude Code
//! (verified on 2.1.310) keeps showing its own dialog while the hook waits, and whichever
//! answer comes first wins: answering in the pane moves it to `working`, which releases the
//! hold. A hold also ends with no decision when the hold time passes, the pane is focused,
//! it closes, a newer prompt from the same pane arrives, or the daemon stops. Every failure
//! prints nothing, which the harness reads as "ask the user": a failure can never approve.
//!
//! Only prompts whose whole effect fits one line are held (a single-line shell command, a
//! file read, a search); everything else is answered in the pane as before.
//!
//! Limit: any process running as you can read the daemon token, so an agent able to run
//! `curl` or `waymux` unprompted could answer its own prompt, as it could already type into
//! its pane with `tmux send-keys`. The harness's permission rules stay the boundary.

use super::state::ApiState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::sync::oneshot;

/// A prompt must have been held this long before an answer counts.
pub const DWELL_MS: u64 = 400;
pub const MAX_LINE_CHARS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Allow,
    Deny,
}

pub struct HeldPrompt {
    pub id: u64,
    pub tool: String,
    pub line: String,
    pub rules: Vec<&'static str>,
    pub created_at: u64,
    tx: Option<oneshot::Sender<Option<Decision>>>,
}

pub type Held = HashMap<String, HeldPrompt>;

/// Tools whose call one argument decides, shown whole on one line.
pub fn holdable_tool(tool: &str) -> bool {
    matches!(
        tool,
        "Bash" | "Read" | "Glob" | "Grep" | "WebFetch" | "WebSearch"
    )
}

/// The line must be the whole request: one line, short enough to read, nothing hidden.
pub fn holdable_line(line: &str) -> bool {
    !line.trim().is_empty()
        && line.chars().count() <= MAX_LINE_CHARS
        && !line.contains("  ")
        && !line
            .chars()
            .any(|c| c.is_control() || crate::delivery::is_invisible(c))
}

#[derive(Debug, PartialEq, Eq)]
pub enum AnswerError {
    Gone,
    Changed,
    TooSoon,
    RiskAckRequired,
}

impl AnswerError {
    pub fn code(&self) -> &'static str {
        match self {
            AnswerError::Gone => "gone",
            AnswerError::Changed => "changed",
            AnswerError::TooSoon => "too_soon",
            AnswerError::RiskAckRequired => "risk_ack_required",
        }
    }
}

/// Checks an answer against the held prompt (pure, for tests).
pub fn check_answer(
    held: &HeldPrompt,
    id: u64,
    decision: Decision,
    line: &str,
    risk_ack: &[String],
    now_ms: u64,
) -> Result<(), AnswerError> {
    if held.id != id || held.line != line {
        return Err(AnswerError::Changed);
    }
    if now_ms.saturating_sub(held.created_at) < DWELL_MS {
        return Err(AnswerError::TooSoon);
    }
    if decision == Decision::Allow
        && !held.rules.is_empty()
        && risk_ack
            .iter()
            .map(String::as_str)
            .ne(held.rules.iter().copied())
    {
        return Err(AnswerError::RiskAckRequired);
    }
    Ok(())
}

/// Holds the pane's prompt until it is answered or the hold ends. `Ok(None)` is "no decision:
/// let the harness ask"; the reason says why.
pub async fn hold(
    state: &ApiState,
    pane_id: &str,
    tool: &str,
    line: &str,
) -> (Option<Decision>, &'static str) {
    let Some(cfg) = state.settings().approvals else {
        return (None, "disabled");
    };
    if !holdable_tool(tool) || !holdable_line(line) {
        return (None, "not_holdable");
    }
    let id = state
        .next_queue_id
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let rules = if tool == "Bash" {
        crate::risk::risky_rules(line)
    } else {
        Vec::new()
    };
    let risky = !rules.is_empty();
    let (tx, rx) = oneshot::channel();
    {
        let mut held = state.approvals.lock().await;
        if let Some(mut old) = held.insert(
            pane_id.to_string(),
            HeldPrompt {
                id,
                tool: tool.to_string(),
                line: line.to_string(),
                rules,
                created_at: crate::sys::unix_ms(),
                tx: Some(tx),
            },
        ) && let Some(old_tx) = old.tx.take()
        {
            let _ = old_tx.send(None);
        }
    }
    if let Some(phone) = state.settings().phone {
        tokio::spawn(super::phone::alert_if_still_held(
            state.clone(),
            pane_id.to_string(),
            id,
            risky,
            phone,
        ));
    }
    let wait = std::time::Duration::from_secs(cfg.hold_secs());
    let outcome = tokio::time::timeout(wait, rx).await;
    {
        let mut held = state.approvals.lock().await;
        if held.get(pane_id).is_some_and(|h| h.id == id) {
            held.remove(pane_id);
        }
    }
    match outcome {
        Ok(Ok(Some(d))) => (Some(d), "answered"),
        Ok(_) => (None, "released"),
        Err(_) => (None, "timeout"),
    }
}

pub async fn get(state: &ApiState, pane_id: &str) -> serde_json::Value {
    let held = state.approvals.lock().await;
    held.get(pane_id).map_or(serde_json::Value::Null, |h| {
        serde_json::json!({
            "id": h.id,
            "tool": h.tool,
            "line": h.line,
            "rules": h.rules,
            "created_at": h.created_at,
        })
    })
}

pub async fn answer(
    state: &ApiState,
    pane_id: &str,
    id: u64,
    decision: Decision,
    line: &str,
    risk_ack: &[String],
) -> Result<(), AnswerError> {
    let mut held = state.approvals.lock().await;
    let entry = held.get_mut(pane_id).ok_or(AnswerError::Gone)?;
    check_answer(entry, id, decision, line, risk_ack, crate::sys::unix_ms())?;
    let tx = entry.tx.take().ok_or(AnswerError::Gone)?;
    held.remove(pane_id);
    tx.send(Some(decision)).map_err(|_| AnswerError::Gone)
}

/// Ends the pane's hold with no decision: the harness shows its own prompt.
pub async fn release(state: &ApiState, pane_id: &str) {
    let mut held = state.approvals.lock().await;
    if let Some(mut h) = held.remove(pane_id)
        && let Some(tx) = h.tx.take()
    {
        let _ = tx.send(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApprovalsConfig;

    fn enabled_state() -> ApiState {
        let state = ApiState::new(vec![], 0);
        crate::adapters::write(
            &state.settings,
            super::super::state::Settings {
                approvals: Some(ApprovalsConfig {
                    enabled: true,
                    hold_seconds: Some(10),
                }),
                ..state.settings()
            },
        );
        state
    }

    fn held(id: u64, line: &str, rules: Vec<&'static str>, created_at: u64) -> HeldPrompt {
        HeldPrompt {
            id,
            tool: "Bash".into(),
            line: line.into(),
            rules,
            created_at,
            tx: None,
        }
    }

    #[test]
    fn test_holdable_line_rejects_what_one_line_cannot_show() {
        assert!(holdable_line("go test ./..."));
        assert!(!holdable_line(""));
        assert!(!holdable_line("a\nb"));
        assert!(!holdable_line("a\tb"));
        assert!(!holdable_line("echo  hidden"));
        assert!(!holdable_line("rm\u{200B} x"));
        assert!(!holdable_line(&"x".repeat(MAX_LINE_CHARS + 1)));
        assert!(holdable_tool("Bash") && !holdable_tool("Edit") && !holdable_tool("mcp__x"));
    }

    #[test]
    fn test_check_answer_rules() {
        let h = held(7, "rm -rf build", vec!["recursive delete"], 1_000);
        let ack = vec!["recursive delete".to_string()];
        assert_eq!(
            check_answer(&h, 8, Decision::Allow, "rm -rf build", &ack, 5_000),
            Err(AnswerError::Changed)
        );
        assert_eq!(
            check_answer(&h, 7, Decision::Allow, "rm -rf dist", &ack, 5_000),
            Err(AnswerError::Changed)
        );
        assert_eq!(
            check_answer(&h, 7, Decision::Allow, "rm -rf build", &ack, 1_100),
            Err(AnswerError::TooSoon)
        );
        assert_eq!(
            check_answer(&h, 7, Decision::Allow, "rm -rf build", &[], 5_000),
            Err(AnswerError::RiskAckRequired)
        );
        assert_eq!(
            check_answer(&h, 7, Decision::Deny, "rm -rf build", &[], 5_000),
            Ok(()),
            "deny never needs the risk ack"
        );
        assert_eq!(
            check_answer(&h, 7, Decision::Allow, "rm -rf build", &ack, 5_000),
            Ok(())
        );
    }

    #[tokio::test]
    async fn test_disabled_holds_nothing() {
        let state = ApiState::new(vec![], 0);
        assert_eq!(hold(&state, "%1", "Bash", "ls").await, (None, "disabled"));
    }

    #[tokio::test]
    async fn test_not_holdable_returns_at_once() {
        let state = enabled_state();
        assert_eq!(
            hold(&state, "%1", "Edit", "a.rs").await,
            (None, "not_holdable")
        );
        assert_eq!(
            hold(&state, "%1", "Bash", "a\nb").await,
            (None, "not_holdable")
        );
    }

    #[tokio::test]
    async fn test_answer_reaches_the_waiting_hook() {
        let state = enabled_state();
        let waiter = {
            let s = state.clone();
            tokio::spawn(async move { hold(&s, "%2", "Bash", "go test ./...").await })
        };
        let id = loop {
            if let Some(id) = get(&state, "%2").await["id"].as_u64() {
                break id;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        };
        assert_eq!(
            answer(&state, "%2", id, Decision::Allow, "go test ./...", &[]).await,
            Err(AnswerError::TooSoon)
        );
        tokio::time::sleep(std::time::Duration::from_millis(DWELL_MS + 20)).await;
        answer(&state, "%2", id, Decision::Allow, "go test ./...", &[])
            .await
            .unwrap();
        assert_eq!(waiter.await.unwrap(), (Some(Decision::Allow), "answered"));
        assert!(get(&state, "%2").await.is_null());
    }

    #[tokio::test]
    async fn test_release_and_newer_prompt_end_the_hold_without_a_decision() {
        let state = enabled_state();
        let first = {
            let s = state.clone();
            tokio::spawn(async move { hold(&s, "%3", "Bash", "ls").await })
        };
        while get(&state, "%3").await.is_null() {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let second = {
            let s = state.clone();
            tokio::spawn(async move { hold(&s, "%3", "Bash", "pwd").await })
        };
        assert_eq!(first.await.unwrap(), (None, "released"));
        while get(&state, "%3").await["line"] != "pwd" {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        release(&state, "%3").await;
        assert_eq!(second.await.unwrap(), (None, "released"));
    }
}
