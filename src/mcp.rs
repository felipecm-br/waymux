//! Stdio MCP (Model Context Protocol) server for sandboxed agents (`waymux mcp`).
//!
//! Agents such as Codex run shell commands inside a network-isolated Linux sandbox that blocks
//! loopback (`127.0.0.1:4040`), while MCP servers spawned by the harness run on the host. This
//! module exposes only the two least-privilege inter-agent tools — `waymux_list` (read-only) and
//! `waymux_tell` (framed peer delivery via `agent/send`, rate-limited and agent-pane-only) — so
//! bidirectional messaging works without opening the agent's network sandbox.

use crate::inject::is_agent_command;
use crate::rpc_client::{CallError, fetch_agent_states, rpc_call};
use crate::verbs::{PaneRow, Resolved, resolve_in};
use serde_json::{Value, json};
use std::io::{BufRead, Write};

const DEFAULT_PROTOCOL: &str = "2024-11-05";
const FIELD: char = '\x1f';

#[derive(Debug, Clone, PartialEq, Eq)]
struct LivePane {
    id: String,
    session: String,
    window_index: String,
    pane_index: String,
    window_name: String,
    active: bool,
    command: String,
    path: String,
}

impl LivePane {
    fn address(&self) -> String {
        format!("{}:{}.{}", self.session, self.window_index, self.pane_index)
    }

    fn as_pane_row(&self) -> PaneRow {
        PaneRow {
            id: self.id.clone(),
            address: self.address(),
            window_name: self.window_name.clone(),
            active: self.active,
        }
    }
}

fn parse_live_panes(out: &str) -> Vec<LivePane> {
    out.lines()
        .filter_map(|line| {
            let mut f = line.split(FIELD);
            let id = f.next()?.to_string();
            if id.is_empty() {
                return None;
            }
            Some(LivePane {
                id,
                session: f.next()?.to_string(),
                window_index: f.next()?.to_string(),
                pane_index: f.next()?.to_string(),
                window_name: f.next()?.to_string(),
                active: f.next()? == "1",
                command: f.next()?.trim().to_string(),
                path: f.next()?.trim().to_string(),
            })
        })
        .collect()
}

fn list_live_panes() -> Option<Vec<LivePane>> {
    let format = format!(
        "#{{pane_id}}{FIELD}#{{session_name}}{FIELD}#{{window_index}}{FIELD}#{{pane_index}}{FIELD}\
         #{{window_name}}{FIELD}#{{pane_active}}{FIELD}#{{pane_current_command}}{FIELD}#{{pane_current_path}}"
    );
    crate::tmux_cmd::tmux_fields_out(&["list-panes", "-a", "-F", &format])
        .map(|out| parse_live_panes(&out))
}

