//! JSON-RPC 2.0 methods served on `/rpc`.

use super::state::{ApiState, IncomingUpdate, dispatch_update};
use crate::adapters::AgentState;
use axum::{
    Json,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    /// A number, a string or null. A request without one is a notification and gets no reply.
    #[serde(default)]
    pub id: Value,
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
        id: Value,
    },
    Error {
        jsonrpc: String,
        error: RpcError,
        id: Value,
    },
}

#[derive(Debug, Serialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

const PARSE_ERROR: i32 = -32700;
const INVALID_REQUEST: i32 = -32600;
const INVALID_PARAMS: i32 = -32602;
const METHOD_NOT_FOUND: i32 = -32601;
const SERVER_ERROR: i32 = -32000;

type RpcResult = Result<Value, RpcError>;

impl RpcError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn invalid_params(e: serde_json::Error) -> Self {
        Self::new(INVALID_PARAMS, format!("Invalid params: {}", e))
    }
}

impl RpcResponse {
    fn from_result(id: Value, result: RpcResult) -> Self {
        match result {
            Ok(result) => RpcResponse::Success {
                jsonrpc: "2.0".into(),
                result,
                id,
            },
            Err(error) => RpcResponse::Error {
                jsonrpc: "2.0".into(),
                error,
                id,
            },
        }
    }
}

/// Serves `/rpc`: one request or a batch, per JSON-RPC 2.0. Malformed JSON and invalid
/// request objects get the spec's error objects instead of an HTTP rejection, and
/// notifications (no `id`) are run without a reply.
pub(super) async fn handle_rpc(State(state): State<ApiState>, body: Bytes) -> Response {
    let reply = |value: Value| Json(value).into_response();
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(e) => {
            let error = RpcError::new(PARSE_ERROR, format!("Parse error: {}", e));
            return reply(json!(RpcResponse::from_result(Value::Null, Err(error))));
        }
    };

    match message {
        Value::Array(batch) if batch.is_empty() => {
            let error = RpcError::new(INVALID_REQUEST, "Invalid Request: empty batch");
            reply(json!(RpcResponse::from_result(Value::Null, Err(error))))
        }
        Value::Array(batch) => {
            let mut responses = Vec::new();
            for message in batch {
                responses.extend(handle_message(&state, message).await);
            }
            if responses.is_empty() {
                StatusCode::NO_CONTENT.into_response()
            } else {
                reply(json!(responses))
            }
        }
        message => match handle_message(&state, message).await {
            Some(response) => reply(json!(response)),
            None => StatusCode::NO_CONTENT.into_response(),
        },
    }
}

/// Handles one request object; `None` for a notification.
async fn handle_message(state: &ApiState, message: Value) -> Option<RpcResponse> {
    let is_notification = message.is_object() && message.get("id").is_none();
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let request = match serde_json::from_value::<RpcRequest>(message) {
        Ok(request) if request.jsonrpc == "2.0" => request,
        Ok(_) => {
            let error = RpcError::new(INVALID_REQUEST, "Invalid Request: jsonrpc must be \"2.0\"");
            return Some(RpcResponse::from_result(id, Err(error)));
        }
        Err(e) => {
            let error = RpcError::new(INVALID_REQUEST, format!("Invalid Request: {}", e));
            return Some(RpcResponse::from_result(id, Err(error)));
        }
    };
    let response = respond(state, request).await;
    (!is_notification).then_some(response)
}

async fn respond(state: &ApiState, request: RpcRequest) -> RpcResponse {
    tracing::info!("Received RPC: method={} id={}", request.method, request.id);
    let result = call(state, &request.method, request.params).await;
    RpcResponse::from_result(request.id, result)
}

