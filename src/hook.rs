use crate::icons::{self, Registry};
use crate::inject::sanitize_workspace_key;
use crate::rpc_client::{http_get_json, http_post_json, post_agent_status_with_session};
use crate::tmux_cmd::{tmux_out, tmux_status};
use std::fs;
use std::io::{self, IsTerminal, Read};
use std::path::Path;
use std::process::{Command, Stdio};

fn is_stdin_tty() -> bool {
    io::stdin().is_terminal()
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
    pane_cmd: Option<&str>,
    ctx: &serde_json::Value,
    agent_hint: Option<&str>,
) -> &'a str {
    // The agent running in the pane beats the hint: Devin imports ~/.claude/settings.json, so
    // Claude's hooks (hinted "claude") also fire inside Devin windows.
    if let Some(cmd) = pane_cmd
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
            let _ = tmux_status(&[
                "set-option",
                "-w",
                "-t",
                pane,
                "@ai_agent_title",
                &clean,
                ";",
                "refresh-client",
                "-S",
            ]);
            return;
        }
    }
    let _ = tmux_status(&[
        "set-option",
        "-w",
        "-t",
        pane,
        "-u",
        "@ai_agent_title",
        ";",
        "refresh-client",
        "-S",
    ]);
}

fn set_lazygitrs_icon(pane: &str, icon: Option<&str>) {
    if pane.is_empty() {
        return;
    }
    if let Some(ic) = icon {
        let _ = tmux_status(&[
            "set-option",
            "-w",
            "-t",
            pane,
            "@lazygitrs_icon",
            ic,
            ";",
            "refresh-client",
            "-S",
        ]);
    } else {
        let _ = tmux_status(&[
            "set-option",
            "-w",
            "-t",
            pane,
            "-u",
            "@lazygitrs_icon",
            ";",
            "refresh-client",
            "-S",
        ]);
    }
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

/// Whether a prompt is a message the agent harness wrapped in its own tag, such as Claude
/// Code's `<task-notification>…</task-notification>` (a background task finished) or
/// `<command-message>…</command-message>` (a slash command's body). Those arrive through
/// `UserPromptSubmit` like typed prompts. Only a tag whose name holds `-` or `_` and that is
/// closed later counts, so a typed prompt such as `<div> is broken` is still a title.
fn is_harness_message(prompt: &str) -> bool {
    let Some(rest) = prompt.trim_start().strip_prefix('<') else {
        return false;
    };
    let name_len = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..name_len];
    name.starts_with(|c: char| c.is_ascii_alphabetic())
        && name.contains(['-', '_'])
        && rest[name_len..].starts_with(['>', ' '])
        && rest.contains(&format!("</{name}>"))
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
        if is_harness_message(p) {
            // A task result or a slash command's body, not something the user typed:
            // keep the title the pane already has.
            return None;
        }
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
        let mut raw = raw_content
            .split_once("<USER_REQUEST>")
            .and_then(|(_, rest)| rest.split_once("</USER_REQUEST>"))
            .map_or(raw_content, |(inner, _)| inner);
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
    let now_ms = crate::sys::unix_ms();
    let data = serde_json::json!({
        "paneId": pane_id,
        "state": state,
        "message": serde_json::Value::Null,
        "timestamp": now_ms,
    });
    let file = std::env::temp_dir().join(format!("acpd-pane-state-{safe_pane}.json"));
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
    // Both parts come from the agent: collapse every run of whitespace, newlines included,
    // so the summary stays one line.
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = one_line(name);
    let detail = detail.map(|d| one_line(&d)).filter(|d| !d.is_empty());
    let summary = match detail {
        Some(d) => format!("{name}: {d}"),
        None => name,
    };
    Some(summary.chars().take(MAX_REASON_CHARS).collect())
}

/// Per-pane memo of the last tool summary, so the permission prompt that follows a
/// `PreToolUse` can say what it is about. Lives in the private runtime dir (0700): commands can
/// contain secrets.
fn pane_tool_memo_in(xdg: Option<&Path>, pane_id: &str) -> Option<std::path::PathBuf> {
    let xdg = xdg?;
    let safe_pane: String = pane_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Some(xdg.join("waymux").join(format!("pane-tool-{safe_pane}")))
}

