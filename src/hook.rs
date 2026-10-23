use crate::icons::{self, Registry};
use crate::inject::sanitize_workspace_key;
use crate::rpc_client::{http_get_json, http_post_json, post_agent_status};
use crate::tmux_cmd::{tmux_out, tmux_status};
use std::fs;
use std::io::{self, Read};
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn is_stdin_tty() -> bool {
    unsafe { nix::libc::isatty(io::stdin().as_raw_fd()) == 1 }
}

fn read_stdin_json() -> serde_json::Value {
    if is_stdin_tty() {
        return serde_json::json!({});
    }
    let mut input = String::new();
    let _ = io::stdin().read_to_string(&mut input);
    if input.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&input).unwrap_or_else(|_| serde_json::json!({}))
    }
}

pub fn get_active_tmux_pane() -> String {
    if let Ok(p) = std::env::var("TMUX_PANE")
        && !p.trim().is_empty()
    {
        return p.trim().to_string();
    }
    if let Some(p) = tmux_out(&["display-message", "-p", "#{pane_id}"])
        && !p.trim().is_empty()
    {
        return p;
    }
    if let Some(panes) = tmux_out(&[
        "list-panes",
        "-a",
        "-F",
        "#{pane_id} #{pane_current_command}",
    ]) {
        for line in panes.lines() {
            if (line.contains("agy")
                || line.contains("devin")
                || line.contains("claude")
                || line.contains("codex")
                || line.contains("opencode")
                || line.contains("node"))
                && let Some(pid) = line.split_whitespace().next()
            {
                return pid.to_string();
            }
        }
    }
    String::new()
}

fn resolve_window_icon<'a>(
    reg: &'a Registry,
    pane: &str,
    ctx: &serde_json::Value,
    agent_hint: Option<&str>,
) -> &'a str {
    // The agent running in the pane beats the hint: Devin imports ~/.claude/settings.json, so
    // Claude's hooks (hinted "claude") also fire inside Devin windows.
    if !pane.is_empty()
        && let Some(cmd) = tmux_out(&[
            "display-message",
            "-p",
            "-t",
            pane,
            "#{pane_current_command}",
        ])
        && crate::cockpit::items::is_agent_binary_name(cmd.trim())
        && let Some(glyph) = reg.program(cmd.trim())
    {
        return glyph;
    }
    let ctx_agent = ctx.get("agent").and_then(|v| v.as_str());
    if let Some(glyph) = [agent_hint, ctx_agent]
        .into_iter()
        .flatten()
        .find_map(|name| reg.program(name))
    {
        return glyph;
    }
    if ctx.get("hook_event_name").is_some() {
        reg.agent(icons::AGENT)
    } else {
        reg.program("agy").unwrap_or_default()
    }
}

fn set_ai_agent_title(pane: &str, title: Option<&str>) {
    if pane.is_empty() {
        return;
    }
    if let Some(t) = title {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            let clean: String = trimmed
                .chars()
                .filter(|c| !matches!(c, '"' | '\\' | '$' | '`'))
                .take(80)
                .collect();
            let _ = tmux_status(&["set-option", "-w", "-t", pane, "@ai_agent_title", &clean]);
            let _ = tmux_status(&["refresh-client", "-S"]);
            return;
        }
    }
    let _ = tmux_status(&["set-option", "-w", "-t", pane, "-u", "@ai_agent_title"]);
    let _ = tmux_status(&["refresh-client", "-S"]);
}

fn set_lazygitrs_icon(pane: &str, icon: Option<&str>) {
    if pane.is_empty() {
        return;
    }
    if let Some(ic) = icon {
        let _ = tmux_status(&["set-option", "-w", "-t", pane, "@lazygitrs_icon", ic]);
    } else {
        let _ = tmux_status(&["set-option", "-w", "-t", pane, "-u", "@lazygitrs_icon"]);
    }
    let _ = tmux_status(&["refresh-client", "-S"]);
}

/// First line of a transcript, at most 1 MiB. Transcripts grow to tens of MB and the hook
/// only needs the opening request, so it must not read the rest.
fn read_first_line(path: &Path) -> Option<String> {
    use std::io::BufRead;
    const MAX_FIRST_LINE: u64 = 1 << 20;
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut line = String::new();
    io::BufReader::new(file.take(MAX_FIRST_LINE))
        .read_line(&mut line)
        .ok()?;
    Some(line)
}

