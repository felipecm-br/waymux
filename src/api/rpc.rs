//! JSON-RPC 2.0 methods served on `/rpc`.

use super::state::{ApiState, IncomingUpdate, dispatch_update};
use crate::adapters::AgentState;
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub id: u64,
}

#[derive(Debug, Deserialize)]
pub struct StateUpdateParams {
    pub state: String,
    pub pane_id: String,
    pub timestamp: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DismissParams {
    pub pane_id: Option<String>,
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct WaitParams {
    pub pane_id: String,
    pub target_state: Option<String>,
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct SplitPaneParams {
    pub command: Option<String>,
    pub target_pane: Option<String>,
    pub vertical: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct NewWindowParams {
    pub name: Option<String>,
    pub target: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct NewSessionParams {
    pub name: String,
    pub directory: Option<String>,
    pub command: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct TargetParams {
    pub target: String,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct CapturePaneParams {
    pub target: Option<String>,
    pub target_pane: Option<String>,
    pub start_line: Option<i32>,
    pub end_line: Option<i32>,
    pub escape_sequences: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ListPanesParams {
    pub target: Option<String>,
    pub all: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ListWindowsParams {
    pub target: Option<String>,
    pub target_session: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ListSessionsParams {}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SendKeysParams {
    pub target: Option<String>,
    pub target_pane: Option<String>,
    #[serde(default)]
    pub keys: Vec<String>,
    pub literal: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SelectWindowParams {
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SelectPaneParams {
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct DisplayMessageParams {
    pub message: String,
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct RingBellParams {
    pub target: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct SelectLayoutParams {
    pub layout: String,
    pub target: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum RpcResponse {
    Success {
        jsonrpc: String,
        result: Value,
        id: u64,
    },
    Error {
        jsonrpc: String,
        error: RpcError,
        id: u64,
    },
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

pub(super) async fn handle_rpc(
    State(adapters): State<ApiState>,
    Json(payload): Json<RpcRequest>,
) -> Json<RpcResponse> {
    tracing::info!("Received RPC: method={} id={}", payload.method, payload.id);

    match payload.method.as_str() {
        "agentState/update" => match serde_json::from_value::<StateUpdateParams>(payload.params) {
            Ok(params) => match params.state.parse::<AgentState>() {
                Ok(state) => {
                    let update = IncomingUpdate {
                        pane_id: params.pane_id,
                        state,
                        message: None,
                        timestamp: params.timestamp,
                    };
                    dispatch_update(&adapters, update).await;
                    Json(RpcResponse::Success {
                        jsonrpc: "2.0".into(),
                        result: serde_json::json!("ok"),
                        id: payload.id,
                    })
                }
                Err(err_msg) => Json(RpcResponse::Error {
                    jsonrpc: "2.0".into(),
                    error: RpcError {
                        code: -32602,
                        message: err_msg,
                    },
                    id: payload.id,
                }),
            },
            Err(e) => json_invalid_params(e, payload.id),
        },
        "agentState/list" => {
            let states = adapters.pane_states.lock().await;
            let mut map = serde_json::Map::new();
            for (pane_id, info) in states.iter() {
                let mut pane_obj = serde_json::json!({
                    "state": info.state.as_str(),
                    "last_timestamp": info.last_timestamp,
                });
                if let Some(ref msg) = info.message {
                    pane_obj["message"] = serde_json::json!(msg);
                }
                if info.finished_at > 0 {
                    pane_obj["finished_at"] = serde_json::json!(info.finished_at);
                }
                map.insert(pane_id.clone(), pane_obj);
            }
            Json(RpcResponse::Success {
                jsonrpc: "2.0".into(),
                result: serde_json::Value::Object(map),
                id: payload.id,
            })
        }
        "agentState/dismiss" | "agent_state/dismiss" => {
            match parse_rpc_params::<DismissParams>(payload.params) {
                Ok(params) => {
                    let pane_id = params.pane_id.or(params.target).unwrap_or_default();
                    if pane_id.is_empty() {
                        return Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32602,
                                message: "Missing pane_id parameter".into(),
                            },
                            id: payload.id,
                        });
                    }

                    let current_state = {
                        let states = adapters.pane_states.lock().await;
                        states.get(&pane_id).map(|s| s.state.clone())
                    };

                    match current_state {
                        Some(AgentState::AwaitingInput)
                        | Some(AgentState::Permission)
                        | Some(AgentState::Stalled)
                        | Some(AgentState::Error) => {
                            tracing::info!("Dismissing attention state for pane {}", pane_id);
                            dispatch_update(
                                &adapters,
                                IncomingUpdate {
                                    pane_id: pane_id.clone(),
                                    state: AgentState::Idle,
                                    message: Some("dismissed by user focus".into()),
                                    timestamp: None,
                                },
                            )
                            .await;

                            let _ = crate::tmux_cmd::async_command()
                                .args(["set", "-g", "@ai_agent_bell", ""])
                                .output()
                                .await;
                            let _ = crate::tmux_cmd::async_command()
                                .args(["refresh-client", "-S"])
                                .output()
                                .await;

                            Json(RpcResponse::Success {
                                jsonrpc: "2.0".into(),
                                result: serde_json::json!({
                                    "status": "dismissed",
                                    "pane_id": pane_id,
                                }),
                                id: payload.id,
                            })
                        }
                        Some(AgentState::Working) => Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!({
                                "status": "ignored",
                                "pane_id": pane_id,
                                "reason": "agent_working",
                            }),
                            id: payload.id,
                        }),
                        _ => {
                            let _ = crate::tmux_cmd::async_command()
                                .args(["set", "-g", "@ai_agent_bell", ""])
                                .output()
                                .await;
                            Json(RpcResponse::Success {
                                jsonrpc: "2.0".into(),
                                result: serde_json::json!({
                                    "status": "noop",
                                    "pane_id": pane_id,
                                }),
                                id: payload.id,
                            })
                        }
                    }
                }
                Err(e) => json_invalid_params(e, payload.id),
            }
        }
        "agentState/wait" | "agent_state/wait" => {
            match parse_rpc_params::<WaitParams>(payload.params) {
                Ok(params) => {
                    if params.pane_id.is_empty() {
                        return Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32602,
                                message: "Missing pane_id parameter".into(),
                            },
                            id: payload.id,
                        });
                    }

                    let target_state = match params
                        .target_state
                        .as_deref()
                        .unwrap_or("idle")
                        .parse::<AgentState>()
                    {
                        Ok(s) => s,
                        Err(e) => {
                            return Json(RpcResponse::Error {
                                jsonrpc: "2.0".into(),
                                error: RpcError {
                                    code: -32602,
                                    message: format!("Invalid target_state: {}", e),
                                },
                                id: payload.id,
                            });
                        }
                    };

                    // Check if current state already matches target_state
                    {
                        let states = adapters.pane_states.lock().await;
                        if let Some(info) = states.get(&params.pane_id) {
                            if info.state == target_state {
                                return Json(RpcResponse::Success {
                                    jsonrpc: "2.0".into(),
                                    result: serde_json::json!({
                                        "status": "reached",
                                        "pane_id": params.pane_id,
                                        "state": target_state.as_str(),
                                    }),
                                    id: payload.id,
                                });
                            }
                        } else if target_state == AgentState::Idle {
                            return Json(RpcResponse::Success {
                                jsonrpc: "2.0".into(),
                                result: serde_json::json!({
                                    "status": "reached",
                                    "pane_id": params.pane_id,
                                    "state": "idle",
                                }),
                                id: payload.id,
                            });
                        }
                    }

                    let mut rx = adapters.state_broadcast.subscribe();
                    let pane_id = params.pane_id.clone();
                    let timeout_secs = params.timeout_secs.unwrap_or(300);

                    let wait_fut = async {
                        loop {
                            match rx.recv().await {
                                Ok(update) => {
                                    if update.pane_id == pane_id {
                                        if update.state == target_state {
                                            return Ok((
                                                "reached",
                                                update.state.as_str().to_string(),
                                            ));
                                        } else if update.state == AgentState::Closed {
                                            return Ok(("closed", "closed".to_string()));
                                        }
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    let states = adapters.pane_states.lock().await;
                                    if let Some(info) = states.get(&pane_id) {
                                        if info.state == target_state {
                                            return Ok((
                                                "reached",
                                                target_state.as_str().to_string(),
                                            ));
                                        } else if info.state == AgentState::Closed {
                                            return Ok(("closed", "closed".to_string()));
                                        }
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                    return Err("Broadcast channel closed".to_string());
                                }
                            }
                        }
                    };

                    match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_secs),
                        wait_fut,
                    )
                    .await
                    {
                        Ok(Ok((status, state_str))) => Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!({
                                "status": status,
                                "pane_id": pane_id,
                                "state": state_str,
                            }),
                            id: payload.id,
                        }),
                        Ok(Err(err)) => Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32000,
                                message: err,
                            },
                            id: payload.id,
                        }),
                        Err(_) => {
                            tracing::debug!(
                                "RPC agentState/wait connection timed out after {}s for pane {}",
                                timeout_secs,
                                pane_id
                            );
                            let current_state = {
                                let states = adapters.pane_states.lock().await;
                                states
                                    .get(&pane_id)
                                    .map(|s| s.state.as_str().to_string())
                                    .unwrap_or_else(|| "unknown".into())
                            };
                            Json(RpcResponse::Success {
                                jsonrpc: "2.0".into(),
                                result: serde_json::json!({
                                    "status": "timeout",
                                    "pane_id": pane_id,
                                    "current_state": current_state,
                                }),
                                id: payload.id,
                            })
                        }
                    }
                }
                Err(e) => json_invalid_params(e, payload.id),
            }
        }
        "tmux.split_pane" => {
            match serde_json::from_value::<SplitPaneParams>(payload.params) {
                Ok(params) => {
                    let mut cmd = crate::tmux_cmd::async_command();
                    cmd.arg("split-window");

                    if params.vertical.unwrap_or(false) {
                        cmd.arg("-h"); // tmux -h is vertical split
                    } else {
                        cmd.arg("-v"); // tmux -v is horizontal split
                    }

                    if let Some(target) = params.target_pane {
                        cmd.arg("-t").arg(target);
                    }

                    if let Some(command) = params.command {
                        cmd.arg(&command);
                    }

                    execute_tmux_cmd(cmd, payload.id, "pane created").await
                }
                Err(e) => Json(RpcResponse::Error {
                    jsonrpc: "2.0".into(),
                    error: RpcError {
                        code: -32602,
                        message: format!("Invalid params: {}", e),
                    },
                    id: payload.id,
                }),
            }
        }
        "tmux.new_window" => match serde_json::from_value::<NewWindowParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("new-window");
                if let Some(name) = params.name {
                    cmd.arg("-n").arg(name);
                }
                if let Some(target) = params.target {
                    cmd.arg("-t").arg(target);
                }
                if let Some(command) = params.command {
                    cmd.arg(&command);
                }
                execute_tmux_cmd(cmd, payload.id, "window created").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.new_session" => match serde_json::from_value::<NewSessionParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("new-session").arg("-d").arg("-s").arg(params.name);
                if let Some(dir) = params.directory {
                    cmd.arg("-c").arg(dir);
                }
                if let Some(command) = params.command {
                    cmd.arg(&command);
                }
                execute_tmux_cmd(cmd, payload.id, "session created").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.kill_pane" => match serde_json::from_value::<TargetParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("kill-pane").arg("-t").arg(params.target);
                execute_tmux_cmd(cmd, payload.id, "pane killed").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.kill_window" => match serde_json::from_value::<TargetParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("kill-window").arg("-t").arg(params.target);
                execute_tmux_cmd(cmd, payload.id, "window killed").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.kill_session" => match serde_json::from_value::<TargetParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("kill-session").arg("-t").arg(params.target);
                execute_tmux_cmd(cmd, payload.id, "session killed").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.capture_pane" => match parse_rpc_params::<CapturePaneParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("capture-pane").arg("-p");
                if let Some(target) = params.target.or(params.target_pane) {
                    cmd.arg("-t").arg(target);
                }
                if let Some(start) = params.start_line {
                    cmd.arg("-S").arg(start.to_string());
                }
                if let Some(end) = params.end_line {
                    cmd.arg("-E").arg(end.to_string());
                }
                if params.escape_sequences.unwrap_or(false) {
                    cmd.arg("-e");
                }
                match cmd.output().await {
                    Ok(output) if output.status.success() => {
                        let content = String::from_utf8_lossy(&output.stdout).to_string();
                        Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!({ "content": content }),
                            id: payload.id,
                        })
                    }
                    Ok(output) => {
                        let err_msg = String::from_utf8_lossy(&output.stderr);
                        Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32000,
                                message: format!("Tmux error: {}", err_msg),
                            },
                            id: payload.id,
                        })
                    }
                    Err(e) => Json(RpcResponse::Error {
                        jsonrpc: "2.0".into(),
                        error: RpcError {
                            code: -32000,
                            message: format!("Failed to execute tmux: {}", e),
                        },
                        id: payload.id,
                    }),
                }
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.list_panes" => match parse_rpc_params::<ListPanesParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("list-panes");
                cmd.arg("-F").arg(
                    "#{pane_id}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_current_path}\t#{pane_current_command}",
                );
                if params.all.unwrap_or(false) {
                    cmd.arg("-a");
                } else if let Some(target) = params.target {
                    cmd.arg("-t").arg(target);
                }
                match cmd.output().await {
                    Ok(output) if output.status.success() => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let panes: Vec<serde_json::Value> = stdout
                            .lines()
                            .filter_map(|line| {
                                let parts: Vec<&str> = line.split('\t').collect();
                                if parts.len() >= 6 {
                                    Some(serde_json::json!({
                                        "pane_id": parts[0],
                                        "active": parts[1] == "1",
                                        "width": parts[2].parse::<u32>().unwrap_or(0),
                                        "height": parts[3].parse::<u32>().unwrap_or(0),
                                        "current_path": parts[4],
                                        "current_command": parts[5],
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!(panes),
                            id: payload.id,
                        })
                    }
                    Ok(output) => {
                        let err_msg = String::from_utf8_lossy(&output.stderr);
                        Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32000,
                                message: format!("Tmux error: {}", err_msg),
                            },
                            id: payload.id,
                        })
                    }
                    Err(e) => Json(RpcResponse::Error {
                        jsonrpc: "2.0".into(),
                        error: RpcError {
                            code: -32000,
                            message: format!("Failed to execute tmux: {}", e),
                        },
                        id: payload.id,
                    }),
                }
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.list_windows" => match parse_rpc_params::<ListWindowsParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("list-windows");
                cmd.arg("-F").arg(
                    "#{window_id}\t#{window_name}\t#{window_index}\t#{window_active}\t#{window_panes}",
                );
                if let Some(target) = params.target.or(params.target_session) {
                    cmd.arg("-t").arg(target);
                }
                match cmd.output().await {
                    Ok(output) if output.status.success() => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let windows: Vec<serde_json::Value> = stdout
                            .lines()
                            .filter_map(|line| {
                                let parts: Vec<&str> = line.split('\t').collect();
                                if parts.len() >= 5 {
                                    Some(serde_json::json!({
                                        "window_id": parts[0],
                                        "window_name": parts[1],
                                        "active": parts[3] == "1",
                                        "panes": parts[4].parse::<u32>().unwrap_or(0),
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!(windows),
                            id: payload.id,
                        })
                    }
                    Ok(output) => {
                        let err_msg = String::from_utf8_lossy(&output.stderr);
                        Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32000,
                                message: format!("Tmux error: {}", err_msg),
                            },
                            id: payload.id,
                        })
                    }
                    Err(e) => Json(RpcResponse::Error {
                        jsonrpc: "2.0".into(),
                        error: RpcError {
                            code: -32000,
                            message: format!("Failed to execute tmux: {}", e),
                        },
                        id: payload.id,
                    }),
                }
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.list_sessions" => match parse_rpc_params::<ListSessionsParams>(payload.params) {
            Ok(_) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("list-sessions");
                cmd.arg("-F")
                    .arg("#{session_id}\t#{session_name}\t#{session_windows}\t#{session_attached}");
                match cmd.output().await {
                    Ok(output) if output.status.success() => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let sessions: Vec<serde_json::Value> = stdout
                            .lines()
                            .filter_map(|line| {
                                let parts: Vec<&str> = line.split('\t').collect();
                                if parts.len() >= 4 {
                                    Some(serde_json::json!({
                                        "session_name": parts[1],
                                        "windows": parts[2].parse::<u32>().unwrap_or(0),
                                        "attached": parts[3] == "1",
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect();
                        Json(RpcResponse::Success {
                            jsonrpc: "2.0".into(),
                            result: serde_json::json!(sessions),
                            id: payload.id,
                        })
                    }
                    Ok(output) => {
                        let err_msg = String::from_utf8_lossy(&output.stderr);
                        Json(RpcResponse::Error {
                            jsonrpc: "2.0".into(),
                            error: RpcError {
                                code: -32000,
                                message: format!("Tmux error: {}", err_msg),
                            },
                            id: payload.id,
                        })
                    }
                    Err(e) => Json(RpcResponse::Error {
                        jsonrpc: "2.0".into(),
                        error: RpcError {
                            code: -32000,
                            message: format!("Failed to execute tmux: {}", e),
                        },
                        id: payload.id,
                    }),
                }
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.send_keys" => match serde_json::from_value::<SendKeysParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("send-keys");
                if let Some(target) = params.target.or(params.target_pane) {
                    cmd.arg("-t").arg(target);
                }
                if params.literal.unwrap_or(false) {
                    cmd.arg("-l");
                }
                for key in params.keys {
                    cmd.arg(&key);
                }
                execute_tmux_cmd(cmd, payload.id, "keys sent").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.select_window" => match parse_rpc_params::<SelectWindowParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("select-window");
                if let Some(target) = params.target {
                    cmd.arg("-t").arg(target);
                }
                execute_tmux_cmd(cmd, payload.id, "window selected").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.select_pane" => match parse_rpc_params::<SelectPaneParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("select-pane");
                if let Some(target) = params.target {
                    cmd.arg("-t").arg(target);
                }
                execute_tmux_cmd(cmd, payload.id, "pane selected").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.display_message" => {
            match serde_json::from_value::<DisplayMessageParams>(payload.params) {
                Ok(params) => {
                    let mut cmd = crate::tmux_cmd::async_command();
                    cmd.arg("display-message");
                    if let Some(target) = params.target {
                        cmd.arg("-t").arg(target);
                    }
                    cmd.arg(params.message);
                    execute_tmux_cmd(cmd, payload.id, "message displayed").await
                }
                Err(e) => json_invalid_params(e, payload.id),
            }
        }
        "tmux.ring_bell" => match parse_rpc_params::<RingBellParams>(payload.params) {
            Ok(params) => {
                let mut cmd = crate::tmux_cmd::async_command();
                cmd.arg("send-keys");
                if let Some(target) = params.target {
                    cmd.arg("-t").arg(target);
                }
                cmd.arg("C-g");
                execute_tmux_cmd(cmd, payload.id, "bell triggered").await
            }
            Err(e) => json_invalid_params(e, payload.id),
        },
        "tmux.select_layout" => {
            match serde_json::from_value::<SelectLayoutParams>(payload.params) {
                Ok(params) => {
                    let mut cmd = crate::tmux_cmd::async_command();
                    cmd.arg("select-layout");
                    if let Some(target) = params.target {
                        cmd.arg("-t").arg(target);
                    }
                    cmd.arg(params.layout);
                    execute_tmux_cmd(cmd, payload.id, "layout applied").await
                }
                Err(e) => json_invalid_params(e, payload.id),
            }
        }
        _ => Json(RpcResponse::Error {
            jsonrpc: "2.0".into(),
            error: RpcError {
                code: -32601,
                message: "Method not found".into(),
            },
            id: payload.id,
        }),
    }
}

async fn execute_tmux_cmd(
    mut cmd: tokio::process::Command,
    id: u64,
    success_msg: &str,
) -> Json<RpcResponse> {
    match cmd.output().await {
        Ok(output) if output.status.success() => Json(RpcResponse::Success {
            jsonrpc: "2.0".into(),
            result: serde_json::json!(success_msg),
            id,
        }),
        Ok(output) => {
            let err_msg = String::from_utf8_lossy(&output.stderr);
            Json(RpcResponse::Error {
                jsonrpc: "2.0".into(),
                error: RpcError {
                    code: -32000,
                    message: format!("Tmux error: {}", err_msg),
                },
                id,
            })
        }
        Err(e) => Json(RpcResponse::Error {
            jsonrpc: "2.0".into(),
            error: RpcError {
                code: -32000,
                message: format!("Failed to execute tmux: {}", e),
            },
            id,
        }),
    }
}

fn json_invalid_params(e: serde_json::Error, id: u64) -> Json<RpcResponse> {
    Json(RpcResponse::Error {
        jsonrpc: "2.0".into(),
        error: RpcError {
            code: -32602,
            message: format!("Invalid params: {}", e),
        },
        id,
    })
}

fn parse_rpc_params<T: serde::de::DeserializeOwned + Default>(
    params: Value,
) -> Result<T, serde_json::Error> {
    if params.is_null() {
        Ok(T::default())
    } else {
        serde_json::from_value(params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::test_support::TestAdapter;
    use std::sync::{Arc, Mutex as StdMutex};

    #[test]
    fn test_rpc_param_deserialization() {
        let capture_json = serde_json::json!({ "target_pane": "%1", "start_line": -100, "escape_sequences": true });
        let capture_params: CapturePaneParams = serde_json::from_value(capture_json).unwrap();
        assert_eq!(capture_params.target_pane, Some("%1".into()));
        assert_eq!(capture_params.start_line, Some(-100));
        assert_eq!(capture_params.escape_sequences, Some(true));

        let send_keys_json = serde_json::json!({ "target": "%2", "keys": ["ls -la", "Enter"] });
        let send_params: SendKeysParams = serde_json::from_value(send_keys_json).unwrap();
        assert_eq!(send_params.target, Some("%2".into()));
        assert_eq!(send_params.keys, vec!["ls -la", "Enter"]);

        let select_win_json = serde_json::json!({ "target": "@1" });
        let select_win_params: SelectWindowParams =
            serde_json::from_value(select_win_json).unwrap();
        assert_eq!(select_win_params.target, Some("@1".into()));

        let select_pane_json = serde_json::json!({ "target": "%1" });
        let select_pane_params: SelectPaneParams =
            serde_json::from_value(select_pane_json).unwrap();
        assert_eq!(select_pane_params.target, Some("%1".into()));

        let display_msg_json = serde_json::json!({ "message": "Hello", "target": "@0" });
        let display_msg_params: DisplayMessageParams =
            serde_json::from_value(display_msg_json).unwrap();
        assert_eq!(display_msg_params.message, "Hello");
        assert_eq!(display_msg_params.target, Some("@0".into()));

        let ring_bell_json = serde_json::json!({ "target": "%0" });
        let ring_bell_params: RingBellParams = serde_json::from_value(ring_bell_json).unwrap();
        assert_eq!(ring_bell_params.target, Some("%0".into()));

        let layout_json = serde_json::json!({ "layout": "even-horizontal", "target": "@0" });
        let layout_params: SelectLayoutParams = serde_json::from_value(layout_json).unwrap();
        assert_eq!(layout_params.layout, "even-horizontal");
        assert_eq!(layout_params.target, Some("@0".into()));
    }

    #[tokio::test]
    async fn test_agent_state_dismiss_and_wait() {
        let updates = Arc::new(StdMutex::new(Vec::new()));
        let adapter = TestAdapter {
            updates: updates.clone(),
        };
        let state = ApiState::new(vec![Box::new(adapter)], 0);

        // 1. Set pane to AwaitingInput
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".to_string(),
                state: AgentState::AwaitingInput,
                message: None,
                timestamp: Some(100),
            },
        )
        .await;

        // Dismiss attention
        let dismiss_req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "agentState/dismiss".to_string(),
            params: serde_json::json!({ "pane_id": "%1" }),
            id: 1,
        };
        let res = handle_rpc(State(state.clone()), Json(dismiss_req)).await;
        match res.0 {
            RpcResponse::Success { result, .. } => {
                assert_eq!(result.get("status").unwrap(), "dismissed");
            }
            _ => panic!("Expected dismiss success"),
        }

        // Verify state is now Idle
        {
            let states = state.pane_states.lock().await;
            assert_eq!(states.get("%1").unwrap().state, AgentState::Idle);
        }

        // 2. Wait for idle when already idle should return immediately
        let wait_req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "agentState/wait".to_string(),
            params: serde_json::json!({ "pane_id": "%1", "target_state": "idle", "timeout_secs": 1 }),
            id: 2,
        };
        let wait_res = handle_rpc(State(state.clone()), Json(wait_req)).await;
        match wait_res.0 {
            RpcResponse::Success { result, .. } => {
                assert_eq!(result.get("status").unwrap(), "reached");
                assert_eq!(result.get("state").unwrap(), "idle");
            }
            _ => panic!("Expected wait success"),
        }

        // 3. When working, dismiss should be ignored
        dispatch_update(
            &state,
            IncomingUpdate {
                pane_id: "%1".to_string(),
                state: AgentState::Working,
                message: None,
                timestamp: Some(200),
            },
        )
        .await;

        let dismiss_working_req = RpcRequest {
            jsonrpc: "2.0".to_string(),
            method: "agentState/dismiss".to_string(),
            params: serde_json::json!({ "pane_id": "%1" }),
            id: 3,
        };
        let dis_work_res = handle_rpc(State(state.clone()), Json(dismiss_working_req)).await;
        match dis_work_res.0 {
            RpcResponse::Success { result, .. } => {
                assert_eq!(result.get("status").unwrap(), "ignored");
            }
            _ => panic!("Expected dismiss ignored"),
        }
    }
}