fn valid_pane_id(text: &str) -> bool {
    text.strip_prefix('%')
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// Resolves the caller's own tmux pane id.
///
/// Prefers `$TMUX_PANE` when it matches the MCP server's working directory (or when tmux cannot
/// be queried, e.g. inside a jail). Falls back to the unique live agent pane in `cwd` when
/// `$TMUX_PANE` is unset or belongs to another directory (e.g. Codex's shared app-server).
fn resolve_sender_pane(
    env_pane: Option<&str>,
    cwd: Option<&str>,
    panes: Option<&[LivePane]>,
) -> Option<String> {
    let env_valid = env_pane.map(str::trim).filter(|p| valid_pane_id(p));
    let Some(panes) = panes else {
        return env_valid.map(str::to_string);
    };
    let cwd = cwd.map(str::trim).filter(|c| !c.is_empty());

    if let Some(pane) = env_valid
        && let Some(row) = panes.iter().find(|r| r.id == pane)
        && cwd.is_none_or(|c| row.path == c)
    {
        return Some(pane.to_string());
    }

    if let Some(dir) = cwd {
        let matches: Vec<&LivePane> = panes
            .iter()
            .filter(|r| r.path == dir && is_agent_command(&r.command))
            .collect();
        if matches.len() == 1 {
            return Some(matches[0].id.clone());
        }
    }

    env_valid.map(str::to_string)
}

fn resolve_target(target: &str, panes: Option<&[LivePane]>) -> Result<String, String> {
    let target = target.trim();
    if target.is_empty() {
        return Err("pane must not be empty".to_string());
    }
    let Some(panes) = panes else {
        return if valid_pane_id(target) {
            Ok(target.to_string())
        } else {
            Err(format!("no tmux server reachable to resolve '{target}'"))
        };
    };
    let rows: Vec<PaneRow> = panes.iter().map(LivePane::as_pane_row).collect();
    match resolve_in(&rows, target) {
        Resolved::Pane(id) => Ok(id),
        Resolved::NotFound => Err(format!("no pane matches '{target}'")),
        Resolved::Ambiguous(ids) => Err(format!(
            "'{target}' matches {} panes ({}); specify a pane id",
            ids.len(),
            ids.join(", ")
        )),
    }
}

fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "waymux_list",
                "description": "List active AI agent panes across all tmux sessions (pane_id, session, address, window, command, state, path) so you can identify a peer pane for waymux_tell.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": false
                }
            },
            {
                "name": "waymux_tell",
                "description": "Send a framed peer message to another AI agent running in a tmux pane (equivalent to `waymux tell <pane> <text>`). Prefer this tool over shell `waymux tell` inside a network sandbox. Messages wait in the target agent's delivery queue until it rests.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "pane": {
                            "type": "string",
                            "description": "Target pane id (e.g. '%0'), tmux address (e.g. 'hermes:0.0'), or window name."
                        },
                        "text": {
                            "type": "string",
                            "description": "Message text to send to the peer agent (max 4096 bytes)."
                        }
                    },
                    "required": ["pane", "text"],
                    "additionalProperties": false
                },
                "annotations": {
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": false
                }
            }
        ]
    })
}

fn tool_ok(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": false
    })
}

fn tool_err(text: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": true
    })
}

fn format_agent_list(
    panes: Option<&[LivePane]>,
    states: &std::collections::HashMap<String, crate::rpc_client::AgentPaneEntry>,
    self_pane: Option<&str>,
) -> Value {
    let mut rows = Vec::new();
    if let Some(panes) = panes {
        for p in panes {
            let daemon_state = states.get(&p.id).map(|e| e.state.as_str());
            let known_live = daemon_state.is_some_and(|s| s != "closed");
            if !known_live && !is_agent_command(&p.command) {
                continue;
            }
            rows.push(json!({
                "pane_id": p.id,
                "session": p.session,
                "address": p.address(),
                "window": p.window_name,
                "command": p.command,
                "state": daemon_state.unwrap_or("idle"),
                "path": p.path,
                "self": self_pane == Some(p.id.as_str()),
            }));
        }
    } else {
        let mut entries: Vec<_> = states.iter().filter(|(_, e)| e.state != "closed").collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        for (pane_id, entry) in entries {
            rows.push(json!({
                "pane_id": pane_id,
                "state": entry.state,
                "self": self_pane == Some(pane_id.as_str()),
            }));
        }
    }
    json!({
        "self_pane": self_pane,
        "agents": rows,
    })
}