pub fn extract_session_title(ctx: &serde_json::Value) -> Option<String> {
    if let Some(t) = ctx.get("title").and_then(|v| v.as_str())
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    if let Some(t) = ctx.get("sessionTitle").and_then(|v| v.as_str())
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_string());
    }
    if let Some(p) = ctx.get("prompt").and_then(|v| v.as_str()) {
        let mut raw = p.trim();
        if let Some(rest) = raw.strip_prefix('/')
            && let Some(idx) = rest.find(char::is_whitespace)
        {
            raw = rest[idx..].trim();
        }
        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            return Some(collapsed);
        }
    }
    if let Some(tp) = ctx
        .get("transcriptPath")
        .or_else(|| ctx.get("transcript_path"))
        .and_then(|v| v.as_str())
        && let Some(first_line) = read_first_line(Path::new(tp))
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&first_line)
        && let Some(raw_content) = parsed.get("content").and_then(|v| v.as_str())
    {
        let mut raw = if let Some(start) = raw_content.find("<USER_REQUEST>")
            && let Some(end) = raw_content.find("</USER_REQUEST>")
            && start + 14 <= end
        {
            &raw_content[start + 14..end]
        } else {
            raw_content
        };
        raw = raw.trim();
        if let Some(rest) = raw.strip_prefix('/')
            && let Some(idx) = rest.find(char::is_whitespace)
        {
            raw = rest[idx..].trim();
        }
        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            return Some(collapsed);
        }
    }
    None
}

pub fn classify_pre_tool_state(ctx: &serde_json::Value) -> &'static str {
    let tool_name = ctx
        .get("toolCall")
        .and_then(|tc| tc.get("name"))
        .and_then(|n| n.as_str())
        .or_else(|| ctx.get("tool_name").and_then(|n| n.as_str()))
        .or_else(|| ctx.get("tool").and_then(|n| n.as_str()))
        .unwrap_or("")
        .to_ascii_lowercase();

    let is_ask = tool_name == "ask"
        || tool_name.starts_with("ask_")
        || tool_name.ends_with("_ask")
        || tool_name.contains("_ask_");

    if tool_name.contains("question") || is_ask {
        "awaiting_input"
    } else if tool_name.contains("permission") {
        "permission"
    } else {
        "working"
    }
}

fn write_pane_state_file(pane_id: &str, state: &str) {
    let safe_pane: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let data = serde_json::json!({
        "paneId": pane_id,
        "state": state,
        "message": serde_json::Value::Null,
        "timestamp": now_ms,
    });
    let file = std::env::temp_dir().join(format!("acpd-pane-state-{}.json", safe_pane));
    if let Ok(s) = serde_json::to_string(&data) {
        let _ = fs::write(file, s);
    }
}

/// Longest reason we keep; the Cockpit shows far less, the rest only bloats RPC payloads.
const MAX_REASON_CHARS: usize = 120;