fn remember_pane_tool_in(xdg: Option<&Path>, pane_id: &str, summary: Option<&str>) {
    let Some(path) = pane_tool_memo_in(xdg, pane_id) else {
        return;
    };
    match summary {
        Some(s) => {
            let _ = fs::write(path, s);
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

fn recall_pane_tool_in(xdg: Option<&Path>, pane_id: &str) -> Option<String> {
    pane_tool_memo_in(xdg, pane_id)
        .and_then(|p| fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
}

/// Longest shell command kept for the risk rules; the Cockpit's `y` reads it whole.
const MAX_COMMAND_BYTES: usize = 16 * 1024;

/// The raw shell command of a tool call (`tool_input.command`), newlines kept, so the risk
/// rules see every command of it rather than the one-line, cut summary.
pub fn tool_command(ctx: &serde_json::Value) -> Option<String> {
    let input = ctx.get("tool_input").or_else(|| {
        ctx.get("toolCall")
            .and_then(|tc| tc.get("args").or_else(|| tc.get("input")))
    })?;
    let cmd = input.get("command")?.as_str()?;
    if cmd.trim().is_empty() {
        return None;
    }
    let mut end = cmd.len().min(MAX_COMMAND_BYTES);
    while !cmd.is_char_boundary(end) {
        end -= 1;
    }
    Some(cmd[..end].to_string())
}

fn pane_command_memo_in(xdg: Option<&Path>, pane_id: &str) -> Option<std::path::PathBuf> {
    pane_tool_memo_in(xdg, pane_id).map(|p| {
        let name = p
            .file_name()
            .map(|f| f.to_string_lossy().replacen("pane-tool-", "pane-cmd-", 1))
            .unwrap_or_default();
        p.with_file_name(name)
    })
}

fn remember_pane_command(pane_id: &str, command: Option<&str>) {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    let Some(path) = pane_command_memo_in(xdg.as_deref().map(Path::new), pane_id) else {
        return;
    };
    match command {
        Some(c) => {
            use std::os::unix::fs::OpenOptionsExt;
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .mode(0o600)
                .open(&path)
            {
                let _ = std::io::Write::write_all(&mut f, c.as_bytes());
            }
        }
        None => {
            let _ = fs::remove_file(path);
        }
    }
}

/// The raw command of the tool call the pane last announced (`PreToolUse`) or asked
/// permission for, if it was a shell command.
pub fn recall_pane_command(pane_id: &str) -> Option<String> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    pane_command_memo_in(xdg.as_deref().map(Path::new), pane_id)
        .and_then(|p| fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
}

fn remember_pane_tool(pane_id: &str, summary: Option<&str>) {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    remember_pane_tool_in(xdg.as_deref().map(Path::new), pane_id, summary);
}

fn recall_pane_tool(pane_id: &str) -> Option<String> {
    let xdg = std::env::var_os("XDG_RUNTIME_DIR");
    recall_pane_tool_in(xdg.as_deref().map(Path::new), pane_id)
}

fn send_acp_state(pane_id: &str, state: &str, message: Option<&str>) {
    send_acp_state_with_session(pane_id, state, message, None);
}

/// In the command line of the shell Claude Code runs every Bash call in (foreground,
/// `run_in_background` and Monitor alike): `zsh -c source ~/.claude/shell-snapshots/…`.
const CLAUDE_TOOL_SHELL: &str = "/.claude/shell-snapshots/";

/// Longest background command kept as the `background` reason.
const MAX_BACKGROUND_CHARS: usize = 80;

/// One process: pid, parent pid and the command line with its arguments joined by spaces.
type Proc = (u32, u32, String);

/// Every process from `/proc`; empty where there is none.
fn process_table() -> Vec<Proc> {
    let Ok(dir) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.filter_map(|e| {
        let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // The command name can hold spaces and parentheses: the parent pid is the second
        // field after the last `)`.
        let parent = stat
            .rsplit_once(')')?
            .1
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let cmdline = String::from_utf8_lossy(&cmdline)
            .trim_end_matches('\0')
            .replace('\0', " ");
        Some((pid, parent, cmdline))
    })
    .collect()
}

fn is_claude_process(cmdline: &str) -> bool {
    let argv0 = cmdline.split(' ').next().unwrap_or_default();
    Path::new(argv0).file_name().is_some_and(|n| n == "claude")
        || cmdline.contains("@anthropic-ai/claude-code")
}

/// The command a Claude Code tool shell runs: the `eval '…'` argument, unquoted.
fn tool_shell_command(cmdline: &str) -> String {
    let cmd = cmdline
        .split_once("eval '")
        .map_or(cmdline, |(_, rest)| {
            rest.split_once("' < /dev/null").map_or(rest, |(c, _)| c)
        })
        .replace("'\\''", "'");
    let cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    if cmd.chars().count() > MAX_BACKGROUND_CHARS {
        let cut: String = cmd.chars().take(MAX_BACKGROUND_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        cmd
    }
}

/// Bash calls still running under the Claude Code process that ran this hook. At `Stop` the
/// turn's own calls have returned, so whatever is left runs in the background, and Claude
/// Code starts a new turn (ending in another `Stop`) when it exits.
///
/// Empty when the hook was not run by Claude Code itself: when something an agent ran (a test
/// suite, a nested shell) calls `waymux hook`, the way up to `claude` passes through a tool
/// shell, and that agent's background calls are not this pane's.
fn background_commands(procs: &[Proc], hook_pid: u32) -> Vec<String> {
    let find = |pid: u32| procs.iter().find(|p| p.0 == pid);
    let mut cursor = hook_pid;
    let agent = loop {
        let Some(&(_, ppid, _)) = find(cursor) else {
            return Vec::new();
        };
        if ppid <= 1 {
            return Vec::new();
        }
        match find(ppid) {
            Some((_, _, cmd)) if cmd.contains(CLAUDE_TOOL_SHELL) => return Vec::new(),
            Some((_, _, cmd)) if is_claude_process(cmd) => break ppid,
            Some(_) => cursor = ppid,
            None => return Vec::new(),
        }
    };
    procs
        .iter()
        .filter(|p| p.1 == agent && p.2.contains(CLAUDE_TOOL_SHELL))
        .map(|p| tool_shell_command(&p.2))
        .collect()
}

/// The `background` reason when Claude Code ends its turn with background Bash calls still
/// running, so the pane neither chimes done nor joins triage's review queue until they exit.
fn background_reason() -> Option<String> {
    let cmds = background_commands(&process_table(), std::process::id());
    let first = cmds.first()?;
    Some(match cmds.len() {
        1 => first.clone(),
        n => format!("{first} (+{} more)", n - 1),
    })
}

fn send_acp_state_with_session(
    pane_id: &str,
    state: &str,
    message: Option<&str>,
    session: Option<(&str, &str)>,
) {
    if pane_id.is_empty() {
        return;
    }
    write_pane_state_file(pane_id, state);
    if !post_agent_status_with_session(pane_id, state, message, session) && state == "idle" {
        let _ = Command::new("ai-sound-notify")
            .arg("response")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }
}

/// The harness and session id to resume this conversation with, for the hinted agents
/// `cockpit restore` can resume. Devin imports Claude's hooks with the `claude` hint, so the
/// restore side also checks how the pane was launched.
fn session_of<'a>(
    ctx: &'a serde_json::Value,
    agent_hint: Option<&'a str>,
) -> Option<(&'a str, &'a str)> {
    let agent = agent_hint.filter(|a| matches!(*a, "claude" | "codex"))?;
    let id = ctx.get("session_id").and_then(|v| v.as_str())?;
    Some((agent, id))
}

/// `SessionStart` fires before any turn, except `compact`, which can land mid-turn.
fn session_start_awaits_prompt(ctx: &serde_json::Value) -> bool {
    ctx.get("source").and_then(|v| v.as_str()) != Some("compact")
}

/// The one line a held prompt shows: the argument that decides the call. `None` for calls
/// the Cockpit cannot show whole (they are answered in the pane).
pub fn held_line(ctx: &serde_json::Value) -> Option<(String, String)> {
    let tool = ctx.get("tool_name")?.as_str()?;
    let input = ctx.get("tool_input")?;
    let field = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let line = match tool {
        "Bash" => {
            if input
                .get("dangerouslyDisableSandbox")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                return None;
            }
            field("command")?
        }
        "Read" => field("file_path")?,
        // A path widens what the call reads beyond the one line shown.
        "Glob" | "Grep" if input.get("path").is_none() => field("pattern")?,
        "WebFetch" => field("url")?,
        "WebSearch" => field("query")?,
        _ => return None,
    };
    Some((tool.to_string(), line))
}

/// Claude Code's `PermissionRequest` decision output for an answer.
pub fn decision_output(decision: &str) -> Option<String> {
    let behavior = match decision {
        "allow" => serde_json::json!({ "behavior": "allow" }),
        "deny" => serde_json::json!({
            "behavior": "deny",
            "message": "The user denied this from the waymux Cockpit.",
        }),
        _ => return None,
    };
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": behavior,
            }
        })
        .to_string(),
    )
}

