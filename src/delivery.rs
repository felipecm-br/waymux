//! Client side of the delivery queue: every prompt waymux types into an agent pane (`inject`,
//! `review-notes send`, the Cockpit's `I`) goes through [`dispatch_prompt`], and `waymux
//! queue` lists and drops what waits.

use crate::rpc_client::{CallError, rpc_call};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

/// A prompt larger than this is written to a file and the agent gets its path: one paste of
/// a few hundred lines through the PTY is slow, and the agent reads a file with its own tools.
pub const OFFLOAD_BYTES: usize = 12 * 1024;
/// Offloaded pastes older than this are removed when the next one is written.
const PASTE_MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, PartialEq, Eq)]
pub enum Dispatch {
    /// Held in the daemon; typed when the agent is at rest. `position` 1 is next.
    Queued { position: u64 },
    /// No daemon: typed directly, after checking the hook's last state for the pane.
    Typed,
    /// Not sent; the reason is for the user.
    Refused(String),
}

impl Dispatch {
    pub fn message(&self, pane: &str) -> String {
        match self {
            Dispatch::Queued { position: 1 } => {
                format!("󰇮 Prompt queued for {pane}: typed when the agent is at rest")
            }
            Dispatch::Queued { position } => {
                format!("󰇮 Prompt queued for {pane} ({position} waiting)")
            }
            Dispatch::Typed => format!("󰊢 Prompt typed into {pane} (daemon not running)"),
            Dispatch::Refused(why) => format!("󰇮 Prompt not sent to {pane}: {why}"),
        }
    }
}

/// Invisible characters that can hide instructions in text pasted from the web: Unicode
/// tags, bidi controls and zero-width spaces. ZWJ/ZWNJ stay (emoji and some scripts use them).
pub fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{200B}' | '\u{200E}' | '\u{200F}' | '\u{2060}' | '\u{FEFF}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2066}'..='\u{2069}'
        | '\u{E0000}'..='\u{E007F}')
}

pub fn strip_invisible(text: &str) -> String {
    text.chars().filter(|c| !is_invisible(*c)).collect()
}

/// The text to type: invisible characters removed, and a large prompt replaced by a
/// reference to a private file holding it.
pub fn prepare_prompt(text: &str) -> String {
    let clean = strip_invisible(text);
    if clean.len() <= OFFLOAD_BYTES {
        return clean;
    }
    match offload(&clean) {
        Some(path) => format!(
            "[Pasted text: {}, {} lines]",
            path.display(),
            clean.lines().count()
        ),
        None => clean,
    }
}

fn offload(text: &str) -> Option<PathBuf> {
    let dir = crate::presets::private_runtime_dir()?.join("paste");
    fs::create_dir_all(&dir).ok()?;
    prune_old_pastes(&dir);
    let path = dir.join(format!(
        "paste-{}-{}.txt",
        crate::sys::unix_ms(),
        std::process::id()
    ));
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .ok()?;
    f.write_all(text.as_bytes()).ok()?;
    Some(path)
}