/// One-line summary of the tool an agent is about to run (`Bash: cargo test -p auth`,
/// `Edit: main.rs`, `AskUserQuestion: Keep the v1 schema?`). Shown as the reason an agent waits.
pub fn tool_summary(ctx: &serde_json::Value) -> Option<String> {
    let name = ctx
        .get("tool_name")
        .and_then(|v| v.as_str())
        .or_else(|| {
            ctx.get("toolCall")
                .and_then(|tc| tc.get("name"))
                .and_then(|v| v.as_str())
        })
        .or_else(|| ctx.get("tool").and_then(|v| v.as_str()))
        .filter(|n| !n.trim().is_empty())?;
    let input = ctx.get("tool_input").or_else(|| {
        ctx.get("toolCall")
            .and_then(|tc| tc.get("args").or_else(|| tc.get("input")))
    });
    let detail = input.and_then(|i| {
        let field = |k: &str| i.get(k).and_then(|v| v.as_str()).map(str::to_string);
        field("command")
            .or_else(|| {
                field("file_path").or_else(|| field("path")).map(|p| {
                    Path::new(&p)
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or(p)
                })
            })
            .or_else(|| {
                i.get("questions")
                    .and_then(|qs| qs.get(0))
                    .and_then(|q| q.get("question"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .or_else(|| field("url"))
            .or_else(|| field("pattern"))
    });
    let detail = detail
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty());
    let summary = match detail {
        Some(d) => format!("{}: {}", name.trim(), d),
        None => name.trim().to_string(),
    };
    Some(summary.chars().take(MAX_REASON_CHARS).collect())
}

/// Per-pane memo of the last tool summary, so the permission prompt that follows a
/// PreToolUse can say what it is about. Lives in the private runtime dir (0700): commands can
/// contain secrets.
fn pane_tool_memo(pane_id: &str) -> Option<std::path::PathBuf> {
    let xdg = std::env::var("XDG_RUNTIME_DIR").ok()?;
    let safe_pane: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Some(
        std::path::PathBuf::from(xdg)
            .join("waymux")
            .join(format!("pane-tool-{}", safe_pane)),
    )
}

fn remember_pane_tool(pane_id: &str, summary: Option<&str>) {
    let Some(path) = pane_tool_memo(pane_id) else {
        return;
    };
    match summary {
        Some(s) if path.parent().is_some_and(|d| d.is_dir()) => {
            let _ = fs::write(path, s);
        }
        _ => {
            let _ = fs::remove_file(path);
        }
    }
}

fn recall_pane_tool(pane_id: &str) -> Option<String> {
    pane_tool_memo(pane_id)
        .and_then(|p| fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
}

fn send_acp_state(pane_id: &str, state: &str, message: Option<&str>) {
    if pane_id.is_empty() {
        return;
    }
    write_pane_state_file(pane_id, state);
    if !post_agent_status(pane_id, state, message) && state == "idle" {
        let _ = Command::new("ai-sound-notify")
            .arg("response")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// SessionStart fires before any turn, except `compact`, which can land mid-turn.
fn session_start_awaits_prompt(ctx: &serde_json::Value) -> bool {
    ctx.get("source").and_then(|v| v.as_str()) != Some("compact")
}

fn print_hook_response(event_type: &str, ctx: &serde_json::Value) {
    if event_type == "PreToolUse" && ctx.get("hook_event_name").is_none() {
        println!("{{\"decision\":\"allow\"}}");
    } else {
        println!("{{}}");
    }
}

fn run_tmux_hook(event_type: &str, agent_hint: Option<&str>) -> i32 {
    let tmux_pane = get_active_tmux_pane();
    let ctx = read_stdin_json();

    match event_type {
        "SessionStart" | "PreInvocation" | "UserPromptSubmit" => {
            if !tmux_pane.is_empty() {
                let icon = resolve_window_icon(Registry::global(), &tmux_pane, &ctx, agent_hint);
                let _ = tmux_status(&["rename-window", "-t", &tmux_pane, icon]);
            }
            if let Some(title) = extract_session_title(&ctx) {
                set_ai_agent_title(&tmux_pane, Some(&title));
            }
            if event_type != "SessionStart" {
                send_acp_state(&tmux_pane, "working", None);
            } else if session_start_awaits_prompt(&ctx) && !tmux_pane.is_empty() {
                // A fresh, resumed or cleared session sits at the prompt: no Stop follows until
                // a turn runs, so "working" would spin forever. Skip the offline chime fallback.
                write_pane_state_file(&tmux_pane, "idle");
                post_agent_status(&tmux_pane, "idle", None);
            }
        }
        "PreToolUse" => {
            let state = classify_pre_tool_state(&ctx);
            let summary = tool_summary(&ctx);
            remember_pane_tool(&tmux_pane, summary.as_deref());
            send_acp_state(&tmux_pane, state, summary.as_deref());
        }
        "PostToolUse" => {
            remember_pane_tool(&tmux_pane, None);
            send_acp_state(&tmux_pane, "working", None);
        }
        "PermissionRequest" | "Notification" => {
            // The prompt is about the tool announced by the preceding PreToolUse, when any.
            let reason = recall_pane_tool(&tmux_pane).or_else(|| {
                ctx.get("message")
                    .and_then(|m| m.as_str())
                    .map(|m| m.chars().take(MAX_REASON_CHARS).collect())
            });
            send_acp_state(&tmux_pane, "permission", reason.as_deref());
        }
        "Stop" => {
            remember_pane_tool(&tmux_pane, None);
            send_acp_state(&tmux_pane, "idle", None);
        }
        "SessionEnd" | "Exit" => {
            set_ai_agent_title(&tmux_pane, None);
            if !tmux_pane.is_empty() {
                let _ = tmux_status(&[
                    "set-window-option",
                    "-t",
                    &tmux_pane,
                    "automatic-rename",
                    "on",
                ]);
            }
            remember_pane_tool(&tmux_pane, None);
            send_acp_state(&tmux_pane, "closed", None);
        }
        _ => {}
    }

    print_hook_response(event_type, &ctx);
    0
}

fn run_lazygit_hook(event_type: &str) -> i32 {
    let tmux_pane = get_active_tmux_pane();
    let ctx = read_stdin_json();

    let conv_id = ctx
        .get("conversationId")
        .or_else(|| ctx.get("sessionId"))
        .or_else(|| ctx.get("session_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| std::env::var("AGY_CONVERSATION_ID").ok())
        .unwrap_or_default();

    let workspace_path = ctx
        .get("workspacePath")
        .or_else(|| ctx.get("cwd"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    if !tmux_pane.is_empty() && !workspace_path.is_empty() {
        let safe_path = sanitize_workspace_key(&workspace_path);
        let _ = fs::write(
            format!("/tmp/agy-active-pane-{}.txt", safe_path),
            &tmux_pane,
        );
    }

    if matches!(event_type, "SessionEnd" | "Exit") {
        let safe_path = sanitize_workspace_key(&workspace_path);
        let safe_conv = sanitize_workspace_key(&conv_id);
        let _ = fs::remove_file(format!(
            "/tmp/agy-registered-{}-{}.sentinel",
            safe_path, safe_conv
        ));
        set_lazygitrs_icon(&tmux_pane, None);
        println!("{{}}");
        return 0;
    }

    if !conv_id.is_empty() && !workspace_path.is_empty() {
        let safe_path = sanitize_workspace_key(&workspace_path);
        let safe_conv = sanitize_workspace_key(&conv_id);
        let sentinel = format!("/tmp/agy-registered-{}-{}.sentinel", safe_path, safe_conv);

        let mut initial_port = 47657u16;
        let port_file = Path::new(&workspace_path).join(".lazygitrs.port");
        if let Ok(ps) = fs::read_to_string(&port_file)
            && let Ok(p) = ps.trim().parse::<u16>()
        {
            initial_port = p;
        }

        if let Ok(stored) = fs::read_to_string(&sentinel)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&stored)
            && parsed.get("conversationId").and_then(|v| v.as_str()) == Some(conv_id.as_str())
        {
            let sport = parsed
                .get("port")
                .and_then(|v| v.as_u64())
                .map(|p| p as u16)
                .unwrap_or(initial_port);
            if let Some((200, sdata)) = http_get_json(sport, "/session-api/session", 50)
                && sdata.get("sessionId").and_then(|v| v.as_str()) == Some(conv_id.as_str())
            {
                set_lazygitrs_icon(&tmux_pane, Some("#[fg=#a6e3a1]#[fg=default]"));
                print_hook_response(event_type, &ctx);
                return 0;
            }
        }

        let exe_str = std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "waymux".to_string());
        let notify_cmd = format!("{} inject \"{}\" {{{{prompt}}}}", exe_str, workspace_path);

        if let Some((200, sdata)) = http_get_json(initial_port, "/session-api/session", 60) {
            let ws_match = sdata
                .get("workspacePath")
                .and_then(|v| v.as_str())
                .is_none_or(|w| w.is_empty() || w == workspace_path);
            if ws_match {
                let reg_payload = serde_json::json!({
                    "sessionId": conv_id,
                    "workspacePath": workspace_path,
                    "notifyCommand": notify_cmd,
                });
                let _ = http_post_json(
                    initial_port,
                    "/session-api/register",
                    None,
                    &reg_payload,
                    80,
                );
                let sentinel_val = serde_json::json!({
                    "conversationId": conv_id,
                    "port": initial_port,
                });
                let _ = fs::write(&sentinel, sentinel_val.to_string());
                set_lazygitrs_icon(&tmux_pane, Some("#[fg=#a6e3a1]#[fg=default]"));
            }
        }
    }

    print_hook_response(event_type, &ctx);
    0
}

pub fn run_cli(args: &[String]) -> i32 {
    let kind = args.first().map(|s| s.as_str()).unwrap_or("tmux");
    let event = args.get(1).map(|s| s.as_str()).unwrap_or("");
    let agent = args.get(2).map(|s| s.as_str());
    match kind {
        "lazygit" => run_lazygit_hook(event),
        _ => run_tmux_hook(event, agent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_start_awaits_prompt_except_compact() {
        for source in ["startup", "resume", "clear"] {
            assert!(session_start_awaits_prompt(
                &serde_json::json!({"source": source})
            ));
        }
        assert!(session_start_awaits_prompt(&serde_json::json!({})));
        assert!(!session_start_awaits_prompt(
            &serde_json::json!({"source": "compact"})
        ));
    }

    #[test]
    fn test_tool_summary_shapes() {
        let bash = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test\n  -p auth", "description": "Run tests"}
        });
        assert_eq!(
            tool_summary(&bash).as_deref(),
            Some("Bash: cargo test -p auth")
        );

        let edit = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "/repo/src/main.rs", "old_string": "a"}
        });
        assert_eq!(tool_summary(&edit).as_deref(), Some("Edit: main.rs"));

        let ask = serde_json::json!({
            "tool_name": "AskUserQuestion",
            "tool_input": {"questions": [{"question": "Keep the v1 schema?"}]}
        });
        assert_eq!(
            tool_summary(&ask).as_deref(),
            Some("AskUserQuestion: Keep the v1 schema?")
        );

        // agy-style payload, tool without recognized input.
        let agy = serde_json::json!({"toolCall": {"name": "view_file", "args": {"x": 1}}});
        assert_eq!(tool_summary(&agy).as_deref(), Some("view_file"));

        assert_eq!(tool_summary(&serde_json::json!({})), None);
    }

    #[test]
    fn test_tool_summary_is_bounded() {
        let long = serde_json::json!({
            "tool_name": "Bash",
            "tool_input": {"command": "x".repeat(500)}
        });
        assert_eq!(
            tool_summary(&long).unwrap().chars().count(),
            MAX_REASON_CHARS
        );
    }

    #[test]
    fn test_resolve_window_icon() {
        let reg = Registry::builtin();
        let ctx = serde_json::json!({});
        let icon = |agent| resolve_window_icon(&reg, "", &ctx, Some(agent));
        assert_eq!(icon("claude"), "");
        assert_eq!(icon("codex"), "");
        assert_eq!(icon("agy"), "");
        assert_eq!(icon("devin"), "");
        assert_eq!(icon("opencode"), "󰅪");
        let hook_ctx = serde_json::json!({"hook_event_name": "SessionStart"});
        assert_eq!(resolve_window_icon(&reg, "", &hook_ctx, Some("aider")), "󰚩");
    }

    #[test]
    fn test_classify_pre_tool_state() {
        let q = serde_json::json!({"toolCall": {"name": "ask_question"}});
        assert_eq!(classify_pre_tool_state(&q), "awaiting_input");

        let p = serde_json::json!({"toolCall": {"name": "request_permission"}});
        assert_eq!(classify_pre_tool_state(&p), "permission");

        let w = serde_json::json!({"toolCall": {"name": "view_file"}});
        assert_eq!(classify_pre_tool_state(&w), "working");
    }

    #[test]
    fn test_extract_session_title() {
        let ctx = serde_json::json!({"title": "Implement Waymux v0.2.0"});
        assert_eq!(
            extract_session_title(&ctx).as_deref(),
            Some("Implement Waymux v0.2.0")
        );

        let prompt_ctx = serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "prompt": "/plan   Refactor   the\n cockpit HUD  "
        });
        assert_eq!(
            extract_session_title(&prompt_ctx).as_deref(),
            Some("Refactor the cockpit HUD")
        );
    }

    #[test]
    fn test_session_title_from_transcript_first_line() {
        let dir = std::env::temp_dir().join(format!("waymux-hook-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("transcript.jsonl");
        let mut content = String::from(
            r#"{"content":"<USER_REQUEST>/plan  Fix the   flaky test</USER_REQUEST>"}"#,
        );
        content.push('\n');
        content.push_str(&"{\"filler\":true}\n".repeat(1000));
        fs::write(&path, content).unwrap();

        let ctx = serde_json::json!({ "transcript_path": path.to_str().unwrap() });
        assert_eq!(
            extract_session_title(&ctx).as_deref(),
            Some("Fix the flaky test")
        );
        // A directory or a missing file yields no title rather than an error.
        let dir_ctx = serde_json::json!({ "transcript_path": dir.to_str().unwrap() });
        assert_eq!(extract_session_title(&dir_ctx), None);
        let _ = fs::remove_dir_all(&dir);
    }
}