/// Runs one method. Methods whose params struct has defaults accept omitted params.
async fn call(state: &ApiState, method: &str, params: Value) -> RpcResult {
    match method {
        "agentState/update" => update_state(state, strict(params)?).await,
        "agentState/list" => Ok(list_states(state).await),
        "agentState/dismiss" | "agent_state/dismiss" => dismiss(state, lenient(params)?).await,
        "agentState/wait" | "agent_state/wait" => wait(state, lenient(params)?).await,
        "tmux.split_pane" => {
            let p: SplitPaneParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            // tmux -h splits side by side ("vertical" divider), -v stacks.
            cmd.arg("split-window").arg(if p.vertical.unwrap_or(false) {
                "-h"
            } else {
                "-v"
            });
            target(&mut cmd, p.target_pane);
            cmd.args(p.command);
            run(cmd, "pane created").await
        }
        "tmux.new_window" => {
            let p: NewWindowParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("new-window");
            if let Some(name) = p.name {
                cmd.arg("-n").arg(name);
            }
            target(&mut cmd, p.target);
            cmd.args(p.command);
            run(cmd, "window created").await
        }
        "tmux.new_session" => {
            let p: NewSessionParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.args(["new-session", "-d", "-s"]).arg(p.name);
            if let Some(dir) = p.directory {
                cmd.arg("-c").arg(dir);
            }
            cmd.args(p.command);
            run(cmd, "session created").await
        }
        "tmux.kill_pane" => kill(params, "kill-pane", "pane killed").await,
        "tmux.kill_window" => kill(params, "kill-window", "window killed").await,
        "tmux.kill_session" => kill(params, "kill-session", "session killed").await,
        "tmux.capture_pane" => {
            let p: CapturePaneParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.args(["capture-pane", "-p"]);
            target(&mut cmd, p.target.or(p.target_pane));
            if let Some(start) = p.start_line {
                cmd.arg("-S").arg(start.to_string());
            }
            if let Some(end) = p.end_line {
                cmd.arg("-E").arg(end.to_string());
            }
            if p.escape_sequences.unwrap_or(false) {
                cmd.arg("-e");
            }
            Ok(serde_json::json!({ "content": stdout(cmd).await? }))
        }
        "tmux.list_panes" => {
            let p: ListPanesParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.args([
                "list-panes",
                "-F",
                "#{pane_id}\t#{pane_active}\t#{pane_width}\t#{pane_height}\t#{pane_current_path}\t#{pane_current_command}",
            ]);
            if p.all.unwrap_or(false) {
                cmd.arg("-a");
            } else {
                target(&mut cmd, p.target);
            }
            Ok(rows(&stdout(cmd).await?, 6, |f| {
                serde_json::json!({
                    "pane_id": f[0],
                    "active": f[1] == "1",
                    "width": f[2].parse::<u32>().unwrap_or(0),
                    "height": f[3].parse::<u32>().unwrap_or(0),
                    "current_path": f[4],
                    "current_command": f[5],
                })
            }))
        }
        "tmux.list_windows" => {
            let p: ListWindowsParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.args([
                "list-windows",
                "-F",
                "#{window_id}\t#{window_name}\t#{window_index}\t#{window_active}\t#{window_panes}",
            ]);
            target(&mut cmd, p.target.or(p.target_session));
            Ok(rows(&stdout(cmd).await?, 5, |f| {
                serde_json::json!({
                    "window_id": f[0],
                    "window_name": f[1],
                    "active": f[3] == "1",
                    "panes": f[4].parse::<u32>().unwrap_or(0),
                })
            }))
        }
        "tmux.list_sessions" => {
            let _: ListSessionsParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.args([
                "list-sessions",
                "-F",
                "#{session_id}\t#{session_name}\t#{session_windows}\t#{session_attached}",
            ]);
            Ok(rows(&stdout(cmd).await?, 4, |f| {
                serde_json::json!({
                    "session_name": f[1],
                    "windows": f[2].parse::<u32>().unwrap_or(0),
                    "attached": f[3] == "1",
                })
            }))
        }
        "tmux.send_keys" => {
            let p: SendKeysParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("send-keys");
            target(&mut cmd, p.target.or(p.target_pane));
            if p.literal.unwrap_or(false) {
                cmd.arg("-l");
            }
            cmd.args(p.keys);
            run(cmd, "keys sent").await
        }
        "tmux.select_window" => {
            let p: SelectWindowParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("select-window");
            target(&mut cmd, p.target);
            run(cmd, "window selected").await
        }
        "tmux.select_pane" => {
            let p: SelectPaneParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("select-pane");
            target(&mut cmd, p.target);
            run(cmd, "pane selected").await
        }
        "tmux.display_message" => {
            let p: DisplayMessageParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("display-message");
            target(&mut cmd, p.target);
            cmd.arg(p.message);
            run(cmd, "message displayed").await
        }
        "tmux.ring_bell" => {
            let p: RingBellParams = lenient(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("send-keys");
            target(&mut cmd, p.target);
            cmd.arg("C-g");
            run(cmd, "bell triggered").await
        }
        "tmux.select_layout" => {
            let p: SelectLayoutParams = strict(params)?;
            let mut cmd = crate::tmux_cmd::async_command();
            cmd.arg("select-layout");
            target(&mut cmd, p.target);
            cmd.arg(p.layout);
            run(cmd, "layout applied").await
        }
        _ => Err(RpcError::new(METHOD_NOT_FOUND, "Method not found")),
    }
}

async fn update_state(state: &ApiState, params: StateUpdateParams) -> RpcResult {
    let agent_state = params
        .state
        .parse::<AgentState>()
        .map_err(|e| RpcError::new(INVALID_PARAMS, e))?;
    dispatch_update(
        state,
        IncomingUpdate {
            pane_id: params.pane_id,
            state: agent_state,
            message: None,
            timestamp: params.timestamp,
        },
    )
    .await;
    Ok(serde_json::json!("ok"))
}

async fn list_states(state: &ApiState) -> Value {
    let states = state.pane_states.lock().await;
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
    Value::Object(map)
}

async fn dismiss(state: &ApiState, params: DismissParams) -> RpcResult {
    let pane_id = params.pane_id.or(params.target).unwrap_or_default();
    if pane_id.is_empty() {
        return Err(RpcError::new(INVALID_PARAMS, "Missing pane_id parameter"));
    }

    let current_state = {
        let states = state.pane_states.lock().await;
        states.get(&pane_id).map(|s| s.state.clone())
    };

    match current_state {
        Some(AgentState::AwaitingInput)
        | Some(AgentState::Permission)
        | Some(AgentState::Stalled)
        | Some(AgentState::Error) => {
            tracing::info!("Dismissing attention state for pane {}", pane_id);
            dispatch_update(
                state,
                IncomingUpdate {
                    pane_id: pane_id.clone(),
                    state: AgentState::Idle,
                    message: Some("dismissed by user focus".into()),
                    timestamp: None,
                },
            )
            .await;
            clear_bell(true);
            Ok(serde_json::json!({ "status": "dismissed", "pane_id": pane_id }))
        }
        Some(AgentState::Working) => Ok(serde_json::json!({
            "status": "ignored",
            "pane_id": pane_id,
            "reason": "agent_working",
        })),
        _ => {
            clear_bell(false);
            Ok(serde_json::json!({ "status": "noop", "pane_id": pane_id }))
        }
    }
}

/// Clears the global bell in the background: `dismiss` runs on every pane focus, and the
/// reply should not wait for one or two more tmux processes.
fn clear_bell(refresh: bool) {
    tokio::spawn(async move {
        let _ = crate::tmux_cmd::async_command()
            .args(["set", "-g", "@ai_agent_bell", ""])
            .output()
            .await;
        if refresh {
            let _ = crate::tmux_cmd::async_command()
                .args(["refresh-client", "-S"])
                .output()
                .await;
        }
    });
}

async fn wait(state: &ApiState, params: WaitParams) -> RpcResult {
    if params.pane_id.is_empty() {
        return Err(RpcError::new(INVALID_PARAMS, "Missing pane_id parameter"));
    }

    let target_state = params
        .target_state
        .as_deref()
        .unwrap_or("idle")
        .parse::<AgentState>()
        .map_err(|e| RpcError::new(INVALID_PARAMS, format!("Invalid target_state: {}", e)))?;

    let pane_id = params.pane_id;
    let reached = |state: &str| serde_json::json!({ "status": "reached", "pane_id": pane_id, "state": state });

    // Already there; an unknown pane counts as idle.
    {
        let states = state.pane_states.lock().await;
        match states.get(&pane_id) {
            Some(info) if info.state == target_state => return Ok(reached(target_state.as_str())),
            None if target_state == AgentState::Idle => return Ok(reached("idle")),
            _ => {}
        }
    }

    let mut rx = state.state_broadcast.subscribe();
    let timeout_secs = params.timeout_secs.unwrap_or(300);

    let wait_fut = async {
        loop {
            match rx.recv().await {
                Ok(update) => {
                    if update.pane_id == pane_id {
                        if update.state == target_state {
                            return Ok(("reached", update.state.as_str().to_string()));
                        } else if update.state == AgentState::Closed {
                            return Ok(("closed", "closed".to_string()));
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    let states = state.pane_states.lock().await;
                    if let Some(info) = states.get(&pane_id) {
                        if info.state == target_state {
                            return Ok(("reached", target_state.as_str().to_string()));
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

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), wait_fut).await {
        Ok(Ok((status, state_str))) => Ok(serde_json::json!({
            "status": status,
            "pane_id": pane_id,
            "state": state_str,
        })),
        Ok(Err(err)) => Err(RpcError::new(SERVER_ERROR, err)),
        Err(_) => {
            tracing::debug!(
                "RPC agentState/wait connection timed out after {}s for pane {}",
                timeout_secs,
                pane_id
            );
            let current_state = {
                let states = state.pane_states.lock().await;
                states
                    .get(&pane_id)
                    .map(|s| s.state.as_str().to_string())
                    .unwrap_or_else(|| "unknown".into())
            };
            Ok(serde_json::json!({
                "status": "timeout",
                "pane_id": pane_id,
                "current_state": current_state,
            }))
        }
    }
}

async fn kill(params: Value, subcommand: &str, success_msg: &str) -> RpcResult {
    let p: TargetParams = strict(params)?;
    let mut cmd = crate::tmux_cmd::async_command();
    cmd.arg(subcommand).arg("-t").arg(p.target);
    run(cmd, success_msg).await
}

fn target(cmd: &mut tokio::process::Command, target: Option<String>) {
    if let Some(target) = target {
        cmd.arg("-t").arg(target);
    }
}

/// Runs tmux and returns its stdout, or the error tmux reported.
async fn stdout(mut cmd: tokio::process::Command) -> Result<String, RpcError> {
    match cmd.output().await {
        Ok(output) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        }
        Ok(output) => Err(RpcError::new(
            SERVER_ERROR,
            format!("Tmux error: {}", String::from_utf8_lossy(&output.stderr)),
        )),
        Err(e) => Err(RpcError::new(
            SERVER_ERROR,
            format!("Failed to execute tmux: {}", e),
        )),
    }
}

async fn run(cmd: tokio::process::Command, success_msg: &str) -> RpcResult {
    stdout(cmd).await.map(|_| serde_json::json!(success_msg))
}

/// Maps each tab-separated line with at least `min_cols` fields through `row`.
fn rows(stdout: &str, min_cols: usize, row: impl Fn(&[&str]) -> Value) -> Value {
    Value::Array(
        stdout
            .lines()
            .map(|line| line.split('\t').collect::<Vec<_>>())
            .filter(|fields| fields.len() >= min_cols)
            .map(|fields| row(&fields))
            .collect(),
    )
}

/// Params that must be present and well-formed.
fn strict<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(RpcError::invalid_params)
}

/// Params that may be omitted (`null` falls back to the defaults).
fn lenient<T: serde::de::DeserializeOwned + Default>(params: Value) -> Result<T, RpcError> {
    if params.is_null() {
        Ok(T::default())
    } else {
        strict(params)
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
            id: json!(1),
        };
        match respond(&state, dismiss_req).await {
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
            id: json!(2),
        };
        match respond(&state, wait_req).await {
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
            id: json!(3),
        };
        match respond(&state, dismiss_working_req).await {
            RpcResponse::Success { result, .. } => {
                assert_eq!(result.get("status").unwrap(), "ignored");
            }
            _ => panic!("Expected dismiss ignored"),
        }
    }

    async fn rpc(state: &ApiState, method: &str, params: Value) -> RpcResponse {
        let req = RpcRequest {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
            id: json!(7),
        };
        respond(state, req).await
    }

    fn error_code(res: RpcResponse) -> i32 {
        match res {
            RpcResponse::Error { error, id, .. } => {
                assert_eq!(id, json!(7));
                error.code
            }
            RpcResponse::Success { result, .. } => panic!("expected an error, got {result}"),
        }
    }

    #[tokio::test]
    async fn test_rpc_error_codes() {
        let state = ApiState::new(Vec::new(), 0);
        let unknown = rpc(&state, "tmux.nope", Value::Null).await;
        assert_eq!(error_code(unknown), METHOD_NOT_FOUND);

        // Strict params reject a missing object; lenient ones still need a pane.
        let missing = rpc(&state, "tmux.kill_pane", Value::Null).await;
        assert_eq!(error_code(missing), INVALID_PARAMS);
        let no_pane = rpc(&state, "agentState/dismiss", Value::Null).await;
        assert_eq!(error_code(no_pane), INVALID_PARAMS);

        let bad_state = rpc(
            &state,
            "agentState/update",
            serde_json::json!({ "pane_id": "%1", "state": "workin" }),
        )
        .await;
        assert_eq!(error_code(bad_state), INVALID_PARAMS);
        let bad_target = rpc(
            &state,
            "agentState/wait",
            serde_json::json!({ "pane_id": "%1", "target_state": "nope" }),
        )
        .await;
        assert_eq!(error_code(bad_target), INVALID_PARAMS);

        // Unit tests run tmux against a socket with no server behind it.
        let tmux_down = rpc(&state, "tmux.list_sessions", Value::Null).await;
        assert_eq!(error_code(tmux_down), SERVER_ERROR);
    }

    #[test]
    fn test_rows_skips_short_lines() {
        let out = rows(
            "%1\t1\n%2\n%3\t0\n",
            2,
            |f| serde_json::json!({ "id": f[0], "active": f[1] == "1" }),
        );
        assert_eq!(
            out,
            serde_json::json!([
                { "id": "%1", "active": true },
                { "id": "%3", "active": false },
            ])
        );
    }
}
