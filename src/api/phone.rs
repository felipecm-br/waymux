//! The phone surface: a page at `/m` that lists the agents and answers held prompts.
//!
//! It is meant for a phone on the same tailnet (`tailscale serve` in front of the daemon). The page
//! itself carries no data and needs no token. It reads the token from its URL fragment, which a
//! browser never sends to a server, and calls two methods with it: `phone/snapshot` and
//! `approval/answer`. A phone token can do nothing else: no typing into panes, no queue, no file
//! paths from the client (the checklist is TUIOS's three advisories of 26 Oct 2026).
//!
//! With `[phone] ntfy` set, a prompt still held after `alert_after_seconds` (so not answered at
//! the desk) sends an ntfy alert that names the window and opens the page. The alert carries no
//! command text: it goes through the ntfy server, and the call is on the page behind the token.
//!
//! One token at a time, minted with `waymux phone-token` (daemon token only) and kept in
//! `$XDG_STATE_HOME/waymux/phone-token` (0600), so pairing survives a daemon restart.
//! `--rotate` replaces it and unpairs every device that held the old one.
//!
//! The same token also reads the diff of an agent's last turn (`phone/diff`): the daemon picks
//! the pane's newest checkpoint itself, and the client names a pane, never a path or a commit.
//!
//! Typing a prompt (`phone/prompt`) needs a second token, `waymux phone-token --prompt`, kept
//! in `phone-prompt-token`: pairing a phone to watch and answer does not give it the keyboard.
//! A prompt goes through the delivery queue (`super::queue`), so it waits for the agent to be
//! at rest and never lands in a permission prompt or the middle of a turn, and only to a pane
//! the daemon has agent state for, so it is never typed at once into a bare shell.

use super::ApiState;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::RwLock;

/// The methods a phone token may call.
pub const METHODS: [&str; 3] = ["phone/snapshot", "approval/answer", "phone/diff"];

/// What a prompt token may call on top of [`METHODS`].
pub const PROMPT_METHODS: [&str; 1] = ["phone/prompt"];

/// Largest prompt typed from the phone; a thumb does not write more.
pub const MAX_PROMPT_BYTES: usize = 4 * 1024;

/// Largest diff sent to the phone; the rest is cut and marked `truncated`.
pub const MAX_DIFF_BYTES: usize = 64 * 1024;

pub const PAGE: &str = include_str!("phone.html");

/// `Default` keeps the token in memory only (tests); the daemon uses [`PhoneToken::load`].
#[derive(Debug, Default)]
pub struct PhoneToken {
    token: RwLock<Option<String>>,
    path: Option<PathBuf>,
}

impl PhoneToken {
    /// The token saved by an earlier daemon, if any.
    pub fn load() -> Self {
        Self::load_named("phone-token")
    }

    /// The prompt token saved by an earlier daemon, if any.
    pub fn load_prompt() -> Self {
        Self::load_named("phone-prompt-token")
    }