/// Asks the daemon to hold the prompt and waits for the Cockpit's answer. `None` (print
/// nothing, so the harness asks in the pane) on every failure and when not held.
fn held_decision(pane: &str, ctx: &serde_json::Value) -> Option<String> {
    let (tool, line) = held_line(ctx)?;
    let params = serde_json::json!({ "pane_id": pane, "tool": tool, "line": line });
    // The daemon bounds the hold (at most 300 s); wait a little longer than that.
    let res = crate::rpc_client::rpc_call("approval/hold", &params, 305_000).ok()?;
    decision_output(res.get("decision")?.as_str()?)
}

/// What ended the turn, for the pane's error state: `usage limit` for a rate limit (the cue to
/// continue in another agent, `ai-quota` / `waymux launch`), else the API error's own name.
pub(crate) const USAGE_LIMIT: &str = "usage limit";

fn stop_failure_reason(ctx: &serde_json::Value) -> String {
    let error: String = ctx
        .get("error")
        .and_then(|e| e.as_str())
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(40)
        .collect();
    match error.as_str() {
        "rate_limit" => USAGE_LIMIT.to_string(),
        "" => "API error".to_string(),
        other => format!("API error: {}", other.replace('_', " ")),
    }
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
                let pane_cmd = tmux_out(&[
                    "display-message",
                    "-p",
                    "-t",
                    &tmux_pane,
                    "#{pane_current_command}",
                ]);
                let icon =
                    resolve_window_icon(Registry::global(), pane_cmd.as_deref(), &ctx, agent_hint);
                let _ = tmux_status(&["rename-window", "-t", &tmux_pane, icon]);
            }
            if let Some(title) = extract_session_title(&ctx) {
                set_ai_agent_title(&tmux_pane, Some(&title));
            }
            let session = session_of(&ctx, agent_hint);
            if event_type != "SessionStart" {
                send_acp_state_with_session(&tmux_pane, "working", None, session);
            } else if session_start_awaits_prompt(&ctx) && !tmux_pane.is_empty() {
                // A fresh, resumed or cleared session sits at the prompt: no Stop follows until
                // a turn runs, so "working" would spin forever. Skip the offline chime fallback.
                write_pane_state_file(&tmux_pane, "idle");
                post_agent_status_with_session(&tmux_pane, "idle", None, session);
            }
        }
        "PreToolUse" => {
            let state = classify_pre_tool_state(&ctx);
            let summary = tool_summary(&ctx);
            remember_pane_tool(&tmux_pane, summary.as_deref());
            remember_pane_command(&tmux_pane, tool_command(&ctx).as_deref());
            send_acp_state(&tmux_pane, state, summary.as_deref());
        }
        "PostToolUse" => {
            remember_pane_tool(&tmux_pane, None);
            remember_pane_command(&tmux_pane, None);
            send_acp_state(&tmux_pane, "working", None);
        }
        "PermissionRequest" | "Notification" => {
            if let Some(cmd) = tool_command(&ctx) {
                remember_pane_command(&tmux_pane, Some(&cmd));
            }
            // The prompt is about the tool announced by the preceding PreToolUse, when any.
            let reason = recall_pane_tool(&tmux_pane).or_else(|| {
                ctx.get("message")
                    .and_then(|m| m.as_str())
                    .map(|m| m.chars().take(MAX_REASON_CHARS).collect())
            });
            send_acp_state(&tmux_pane, "permission", reason.as_deref());
            if event_type == "PermissionRequest"
                && !tmux_pane.is_empty()
                && let Some(out) = held_decision(&tmux_pane, &ctx)
            {
                println!("{out}");
                return 0;
            }
        }
        "Stop" => {
            remember_pane_tool(&tmux_pane, None);
            remember_pane_command(&tmux_pane, None);
            match background_reason() {
                Some(reason) => send_acp_state(&tmux_pane, "background", Some(&reason)),
                None => send_acp_state(&tmux_pane, "idle", None),
            }
        }
        // Claude Code fires StopFailure instead of Stop when an API error ended the turn.
        "StopFailure" => {
            remember_pane_tool(&tmux_pane, None);
            remember_pane_command(&tmux_pane, None);
            send_acp_state(&tmux_pane, "error", Some(&stop_failure_reason(&ctx)));
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
            remember_pane_command(&tmux_pane, None);
            send_acp_state(&tmux_pane, "closed", None);
        }
        _ => {}
    }

    print_hook_response(event_type, &ctx);
    0
}