fn prune_old_pastes(dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age.as_secs() > PASTE_MAX_AGE_SECS);
        if old {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// States the hook may have last written for a pane that must never be typed into.
pub fn blocks_typing(state: &str) -> bool {
    matches!(
        state,
        "working" | "busy" | "permission" | "question" | "awaiting_input"
    )
}

/// The state the hook last wrote for `pane` (it writes one file per pane whether or not the
/// daemon runs).
fn hook_state(pane: &str) -> Option<String> {
    let safe: String = pane
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let file = std::env::temp_dir().join(format!("acpd-pane-state-{safe}.json"));
    let val: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
    val.get("state")?.as_str().map(str::to_string)
}

/// Sends `text` to the agent in `pane` through the daemon's delivery queue. Without a daemon
/// it types directly, unless the hook last saw the agent working or on a prompt.
pub fn dispatch_prompt(pane: &str, text: &str, by: &str) -> Dispatch {
    let prompt = prepare_prompt(text);
    if prompt.trim().is_empty() {
        return Dispatch::Refused("empty prompt".into());
    }
    let params = serde_json::json!({ "pane_id": pane, "text": prompt, "by": by });
    match rpc_call("queue/push", &params, 500) {
        Ok(res) => Dispatch::Queued {
            position: res
                .get("position")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(1),
        },
        Err(CallError::Rejected(code)) => Dispatch::Refused(match code.as_str() {
            "queue_full" => "the queue is full (waymux queue ls)".into(),
            "too_large" => "the prompt is too large".into(),
            other => other.to_string(),
        }),
        Err(CallError::Unreachable) => {
            if let Some(state) = hook_state(pane).filter(|s| blocks_typing(s)) {
                let doing = if matches!(state.as_str(), "working" | "busy") {
                    "mid-turn"
                } else {
                    "waiting on a prompt"
                };
                return Dispatch::Refused(format!(
                    "the agent is {doing} and the daemon is not running to hold it"
                ));
            }
            if type_now(pane, &prompt) {
                Dispatch::Typed
            } else {
                Dispatch::Refused("tmux could not type into the pane".into())
            }
        }
    }
}

fn type_now(pane: &str, text: &str) -> bool {
    use crate::tmux_cmd::{tmux_load_and_paste_bracketed, tmux_status};
    if tmux_load_and_paste_bracketed(Some("waymux_queue"), pane, text) {
        return true;
    }
    let flat = text.replace(['\n', '\r'], " ");
    tmux_status(&["send-keys", "-l", "-t", pane, "--", &flat])
        && tmux_status(&["send-keys", "-t", pane, "Enter"])
}

fn current_pane() -> String {
    std::env::var("TMUX_PANE").unwrap_or_default()
}

/// `waymux queue add <pane> <text…> | ls [pane] | rm <pane> [id]`
pub fn run_cli(args: &[String]) -> i32 {
    let sub = args.first().map_or("ls", String::as_str);
    match sub {
        "add" | "push" => {
            let pane = args.get(1).cloned().unwrap_or_default();
            let text = args.get(2..).map(|a| a.join(" ")).unwrap_or_default();
            if pane.is_empty() || text.trim().is_empty() {
                eprintln!("Usage: waymux queue add <pane_id> <text>");
                return 1;
            }
            let outcome = dispatch_prompt(&pane, &text, "cli");
            println!("{}", outcome.message(&pane));
            i32::from(matches!(outcome, Dispatch::Refused(_)))
        }
        "ls" | "list" => {
            let params = args.get(1).map_or(
                serde_json::Value::Null,
                |p| serde_json::json!({ "pane_id": p }),
            );
            match rpc_call("queue/list", &params, 500) {
                Ok(serde_json::Value::Object(map)) => {
                    let mut panes: Vec<_> = map.into_iter().collect();
                    panes.sort_by(|a, b| a.0.cmp(&b.0));
                    for (pane, items) in panes {
                        for item in items.as_array().into_iter().flatten() {
                            let text = item["text"].as_str().unwrap_or("");
                            let first = text.lines().next().unwrap_or("");
                            println!(
                                "{pane}\t{}\t{}\t{}\t{}",
                                item["id"],
                                item["status"].as_str().unwrap_or(""),
                                item["by"].as_str().unwrap_or(""),
                                first.chars().take(60).collect::<String>()
                            );
                        }
                    }
                    0
                }
                Ok(_) => 0,
                Err(_) => {
                    eprintln!("waymux daemon is not running");
                    1
                }
            }
        }
        "rm" | "cancel" => {
            let pane = args.get(1).cloned().unwrap_or_else(current_pane);
            if pane.is_empty() {
                eprintln!("Usage: waymux queue rm <pane_id> [id]");
                return 1;
            }
            let mut params = serde_json::json!({ "pane_id": pane });
            if let Some(id) = args.get(2).and_then(|i| i.parse::<u64>().ok()) {
                params["id"] = serde_json::json!(id);
            }
            let Ok(res) = rpc_call("queue/cancel", &params, 500) else {
                eprintln!("waymux daemon is not running");
                return 1;
            };
            let n = res["dropped"].as_u64().unwrap_or(0);
            println!("Dropped {n} queued prompt(s) for {pane}");
            i32::from(n == 0)
        }
        _ => {
            eprintln!("Usage: waymux queue <add <pane> <text> | ls [pane] | rm <pane> [id]>");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_invisible_keeps_visible_text_and_joiners() {
        let hidden = "run\u{200B} tests\u{202E}\u{E0041}\u{E0042} now\u{FEFF}";
        assert_eq!(strip_invisible(hidden), "run tests now");
        // ZWJ in an emoji sequence and Nerd Font glyphs survive.
        let kept = "👩\u{200D}💻 󰊢";
        assert_eq!(strip_invisible(kept), kept);
    }

    #[test]
    fn test_small_prompt_is_not_offloaded() {
        assert_eq!(prepare_prompt("fix line 42"), "fix line 42");
    }

    #[test]
    fn test_blocks_typing() {
        for s in ["working", "permission", "question", "awaiting_input"] {
            assert!(blocks_typing(s), "{s}");
        }
        for s in ["idle", "", "error", "closed"] {
            assert!(!blocks_typing(s), "{s}");
        }
    }

    #[test]
    fn test_dispatch_messages_name_the_pane() {
        assert!(
            Dispatch::Queued { position: 1 }
                .message("%3")
                .contains("%3")
        );
        assert!(
            Dispatch::Queued { position: 3 }
                .message("%3")
                .contains("3 waiting")
        );
        assert!(
            Dispatch::Refused("x".into())
                .message("%3")
                .contains("not sent")
        );
    }
}