    fn load_named(name: &str) -> Self {
        let path = crate::auth::state_file(name);
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

/// Whether a request with a phone token may go through: one JSON-RPC call to one of
/// [`METHODS`], or of [`PROMPT_METHODS`] with the prompt token.
pub fn allowed(path: &str, body: &Value, prompt: bool) -> bool {
    path == "/rpc"
        && body["method"]
            .as_str()
            .is_some_and(|m| METHODS.contains(&m) || (prompt && PROMPT_METHODS.contains(&m)))
}

/// A prompt as typed text: line endings as `\n`, every other control character (escape
/// sequences, `\r` that would submit early) dropped, surrounding blank space trimmed.
fn clean_prompt(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect::<String>()
        .trim()
        .to_string()
}

/// Whether the daemon has agent state for `pane`: the queue types at once into a pane it does
/// not know, which from the phone would be a shell.
async fn known_agent(state: &ApiState, pane: &str) -> bool {
    super::hook_tokens::valid_pane(pane) && state.pane_states.lock().await.contains_key(pane)
}

/// Queues `text` for the agent in `pane`, as `phone`.
pub async fn prompt(state: &ApiState, pane: &str, text: &str) -> Result<Value, &'static str> {
    if !known_agent(state, pane).await {
        return Err("unknown_agent");
    }
    let text = clean_prompt(text);
    if text.len() > MAX_PROMPT_BYTES {
        return Err("too_large");
    }
    super::queue::push(state, pane, &text, "phone")
        .await
        .map(|(id, position)| json!({ "id": id, "position": position, "status": "queued" }))
        .map_err(|e| e.code())
}

/// `text` cut to at most `max` bytes on a character boundary, and whether it was cut.
fn cut(text: String, max: usize) -> (String, bool) {
    if text.len() <= max {
        return (text, false);
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

/// The diff of the newest checkpoint of `pane` (its agent's last turn), plain text.
pub async fn diff(state: &ApiState, pane: &str) -> Result<Value, &'static str> {
    if !known_agent(state, pane).await {
        return Err("unknown_agent");
    }
    let pane = pane.to_string();
    tokio::task::spawn_blocking(move || {
        let dir = crate::checkpoint::pane_dir(&pane).ok_or("no_directory")?;
        let repo = crate::checkpoint::Repo::open(&dir, None).ok_or("no_repository")?;
        let last = repo.list(&pane).pop().ok_or("no_checkpoint")?;
        let text = repo.diff(&pane, last.n, false).map_err(|_| "diff_failed")?;
        let (text, truncated) = cut(text, MAX_DIFF_BYTES);
        Ok(json!({
            "n": last.n,
            "subject": last.subject,
            "time": last.time,
            "diff": text,
            "truncated": truncated,
        }))
    })
    .await
    .map_err(|_| "diff_failed")?
}

/// This machine's Tailscale name (`host.tailnet.ts.net`), if Tailscale runs.
pub fn tailscale_host() -> Option<String> {
    std::process::Command::new("tailscale")
        .args(["status", "--json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| {
            v["Self"]["DNSName"]
                .as_str()
                .map(|n| n.trim_end_matches('.').to_string())
        })
        .filter(|n| !n.is_empty())
}

/// An `http(s)` URL that fits in a curl config line and an HTTP header: no space, quote,
/// backslash or control character.
fn plain_url(url: &str) -> bool {
    (url.starts_with("https://") || url.starts_with("http://"))
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
}

/// A window name as text: Nerd Font icons (private use), control characters and runs of
/// spaces dropped, at most 60 characters.
fn plain_label(label: &str) -> String {
    let kept: String = label
        .chars()
        .filter(|c| !c.is_control() && !matches!(*c as u32, 0xE000..=0xF8FF | 0xF0000..))
        .collect();
    kept.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(60)
        .collect()
}

fn alert_text(label: &str, risky: bool) -> String {
    let who = if label.is_empty() { "An agent" } else { label };
    if risky {
        format!("{who} waits for you (risky call)")
    } else {
        format!("{who} waits for you")
    }
}

/// Sleeps `alert_after_seconds`, then alerts the phone if prompt `id` is still held on `pane`.
pub async fn alert_if_still_held(
    state: ApiState,
    pane: String,
    id: u64,
    risky: bool,
    cfg: crate::config::PhoneConfig,
) {
    tokio::time::sleep(std::time::Duration::from_secs(cfg.alert_after_secs())).await;
    if state
        .approvals
        .lock()
        .await
        .get(&pane)
        .is_none_or(|h| h.id != id)
    {
        return;
    }
    let label = crate::tmux_cmd::async_command()
        .args(["display-message", "-p", "-t", &pane, "#{pane_id} #S:#I #W"])
        .output()
        .await
        .ok()
        // tmux prints empty fields, and exits 0, for a pane that is gone.
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .strip_prefix(&format!("{pane} "))
                .map(plain_label)
        })
        .unwrap_or_default();
    if let Err(e) = send_alert(&cfg, &alert_text(&label, risky)).await {
        tracing::warn!("phone alert not sent: {e}");
    }
}