/// The command lazygitrs runs to hand review notes to this agent. lazygitrs fills in
/// `{{prompt}}` and runs it through a shell, so the binary and workspace are quoted words, never
/// spliced: a directory named `$(...)` or `a"b` must stay a path.
fn notify_command(exe: &str, workspace: &str) -> String {
    format!(
        "{} inject {} {{{{prompt}}}}",
        crate::sys::shell_quote(exe),
        crate::sys::shell_quote(workspace)
    )
}

fn run_lazygit_hook(event_type: &str) -> i32 {
    let tmux_pane = get_active_tmux_pane();
    let ctx = read_stdin_json();

    let conv_id = ctx
        .get("conversationId")
        .or_else(|| ctx.get("sessionId"))
        .or_else(|| ctx.get("session_id"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
        .or_else(|| std::env::var("AGY_CONVERSATION_ID").ok())
        .unwrap_or_default();

    let workspace_path = ctx
        .get("workspacePath")
        .or_else(|| ctx.get("cwd"))
        .and_then(|v| v.as_str())
        .map(std::string::ToString::to_string)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().to_string())
        })
        .unwrap_or_default();

    if !tmux_pane.is_empty()
        && !workspace_path.is_empty()
        && let Some(path) = crate::presets::active_pane_file(&workspace_path)
    {
        let _ = fs::write(path, &tmux_pane);
    }

    // Which lazygitrs port this conversation registered with, so a repeat hook can skip it.
    let sentinel = crate::presets::private_runtime_dir().map(|dir| {
        dir.join(format!(
            "lazygitrs-registered-{}-{}",
            sanitize_workspace_key(&workspace_path),
            sanitize_workspace_key(&conv_id)
        ))
    });

    if matches!(event_type, "SessionEnd" | "Exit") {
        if let Some(sentinel) = &sentinel {
            let _ = fs::remove_file(sentinel);
        }
        set_lazygitrs_icon(&tmux_pane, None);
        println!("{{}}");
        return 0;
    }

    if !conv_id.is_empty()
        && !workspace_path.is_empty()
        && let Some(sentinel) = sentinel
    {
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
                .and_then(serde_json::Value::as_u64)
                // A value past u16 would wrap to some other port; keep the default instead.
                .map_or(initial_port, |p| u16::try_from(p).unwrap_or(initial_port));
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
            .map_or_else(|| "waymux".to_string(), |p| p.to_string_lossy().to_string());
        let notify_cmd = notify_command(&exe_str, &workspace_path);

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
    let kind = args.first().map_or("tmux", std::string::String::as_str);
    let event = args.get(1).map_or("", std::string::String::as_str);
    let agent = args.get(2).map(std::string::String::as_str);
    match kind {
        "lazygit" => run_lazygit_hook(event),
        _ => run_tmux_hook(event, agent),
    }
}