fn handle_tell_with(
    args: &Value,
    env_pane: Option<&str>,
    cwd: Option<&str>,
    panes: Option<&[LivePane]>,
    rpc: impl FnOnce(&str, &Value) -> Result<Value, CallError>,
) -> Value {
    let Some(target) = args.get("pane").and_then(Value::as_str) else {
        return tool_err("missing required string argument 'pane'");
    };
    let Some(text) = args.get("text").and_then(Value::as_str) else {
        return tool_err("missing required string argument 'text'");
    };
    let Some(from) = resolve_sender_pane(env_pane, cwd, panes) else {
        return tool_err(
            "could not determine sender tmux pane (TMUX_PANE unset and no unique agent pane in cwd)",
        );
    };
    let to = match resolve_target(target, panes) {
        Ok(id) => id,
        Err(msg) => return tool_err(&msg),
    };
    let params = json!({ "from": from, "to": to, "text": text });
    match rpc("agent/send", &params) {
        Ok(res) => {
            let pos = res["position"].as_u64().unwrap_or(1);
            tool_ok(&format!(
                "queued for {to} from {from} (position {pos}); it is typed once that agent rests"
            ))
        }
        Err(CallError::Blocked) => tool_err(&format!(
            "connection to 127.0.0.1:{} was blocked by a sandbox",
            crate::rpc_client::daemon_port()
        )),
        Err(CallError::Unreachable) => tool_err(&format!(
            "waymux daemon is not running or unreachable on port {}",
            crate::rpc_client::daemon_port()
        )),
        Err(CallError::Rejected(code)) => {
            let why = match code.as_str() {
                "not_an_agent" => format!("no agent the daemon knows of runs in {to}"),
                "rate_limited" => format!(
                    "this pane sent {} messages in the last minute; wait before sending more",
                    crate::api::MAIL_RATE_MAX
                ),
                "self_message" => "cannot send a message to your own pane".to_string(),
                "too_large" => format!(
                    "the message is over {} bytes; put it in a file and send its path",
                    crate::api::MAIL_MAX_BYTES
                ),
                other => other.to_string(),
            };
            tool_err(&why)
        }
    }
}

