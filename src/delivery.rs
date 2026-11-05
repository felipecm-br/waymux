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
    prepare_prompt_with(text, offload)
}

fn prepare_prompt_with(text: &str, offload_fn: impl FnOnce(&str) -> Option<PathBuf>) -> String {
    let clean = strip_invisible(text);
    if clean.len() <= OFFLOAD_BYTES {
        return clean;
    }
    match offload_fn(&clean) {
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
    offload_in(&dir, text)
}

fn offload_in(dir: &std::path::Path, text: &str) -> Option<PathBuf> {
    fs::create_dir_all(dir).ok()?;
    prune_old_pastes(dir);
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
    hook_state_in(&std::env::temp_dir(), pane)
}

fn hook_state_in(dir: &std::path::Path, pane: &str) -> Option<String> {
    let safe: String = pane
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let file = dir.join(format!("acpd-pane-state-{safe}.json"));
    let val: serde_json::Value = serde_json::from_str(&fs::read_to_string(file).ok()?).ok()?;
    val.get("state")?.as_str().map(str::to_string)
}

/// Sends `text` to the agent in `pane` through the daemon's delivery queue. Without a daemon
/// it types directly, unless the hook last saw the agent working or on a prompt.
pub fn dispatch_prompt(pane: &str, text: &str, by: &str) -> Dispatch {
    dispatch_prompt_with(
        pane,
        text,
        by,
        |method, params| rpc_call(method, params, 500),
        hook_state,
        type_now,
    )
}

fn dispatch_prompt_with(
    pane: &str,
    text: &str,
    by: &str,
    mut rpc: impl FnMut(&str, &serde_json::Value) -> Result<serde_json::Value, CallError>,
    hook: impl FnOnce(&str) -> Option<String>,
    mut type_fn: impl FnMut(&str, &str) -> bool,
) -> Dispatch {
    let prompt = prepare_prompt(text);
    if prompt.trim().is_empty() {
        return Dispatch::Refused("empty prompt".into());
    }
    let params = serde_json::json!({ "pane_id": pane, "text": prompt, "by": by });
    match rpc("queue/push", &params) {
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
            if let Some(state) = hook(pane).filter(|s| blocks_typing(s)) {
                let doing = if matches!(state.as_str(), "working" | "busy") {
                    "mid-turn"
                } else {
                    "waiting on a prompt"
                };
                return Dispatch::Refused(format!(
                    "the agent is {doing} and the daemon is not running to hold it"
                ));
            }
            if type_fn(pane, &prompt) {
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
    run_cli_with(
        args,
        |method, params| rpc_call(method, params, 500),
        dispatch_prompt,
        current_pane,
    )
}

fn run_cli_with(
    args: &[String],
    mut rpc: impl FnMut(&str, &serde_json::Value) -> Result<serde_json::Value, CallError>,
    mut dispatch: impl FnMut(&str, &str, &str) -> Dispatch,
    pane_env: impl FnOnce() -> String,
) -> i32 {
    let sub = args.first().map_or("ls", String::as_str);
    match sub {
        "add" | "push" => {
            let pane = args.get(1).cloned().unwrap_or_default();
            let text = args.get(2..).map(|a| a.join(" ")).unwrap_or_default();
            if pane.is_empty() || text.trim().is_empty() {
                eprintln!("Usage: waymux queue add <pane_id> <text>");
                return 1;
            }
            let outcome = dispatch(&pane, &text, "cli");
            println!("{}", outcome.message(&pane));
            i32::from(matches!(outcome, Dispatch::Refused(_)))
        }
        "ls" | "list" => {
            let params = args.get(1).map_or(
                serde_json::Value::Null,
                |p| serde_json::json!({ "pane_id": p }),
            );
            match rpc("queue/list", &params) {
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
            let pane = args.get(1).cloned().unwrap_or_else(pane_env);
            if pane.is_empty() {
                eprintln!("Usage: waymux queue rm <pane_id> [id]");
                return 1;
            }
            let mut params = serde_json::json!({ "pane_id": pane });
            if let Some(id) = args.get(2).and_then(|i| i.parse::<u64>().ok()) {
                params["id"] = serde_json::json!(id);
            }
            let Ok(res) = rpc("queue/cancel", &params) else {
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
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_strip_invisible_keeps_visible_text_and_joiners() {
        let hidden = "run\u{200B} tests\u{202E}\u{E0041}\u{E0042} now\u{FEFF}";
        assert_eq!(strip_invisible(hidden), "run tests now");
        // ZWJ in an emoji sequence and Nerd Font glyphs survive.
        let kept = "👩\u{200D}💻 󰊢";
        assert_eq!(strip_invisible(kept), kept);
        for c in [
            '\u{200B}',
            '\u{200E}',
            '\u{200F}',
            '\u{2060}',
            '\u{FEFF}',
            '\u{202A}',
            '\u{202E}',
            '\u{2066}',
            '\u{2069}',
            '\u{E0000}',
            '\u{E007F}',
        ] {
            assert!(is_invisible(c), "{c:?}");
        }
        assert!(!is_invisible('\u{200D}'));
        assert!(!is_invisible('a'));
        assert!(!is_invisible(' '));
        assert_eq!(OFFLOAD_BYTES, 12288);
        assert_eq!(PASTE_MAX_AGE_SECS, 86400);
    }

    #[test]
    fn test_small_prompt_is_not_offloaded_and_large_prompt_is() {
        let exact = "x".repeat(OFFLOAD_BYTES);
        assert_eq!(
            prepare_prompt_with(&exact, |_| panic!("must not offload at limit")),
            exact
        );
        let over = format!("{exact}\nline2");
        let replaced = prepare_prompt_with(&over, |_| Some(PathBuf::from("/tmp/paste-1.txt")));
        assert_eq!(replaced, "[Pasted text: /tmp/paste-1.txt, 2 lines]");
        // Fallback if offload fails returns cleaned text.
        assert_eq!(prepare_prompt_with(&over, |_| None), over);
    }

    #[test]
    fn test_offload_in_writes_0600_file_and_prunes_stale_entries() {
        let dir = std::env::temp_dir().join(format!("waymux-delivery-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let fresh = dir.join("fresh.txt");
        fs::write(&fresh, "keep").unwrap();

        let path = offload_in(&dir, "hello paste\nline 2").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello paste\nline 2");
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(fresh.exists());

        prune_old_pastes(&dir.join("missing-subdir"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_blocks_typing_and_hook_state_in() {
        for s in [
            "working",
            "busy",
            "permission",
            "question",
            "awaiting_input",
        ] {
            assert!(blocks_typing(s), "{s}");
        }
        for s in ["idle", "", "error", "closed"] {
            assert!(!blocks_typing(s), "{s}");
        }

        let dir = std::env::temp_dir().join(format!("waymux-hook-state-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(hook_state_in(&dir, "%7"), None);
        fs::write(
            dir.join("acpd-pane-state-_7.json"),
            r#"{"state":"working"}"#,
        )
        .unwrap();
        assert_eq!(hook_state_in(&dir, "%7").as_deref(), Some("working"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_dispatch_messages_name_the_pane() {
        let q1 = Dispatch::Queued { position: 1 }.message("%3");
        assert!(
            q1.contains("%3") && q1.contains("when the agent is at rest"),
            "{q1}"
        );
        let q3 = Dispatch::Queued { position: 3 }.message("%3");
        assert!(q3.contains("%3") && q3.contains("3 waiting"), "{q3}");
        let typed = Dispatch::Typed.message("%3");
        assert!(
            typed.contains("%3") && typed.contains("daemon not running"),
            "{typed}"
        );
        let refused = Dispatch::Refused("x".into()).message("%3");
        assert!(
            refused.contains("%3") && refused.contains("not sent") && refused.contains(": x"),
            "{refused}"
        );
    }

    #[test]
    fn test_dispatch_prompt_with_all_outcomes() {
        assert_eq!(
            dispatch_prompt_with(
                "%1",
                " \n\t\u{200B} ",
                "cli",
                |_, _| panic!("must not call rpc on empty"),
                |_| None,
                |_, _| false,
            ),
            Dispatch::Refused("empty prompt".into())
        );

        let mut seen = None;
        let out = dispatch_prompt_with(
            "%2",
            "run\u{200B} cargo test",
            "cockpit",
            |method, params| {
                seen = Some((method.to_string(), params.clone()));
                Ok(json!({ "position": 4 }))
            },
            |_| None,
            |_, _| false,
        );
        assert_eq!(out, Dispatch::Queued { position: 4 });
        let (m, p) = seen.unwrap();
        assert_eq!(m, "queue/push");
        assert_eq!(p["pane_id"], "%2");
        assert_eq!(p["text"], "run cargo test");
        assert_eq!(p["by"], "cockpit");

        // Missing position defaults to 1.
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Ok(json!({})),
                |_| None,
                |_, _| { false }
            ),
            Dispatch::Queued { position: 1 }
        );

        // Rejected codes map to friendly reasons.
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Err(CallError::Rejected("queue_full".into())),
                |_| None,
                |_, _| false,
            ),
            Dispatch::Refused("the queue is full (waymux queue ls)".into())
        );
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Err(CallError::Rejected("too_large".into())),
                |_| None,
                |_, _| false,
            ),
            Dispatch::Refused("the prompt is too large".into())
        );
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Err(CallError::Rejected("custom_err".into())),
                |_| None,
                |_, _| false,
            ),
            Dispatch::Refused("custom_err".into())
        );

        // Unreachable daemon consults hook state before typing.
        for state in ["working", "busy"] {
            assert_eq!(
                dispatch_prompt_with(
                    "%2",
                    "hi",
                    "cli",
                    |_, _| Err(CallError::Unreachable),
                    |_| Some(state.into()),
                    |_, _| panic!("must not type while mid-turn"),
                ),
                Dispatch::Refused(
                    "the agent is mid-turn and the daemon is not running to hold it".into()
                )
            );
        }
        for state in ["permission", "question", "awaiting_input"] {
            assert_eq!(
                dispatch_prompt_with(
                    "%2",
                    "hi",
                    "cli",
                    |_, _| Err(CallError::Unreachable),
                    |_| Some(state.into()),
                    |_, _| panic!("must not type while waiting on prompt"),
                ),
                Dispatch::Refused(
                    "the agent is waiting on a prompt and the daemon is not running to hold it"
                        .into()
                )
            );
        }

        // Idle hook state allows direct typing.
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Err(CallError::Unreachable),
                |_| Some("idle".into()),
                |pane, text| pane == "%2" && text == "hi",
            ),
            Dispatch::Typed
        );
        assert_eq!(
            dispatch_prompt_with(
                "%2",
                "hi",
                "cli",
                |_, _| Err(CallError::Unreachable),
                |_| None,
                |_, _| false,
            ),
            Dispatch::Refused("tmux could not type into the pane".into())
        );
    }

    #[test]
    fn test_run_cli_with_add_ls_rm_and_unknown() {
        let s = |items: &[&str]| items.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();

        // add missing pane or text -> 1
        assert_eq!(
            run_cli_with(
                &s(&["add"]),
                |_, _| unreachable!(),
                |_, _, _| unreachable!(),
                String::new
            ),
            1
        );
        assert_eq!(
            run_cli_with(
                &s(&["add", "%1", "   "]),
                |_, _| unreachable!(),
                |_, _, _| unreachable!(),
                String::new,
            ),
            1
        );
        // add queued -> 0, refused -> 1
        assert_eq!(
            run_cli_with(
                &s(&["push", "%1", "hello", "world"]),
                |_, _| unreachable!(),
                |pane, text, by| {
                    assert_eq!((pane, text, by), ("%1", "hello world", "cli"));
                    Dispatch::Queued { position: 1 }
                },
                String::new,
            ),
            0
        );
        assert_eq!(
            run_cli_with(
                &s(&["add", "%1", "hello"]),
                |_, _| unreachable!(),
                |_, _, _| Dispatch::Refused("nope".into()),
                String::new,
            ),
            1
        );

        // ls with and without pane filter, plus unreachable -> 1
        assert_eq!(
            run_cli_with(
                &[],
                |m, p| {
                    assert_eq!(m, "queue/list");
                    assert!(p.is_null());
                    Ok(json!({
                        "%2": [{ "id": 7, "status": "queued", "by": "cli", "text": "first line\nsecond" }]
                    }))
                },
                |_, _, _| unreachable!(),
                String::new,
            ),
            0
        );
        assert_eq!(
            run_cli_with(
                &s(&["list", "%2"]),
                |_, p| {
                    assert_eq!(p["pane_id"], "%2");
                    Ok(json!(null))
                },
                |_, _, _| unreachable!(),
                String::new,
            ),
            0
        );
        assert_eq!(
            run_cli_with(
                &s(&["ls"]),
                |_, _| Err(CallError::Unreachable),
                |_, _, _| unreachable!(),
                String::new,
            ),
            1
        );

        // rm with explicit pane + id, env pane fallback, empty pane, dropped == 0, unreachable
        assert_eq!(
            run_cli_with(
                &s(&["rm"]),
                |_, _| unreachable!(),
                |_, _, _| unreachable!(),
                String::new,
            ),
            1
        );
        assert_eq!(
            run_cli_with(
                &s(&["rm", "%3", "42"]),
                |m, p| {
                    assert_eq!(m, "queue/cancel");
                    assert_eq!(p["pane_id"], "%3");
                    assert_eq!(p["id"], 42);
                    Ok(json!({ "dropped": 1 }))
                },
                |_, _, _| unreachable!(),
                String::new,
            ),
            0
        );
        assert_eq!(
            run_cli_with(
                &s(&["cancel"]),
                |_, p| {
                    assert_eq!(p["pane_id"], "%9");
                    assert!(p.get("id").is_none());
                    Ok(json!({ "dropped": 0 }))
                },
                |_, _, _| unreachable!(),
                || "%9".into(),
            ),
            1
        );
        assert_eq!(
            run_cli_with(
                &s(&["rm", "%3"]),
                |_, _| Err(CallError::Unreachable),
                |_, _, _| unreachable!(),
                String::new,
            ),
            1
        );

        // unknown subcommand -> 1
        assert_eq!(
            run_cli_with(
                &s(&["bogus"]),
                |_, _| unreachable!(),
                |_, _, _| unreachable!(),
                String::new,
            ),
            1
        );
    }
}