#[cfg(test)]
mod tests {
    fn tool_shell(cmd: &str) -> String {
        format!(
            "/usr/bin/zsh -c source /home/u/.claude/shell-snapshots/snapshot-zsh-1-a.sh 2>/dev/null || true && eval '{cmd}' < /dev/null && pwd -P >| /tmp/claude-571e-cwd"
        )
    }

    /// tmux pane shell 10 > claude 20 > { hook shell 30 > waymux 31, tool shell 40 > cargo 41,
    /// MCP server 50 }.
    fn claude_tree(extra: &[super::Proc]) -> Vec<super::Proc> {
        let mut procs = vec![
            (10, 1, "-zsh".to_string()),
            (20, 10, "claude --model opus".to_string()),
            (30, 20, "/bin/sh -c waymux hook tmux Stop".to_string()),
            (31, 30, "waymux hook tmux Stop".to_string()),
            (50, 20, "node /usr/lib/mcp-server.js".to_string()),
        ];
        procs.extend_from_slice(extra);
        procs
    }

    #[test]
    fn test_background_commands_lists_tool_shells_left_under_claude() {
        let procs = claude_tree(&[
            (
                40,
                20,
                tool_shell("cargo mutants -F \"is_invisible.*true\""),
            ),
            (41, 40, "cargo mutants".to_string()),
        ]);
        assert_eq!(
            super::background_commands(&procs, 31),
            vec!["cargo mutants -F \"is_invisible.*true\""]
        );
    }