fn handle_request_with(
    req: &Value,
    env_pane: Option<&str>,
    cwd: Option<&str>,
    list_panes: impl Fn() -> Option<Vec<LivePane>>,
    list_states: impl Fn() -> std::collections::HashMap<String, crate::rpc_client::AgentPaneEntry>,
    rpc: impl FnOnce(&str, &Value) -> Result<Value, CallError>,
) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let id = req.get("id");
    // JSON-RPC 2.0 notifications omit `id` and expect no reply.
    let id = match id {
        Some(v) if !v.is_null() => v.clone(),
        _ => return None,
    };

    let result = match method {
        "initialize" => {
            let proto = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_PROTOCOL);
            Ok(json!({
                "protocolVersion": proto,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "waymux",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tool_definitions()),
        "tools/call" => {
            let params = req.get("params").unwrap_or(&Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").unwrap_or(&Value::Null);
            match name {
                "waymux_list" => {
                    let panes = list_panes();
                    let states = list_states();
                    let self_pane = resolve_sender_pane(env_pane, cwd, panes.as_deref());
                    let summary =
                        format_agent_list(panes.as_deref(), &states, self_pane.as_deref());
                    let pretty = serde_json::to_string_pretty(&summary).unwrap_or_default();
                    Ok(tool_ok(&pretty))
                }
                "waymux_tell" => {
                    let panes = list_panes();
                    Ok(handle_tell_with(args, env_pane, cwd, panes.as_deref(), rpc))
                }
                _ => Err((-32602, format!("unknown tool '{name}'"))),
            }
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };

    Some(match result {
        Ok(res) => json!({ "jsonrpc": "2.0", "id": id, "result": res }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": code, "message": message }
        }),
    })
}

/// `waymux mcp`: stdio MCP server exposing `waymux_list` and `waymux_tell`.
pub fn run_cli(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("Usage: waymux mcp");
        return 2;
    }
    let env_pane = std::env::var("TMUX_PANE").ok();
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().to_string());
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Value>(trimmed) {
            Ok(req) => handle_request_with(
                &req,
                env_pane.as_deref(),
                cwd.as_deref(),
                list_live_panes,
                fetch_agent_states,
                |method, params| rpc_call(method, params, 3000),
            ),
            Err(err) => Some(json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": { "code": -32700, "message": format!("parse error: {err}") }
            })),
        };
        if let Some(resp) = resp
            && (serde_json::to_writer(&mut stdout, &resp).is_err()
                || writeln!(stdout).is_err()
                || stdout.flush().is_err())
        {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc_client::AgentPaneEntry;
    use std::collections::HashMap;

    fn pane(id: &str, session: &str, cmd: &str, path: &str) -> LivePane {
        LivePane {
            id: id.into(),
            session: session.into(),
            window_index: "0".into(),
            pane_index: "0".into(),
            window_name: session.into(),
            active: true,
            command: cmd.into(),
            path: path.into(),
        }
    }

    #[test]
    fn test_resolve_sender_pane_prefers_matching_env_then_unique_cwd_agent() {
        let panes = vec![
            pane("%0", "dotfiles", "agy", "/repo/dotfiles"),
            pane("%1", "hermes", "codex", "/repo/hermes"),
            pane("%2", "shell", "zsh", "/repo/hermes"),
        ];
        // Normal --no-daemon / host launch: TMUX_PANE matches cwd.
        assert_eq!(
            resolve_sender_pane(Some("%1"), Some("/repo/hermes"), Some(&panes)).as_deref(),
            Some("%1")
        );
        // Shared app-server inherited wrong TMUX_PANE (%0): unique agent in cwd wins.
        assert_eq!(
            resolve_sender_pane(Some("%0"), Some("/repo/hermes"), Some(&panes)).as_deref(),
            Some("%1")
        );
        // Unset TMUX_PANE with unique agent in cwd: resolves to %1 (ignores zsh in %2).
        assert_eq!(
            resolve_sender_pane(None, Some("/repo/hermes"), Some(&panes)).as_deref(),
            Some("%1")
        );
        // Two codex panes in same dir: TMUX_PANE disambiguates.
        let dup = vec![
            pane("%1", "hermes", "codex", "/repo/hermes"),
            pane("%3", "hermes", "codex", "/repo/hermes"),
        ];
        assert_eq!(
            resolve_sender_pane(Some("%3"), Some("/repo/hermes"), Some(&dup)).as_deref(),
            Some("%3")
        );
        // Inside a jail with no tmux server: keeps valid TMUX_PANE.
        assert_eq!(
            resolve_sender_pane(Some("%7"), Some("/repo/hermes"), None).as_deref(),
            Some("%7")
        );
    }

    #[test]
    fn test_mcp_initialize_list_and_tell_dispatch() {
        let panes = vec![
            pane("%0", "dotfiles", "agy", "/repo/dotfiles"),
            pane("%1", "hermes", "codex", "/repo/hermes"),
            pane("%2", "term", "zsh", "/repo/hermes"),
        ];
        let mut states = HashMap::new();
        states.insert(
            "%0".into(),
            AgentPaneEntry {
                state: "working".into(),
                ..Default::default()
            },
        );

        let init = handle_request_with(
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),
            Some("%1"),
            Some("/repo/hermes"),
            || Some(panes.clone()),
            || states.clone(),
            |_, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["result"]["serverInfo"]["name"], "waymux");

        // Notifications get no reply.
        assert!(
            handle_request_with(
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                Some("%1"),
                Some("/repo/hermes"),
                || Some(panes.clone()),
                || states.clone(),
                |_, _| unreachable!(),
            )
            .is_none()
        );

        // tools/list returns both tools.
        let tools = handle_request_with(
            &json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            Some("%1"),
            Some("/repo/hermes"),
            || Some(panes.clone()),
            || states.clone(),
            |_, _| unreachable!(),
        )
        .unwrap();
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, vec!["waymux_list", "waymux_tell"]);

        // waymux_list omits zsh (%2) and includes both tracked %0 and cold-start codex %1.
        let listed = handle_request_with(
            &json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"waymux_list","arguments":{}}}),
            Some("%1"),
            Some("/repo/hermes"),
            || Some(panes.clone()),
            || states.clone(),
            |_, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(listed["result"]["isError"], false);
        let body: Value =
            serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(body["self_pane"], "%1");
        assert_eq!(body["agents"].as_array().unwrap().len(), 2);

        // waymux_tell resolves target and forwards to agent/send.
        let mut called = None;
        let sent = handle_request_with(
            &json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"waymux_tell","arguments":{"pane":"dotfiles:0.0","text":"pong"}}}),
            None,
            Some("/repo/hermes"),
            || Some(panes.clone()),
            || states.clone(),
            |method, params| {
                called = Some((method.to_string(), params.clone()));
                Ok(json!({ "id": 1, "position": 1 }))
            },
        )
        .unwrap();
        assert_eq!(sent["result"]["isError"], false);
        let (m, p) = called.unwrap();
        assert_eq!(m, "agent/send");
        assert_eq!(p, json!({ "from": "%1", "to": "%0", "text": "pong" }));
    }
}