/// Posts `text` to the ntfy topic with curl. The topic URL goes in on stdin, not argv, so other
/// users cannot read it in the process list.
pub async fn send_alert(cfg: &crate::config::PhoneConfig, text: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let topic = cfg.topic().unwrap_or_default();
    anyhow::ensure!(
        plain_url(&topic),
        "[phone] ntfy (or the first line of ntfy_file) must be an http(s) URL"
    );
    let page = match &cfg.url {
        Some(url) => Some(url.clone()),
        None => tokio::task::spawn_blocking(tailscale_host)
            .await
            .ok()
            .flatten()
            .map(|host| format!("https://{host}/m")),
    }
    .filter(|url| plain_url(url));
    let mut cmd = tokio::process::Command::new("curl");
    cmd.args(["-sS", "-o", "/dev/null", "--fail", "--max-time", "10"])
        .args(["--proto", "=https,http", "-K", "-"])
        .args([
            "-H",
            "Title: waymux",
            "-H",
            "Priority: high",
            "-H",
            "Tags: hourglass",
        ])
        .args(["--data-binary", text]);
    if let Some(page) = page {
        cmd.args(["-H", &format!("Click: {page}")]);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(format!("url = \"{topic}\"\n").as_bytes())
            .await?;
    }
    let out = child.wait_with_output().await?;
    anyhow::ensure!(
        out.status.success(),
        "curl: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
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
    fn test_allowed_only_snapshot_answer_and_diff() {
        for prompt in [false, true] {
            for method in ["phone/snapshot", "approval/answer", "phone/diff"] {
                assert!(
                    allowed("/rpc", &json!({ "method": method }), prompt),
                    "{method}"
                );
            }
            for method in [
                "queue/push",
                "tmux.send_keys",
                "phone/token",
                "phone/alert",
                "hook/token",
                "agentState/update",
                "checkpoint/restore",
            ] {
                assert!(
                    !allowed("/rpc", &json!({ "method": method }), prompt),
                    "{method}"
                );
            }
            assert!(!allowed(
                "/api/status",
                &json!({ "method": "phone/snapshot" }),
                prompt
            ));
            assert!(!allowed(
                "/rpc",
                &json!([{ "method": "phone/snapshot" }]),
                prompt
            ));
        }
    }

    #[test]
    fn test_only_the_prompt_token_types() {
        let body = json!({ "method": "phone/prompt" });
        assert!(!allowed("/rpc", &body, false));
        assert!(allowed("/rpc", &body, true));
        assert!(!allowed("/api/status", &body, true));
    }

    #[test]
    fn test_clean_prompt_keeps_lines_and_drops_escapes() {
        assert_eq!(
            clean_prompt("  fix the test\r\nthen run it \n"),
            "fix the test\nthen run it"
        );
        assert_eq!(clean_prompt("a\u{1b}[2Jb\rc\u{7}"), "a[2Jbc");
        assert_eq!(clean_prompt("tab\there"), "tab\there");
        assert_eq!(clean_prompt(" \r\n\t "), "");
    }

    #[test]
    fn test_cut_stays_on_a_character_boundary() {
        assert_eq!(cut("abc".into(), 5), ("abc".into(), false));
        assert_eq!(cut("abcdef".into(), 3), ("abc".into(), true));
        // "é" is two bytes: a cut at 2 would split it.
        assert_eq!(cut("aé".into(), 2), ("a".into(), true));
    }

    #[tokio::test]
    async fn test_prompt_and_diff_refuse_panes_with_no_agent() {
        let state = ApiState::new(Vec::new(), 0);
        assert_eq!(prompt(&state, "%1", "hi").await, Err("unknown_agent"));
        assert_eq!(prompt(&state, "api:1", "hi").await, Err("unknown_agent"));
        assert_eq!(diff(&state, "%1").await, Err("unknown_agent"));
        assert_eq!(diff(&state, "../etc").await, Err("unknown_agent"));
    }

    #[tokio::test]
    async fn test_prompt_queues_for_a_known_agent() {
        let state = ApiState::new(Vec::new(), 0);
        // Working: the queue holds the prompt and types nothing until the agent rests.
        state.pane_states.lock().await.insert(
            "%3".into(),
            crate::api::PaneStateInfo {
                last_timestamp: 0,
                seq_id: 1,
                state: crate::adapters::AgentState::Working,
                message: None,
                finished_at: 0,
                changed_at: 0,
            },
        );
        let queued = prompt(&state, "%3", "  run the tests\r\n").await.unwrap();
        assert_eq!(queued["position"], 1);
        let list = crate::api::queue::list(&state, Some("%3")).await;
        assert!(list.to_string().contains("run the tests"), "{list}");
        assert!(list.to_string().contains(r#""by":"phone""#), "{list}");
        assert_eq!(prompt(&state, "%3", " \r\n").await, Err("empty"));
        let long = "x".repeat(MAX_PROMPT_BYTES + 1);
        assert_eq!(prompt(&state, "%3", &long).await, Err("too_large"));
    }

    #[test]
    fn test_alert_names_the_window_and_never_the_call() {
        assert_eq!(plain_label("api:2 \u{f121} build\n"), "api:2 build");
        assert_eq!(plain_label("a\u{1b}[31mb"), "a[31mb");
        assert_eq!(plain_label(&"x".repeat(80)).len(), 60);
        assert_eq!(
            alert_text("api:2 build", false),
            "api:2 build waits for you"
        );
        assert_eq!(alert_text("", true), "An agent waits for you (risky call)");
    }

    #[test]
    fn test_plain_url_refuses_what_breaks_a_curl_config_line() {
        assert!(plain_url("https://ntfy.sh/waymux-3f9a"));
        assert!(plain_url("http://ntfy.tail.ts.net/topic"));
        for url in [
            "ntfy.sh/topic",
            "file:///etc/passwd",
            "https://ntfy.sh/a b",
            "https://ntfy.sh/a\"\nurl=x",
            "https://ntfy.sh/a\\",
        ] {
            assert!(!plain_url(url), "{url}");
        }
    }

    #[test]
    fn test_rank_puts_held_prompts_first() {
        assert!(rank("working", true) < rank("permission", false));
        assert!(rank("permission", false) < rank("error", false));
        assert!(rank("error", false) < rank("working", false));
        assert!(rank("working", false) < rank("idle", false));
    }
}