    #[test]
    fn test_background_commands_empty_when_the_turn_left_nothing_running() {
        assert!(super::background_commands(&claude_tree(&[]), 31).is_empty());
    }

    #[test]
    fn test_background_commands_ignores_tool_shells_of_an_outer_agent() {
        // Claude 20 runs inside a tool shell 15 of another Claude 5, which also has 16 running.
        let procs = vec![
            (5, 1, "claude".to_string()),
            (15, 5, tool_shell("claude --model opus")),
            (16, 5, tool_shell("sleep 300")),
            (20, 15, "claude --model opus".to_string()),
            (30, 20, "/bin/sh -c waymux hook tmux Stop".to_string()),
            (31, 30, "waymux hook tmux Stop".to_string()),
        ];
        assert!(super::background_commands(&procs, 31).is_empty());
    }

    #[test]
    fn test_background_commands_ignores_hooks_run_from_a_tool_shell() {
        // `cargo test` in Claude's tool shell 40 runs `waymux hook tmux Stop` while 45 runs
        // in the background: that hook belongs to the test's fake pane, not to this Claude.
        let procs = claude_tree(&[
            (40, 20, tool_shell("cargo test")),
            (41, 40, "cargo test".to_string()),
            (42, 41, "waymux hook tmux Stop claude".to_string()),
            (45, 20, tool_shell("sleep 300")),
        ]);
        assert!(super::background_commands(&procs, 42).is_empty());
    }

    #[test]
    fn test_background_commands_needs_a_claude_ancestor() {
        let procs = vec![
            (10, 1, "codex".to_string()),
            (30, 10, "waymux hook tmux Stop".to_string()),
            (40, 10, tool_shell("sleep 300")),
        ];
        assert!(super::background_commands(&procs, 30).is_empty());
    }

    #[test]
    fn test_tool_shell_command_unquotes_and_truncates() {
        assert_eq!(
            super::tool_shell_command(&tool_shell("echo '\\''hi'\\''; sleep 1")),
            "echo 'hi'; sleep 1"
        );
        let long = super::tool_shell_command(&tool_shell(&"x".repeat(200)));
        assert_eq!(long.chars().count(), super::MAX_BACKGROUND_CHARS);
        assert!(long.ends_with('…'));
    }

    #[test]
    fn test_is_claude_process_matches_native_and_npm_installs() {
        assert!(super::is_claude_process(
            "/home/u/.local/bin/claude --resume"
        ));
        assert!(super::is_claude_process(
            "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
        ));
        assert!(!super::is_claude_process("waymux hook tmux Stop claude"));
    }

    #[test]
    fn test_process_table_finds_this_process_and_its_parent() {
        let procs = super::process_table();
        let me = std::process::id();
        let Some(entry) = procs.iter().find(|p| p.0 == me) else {
            return; // no /proc on this platform
        };
        assert_eq!(entry.1, std::os::unix::process::parent_id());
    }

    #[test]
    fn test_stop_failure_names_a_usage_limit() {
        let reason = |v: serde_json::Value| stop_failure_reason(&v);
        assert_eq!(
            reason(serde_json::json!({"error": "rate_limit"})),
            USAGE_LIMIT
        );
        assert_eq!(
            reason(serde_json::json!({"error": "billing_error"})),
            "API error: billing error"
        );
        assert_eq!(reason(serde_json::json!({})), "API error");
        // Never more than a short word from the payload: no escapes, no long text.
        assert_eq!(
            reason(serde_json::json!({"error": "x\u{1b}[31m;rm -rf ~"})),
            "API error: x31mrmrf"
        );
    }

    #[test]
    fn test_session_of_only_for_resumable_hints() {
        let ctx = serde_json::json!({ "session_id": "0d5f3c1e-8a2b" });
        assert_eq!(
            session_of(&ctx, Some("claude")),
            Some(("claude", "0d5f3c1e-8a2b"))
        );
        assert_eq!(session_of(&ctx, Some("agy")), None);
        assert_eq!(session_of(&ctx, None), None);
        assert_eq!(session_of(&serde_json::json!({}), Some("claude")), None);
    }

    use super::*;

    #[test]
    fn test_held_line_only_for_one_line_calls() {
        let ctx = |tool: &str, input: serde_json::Value| serde_json::json!({ "tool_name": tool, "tool_input": input });
        assert_eq!(
            held_line(&ctx(
                "Bash",
                serde_json::json!({ "command": "go test ./..." })
            )),
            Some(("Bash".into(), "go test ./...".into()))
        );
        assert_eq!(
            held_line(&ctx(
                "Bash",
                serde_json::json!({ "command": "ls", "dangerouslyDisableSandbox": true })
            )),
            None
        );
        assert_eq!(
            held_line(&ctx(
                "Grep",
                serde_json::json!({ "pattern": "x", "path": "/" })
            )),
            None
        );
        assert_eq!(
            held_line(&ctx(
                "Read",
                serde_json::json!({ "file_path": "/etc/hosts" })
            )),
            Some(("Read".into(), "/etc/hosts".into()))
        );
        assert_eq!(
            held_line(&ctx("Edit", serde_json::json!({ "file_path": "a" }))),
            None
        );
    }

    #[test]
    fn test_decision_output_shape() {
        let allow: serde_json::Value =
            serde_json::from_str(&decision_output("allow").unwrap()).unwrap();
        assert_eq!(
            allow["hookSpecificOutput"]["hookEventName"],
            "PermissionRequest"
        );
        assert_eq!(allow["hookSpecificOutput"]["decision"]["behavior"], "allow");
        let deny: serde_json::Value =
            serde_json::from_str(&decision_output("deny").unwrap()).unwrap();
        assert_eq!(deny["hookSpecificOutput"]["decision"]["behavior"], "deny");
        assert!(decision_output("maybe").is_none());
    }

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
        // Found by fuzz/hook_input: a newline inside the tool name made a two-line reason.
        let odd = serde_json::json!({"tool_name": "Bash\nrm  -rf", "tool_input": {"command": "x"}});
        assert_eq!(tool_summary(&odd).as_deref(), Some("Bash rm -rf: x"));
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
        let icon = |agent| resolve_window_icon(&reg, None, &ctx, Some(agent));
        assert_eq!(icon("claude"), "");
        assert_eq!(icon("codex"), "");
        assert_eq!(icon("agy"), "");
        assert_eq!(icon("devin"), "");
        assert_eq!(icon("opencode"), "󰅪");
        assert_eq!(
            resolve_window_icon(&reg, Some("opencode"), &ctx, Some("claude")),
            "󰅪",
            "live pane command beats agent hint"
        );
        assert_eq!(
            resolve_window_icon(&reg, Some("zsh"), &ctx, Some("opencode")),
            "󰅪",
            "non-agent pane command falls back to hint"
        );
        let hook_ctx = serde_json::json!({"hook_event_name": "SessionStart"});
        assert_eq!(
            resolve_window_icon(&reg, None, &hook_ctx, Some("aider")),
            "󰚩"
        );
    }

    #[test]
    fn test_classify_pre_tool_state() {
        for tool in [
            "ask",
            "ask_user",
            "confirm_ask",
            "mcp_ask_tool",
            "user_question",
        ] {
            let q = serde_json::json!({"toolCall": {"name": tool}});
            assert_eq!(classify_pre_tool_state(&q), "awaiting_input", "{tool}");
        }

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

        let session_ctx = serde_json::json!({"sessionTitle": "  Triage queue  "});
        assert_eq!(
            extract_session_title(&session_ctx).as_deref(),
            Some("Triage queue")
        );

        let prompt_ctx = serde_json::json!({
            "sessionTitle": "   ",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "/plan   Refactor   the\n cockpit HUD  "
        });
        assert_eq!(
            extract_session_title(&prompt_ctx).as_deref(),
            Some("Refactor the cockpit HUD")
        );
    }

    #[test]
    fn test_harness_messages_are_not_titles() {
        for prompt in [
            "<task-notification> <task-id>b1</task-id> <status>completed</status> </task-notification>",
            "  <command-message>tui-ux-architect</command-message>\n<command-name>/tui-ux-architect</command-name>",
            "<system-reminder>\nnote\n</system-reminder>",
            "<local-command-stdout>ok</local-command-stdout>",
            "<user_prompt_submit_hook id=\"1\">x</user_prompt_submit_hook>",
        ] {
            let ctx = serde_json::json!({ "prompt": prompt, "transcript_path": "/nonexistent" });
            assert_eq!(extract_session_title(&ctx), None, "{prompt}");
        }
        // Typed prompts that merely start with markup are still titles.
        for (prompt, title) in [
            ("<div> is broken", "<div> is broken"),
            ("<my-widget> renders twice", "<my-widget> renders twice"),
            ("<a-b>x</a-bc> mismatched", "<a-b>x</a-bc> mismatched"),
            ("<-x->x</-x->", "<-x->x</-x->"),
            (
                "fix <task-notification> parsing",
                "fix <task-notification> parsing",
            ),
        ] {
            let ctx = serde_json::json!({ "prompt": prompt });
            assert_eq!(
                extract_session_title(&ctx).as_deref(),
                Some(title),
                "{prompt}"
            );
        }
    }

    #[test]
    fn test_remember_and_recall_pane_tool_in() {
        assert_eq!(pane_tool_memo_in(None, "%1"), None);
        let dir = std::env::temp_dir().join(format!("waymux-memo-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Missing waymux/ subdir is not created by remember_pane_tool_in.
        remember_pane_tool_in(Some(&dir), "%1", Some("Bash: ls"));
        assert_eq!(recall_pane_tool_in(Some(&dir), "%1"), None);

        fs::create_dir_all(dir.join("waymux")).unwrap();
        remember_pane_tool_in(Some(&dir), "%1", Some("Bash: ls"));
        assert_eq!(
            recall_pane_tool_in(Some(&dir), "%1").as_deref(),
            Some("Bash: ls")
        );

        remember_pane_tool_in(Some(&dir), "%1", Some("   "));
        assert_eq!(recall_pane_tool_in(Some(&dir), "%1"), None);

        remember_pane_tool_in(Some(&dir), "%1", Some("Edit: main.rs"));
        remember_pane_tool_in(Some(&dir), "%1", None);
        assert_eq!(recall_pane_tool_in(Some(&dir), "%1"), None);
        let _ = fs::remove_dir_all(&dir);
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

    mod properties {
        use super::*;
        use crate::test_strategies::{arb_json, arb_text};
        use proptest::prelude::*;

        proptest! {
            /// Hook stdin comes from whatever agent is running: any JSON must be handled
            /// without a panic, and the wait reason must stay within its bound.
            #[test]
            fn hook_input_never_panics(ctx in arb_json()) {
                let _ = extract_session_title(&ctx);
                let _ = classify_pre_tool_state(&ctx);
                let _ = session_start_awaits_prompt(&ctx);
                if let Some(summary) = tool_summary(&ctx) {
                    prop_assert!(summary.chars().count() <= MAX_REASON_CHARS);
                    prop_assert!(!summary.contains('\n'));
                }
            }

            /// A harness-wrapped message never becomes the title, whatever it wraps.
            #[test]
            fn harness_messages_keep_the_title(
                name in "[a-z]{1,8}[-_][a-z]{1,8}",
                body in arb_text(),
                lead in "[ \n]{0,3}",
            ) {
                let prompt = format!("{lead}<{name}>{body}</{name}>");
                let ctx = serde_json::json!({ "prompt": prompt });
                prop_assert_eq!(extract_session_title(&ctx), None);
            }

            #[test]
            fn titles_are_one_trimmed_line(prompt in arb_text()) {
                let ctx = serde_json::json!({ "prompt": prompt });
                if let Some(title) = extract_session_title(&ctx) {
                    prop_assert!(!title.is_empty());
                    prop_assert!(!title.contains("  "), "whitespace is collapsed");
                    prop_assert_eq!(title.trim(), title.as_str());
                    prop_assert!(!title.contains('\n'));
                }
            }
        }
    }

    #[test]
    fn test_notify_command_keeps_hostile_paths_as_one_word() {
        use crate::sys::shell_quote;
        for workspace in ["/src/a b", "/src/$(touch pwned)", "/src/a\"b", "/src/it's"] {
            let prompt = "fix `this`; $HOME";
            // lazygitrs fills {{prompt}} with a single-quoted word and runs the result with sh.
            let cmd = notify_command("f", workspace).replace("{{prompt}}", &shell_quote(prompt));
            let script = format!("f() {{ printf '%s\\n' \"$@\"; }}; {cmd}");
            let out = Command::new("sh").arg("-c").arg(&script).output().unwrap();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("inject\n{workspace}\n{prompt}\n"),
                "{cmd}"
            );
        }
    }
}
