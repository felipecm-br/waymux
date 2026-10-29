//! HTTP control plane: the authenticated `/rpc` and `/api/status` routes.

pub(crate) mod approvals;
pub mod hook_tokens;
pub mod phone;
mod queue;
mod rpc;
mod state;

pub use rpc::{RpcError, RpcRequest, RpcResponse};
pub use state::{ApiState, PaneStateInfo, Settings, StallRecord};

use crate::adapters::AgentState;
use axum::{Json, Router, extract::State, routing::post};
use rpc::handle_rpc;
use serde::{Deserialize, Serialize};
use state::{IncomingUpdate, dispatch_update};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct StatusRequest {
    pub agent: Option<String>,
    pub pane_id: String,
    pub state: String,
    pub message: Option<String>,
    pub timestamp: Option<u64>,
    /// The agent's own session id (`agent` names the harness), kept on the pane so
    /// `cockpit restore` can resume the conversation after a reboot.
    pub session: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub success: bool,
}

async fn handle_status(
    State(adapters): State<ApiState>,
    Json(payload): Json<StatusRequest>,
) -> Result<Json<StatusResponse>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    tracing::info!("Received REST status: {:?}", payload);
    match payload.state.parse::<AgentState>() {
        Ok(state) => {
            let update = IncomingUpdate {
                pane_id: payload.pane_id.clone(),
                state,
                message: payload.message.clone(),
                timestamp: payload.timestamp,
            };
            dispatch_update(&adapters, update).await;
            if let (Some(agent), Some(session)) = (&payload.agent, &payload.session) {
                tag_session(&payload.pane_id, agent, session).await;
            }
            Ok(Json(StatusResponse { success: true }))
        }
        Err(err_msg) => Err((
            axum::http::StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "success": false,
                "error": err_msg
            })),
        )),
    }
}

/// Harnesses whose sessions `cockpit restore` knows how to resume.
const RESUMABLE: [&str; 2] = ["claude", "codex"];

/// A session id that is safe to type back into a shell: a UUID or similar, nothing else.
fn valid_session(id: &str) -> bool {
    (8..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Sets the pane option `@ai_session` to `agent:id`. The daemon sets it rather than the hook
/// because a jailed hook cannot reach tmux; a hook token can only name its own pane.
async fn tag_session(pane: &str, agent: &str, id: &str) {
    if !hook_tokens::valid_pane(pane) || !RESUMABLE.contains(&agent) || !valid_session(id) {
        return;
    }
    let _ = crate::tmux_cmd::async_command()
        .args(["set-option", "-p", "-t", pane, "@ai_session"])
        .arg(format!("{agent}:{id}"))
        .output()
        .await;
}

/// Compares without an early exit, so response time does not reveal how much of a
/// guessed token matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone)]
pub struct AuthState {
    token: Arc<String>,
    hook_tokens: Arc<hook_tokens::HookTokens>,
    phone_token: Arc<phone::PhoneToken>,
}

/// What a token other than the daemon's may reach.
enum Scope {
    /// A sandboxed agent's hooks, for one pane.
    Hook(String),
    /// The phone surface: list agents, answer held prompts.
    Phone,
}

/// Largest body a hook token may send; a status or a held prompt is far smaller.
const HOOK_BODY_LIMIT: usize = 64 * 1024;

pub async fn auth_middleware(
    State(auth): State<AuthState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let bearer = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|b| b.trim().to_string());
    let Some(bearer) = bearer else {
        tracing::warn!("Unauthorized HTTP request to {}", req.uri().path());
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    };

    if constant_time_eq(bearer.as_bytes(), auth.token.as_bytes()) {
        return Ok(next.run(req).await);
    }
    let scope = if let Some(pane) = auth.hook_tokens.pane_of(&bearer) {
        Scope::Hook(pane)
    } else if auth.phone_token.matches(&bearer) {
        Scope::Phone
    } else {
        tracing::warn!("Unauthorized HTTP request to {}", req.uri().path());
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    };

    // A scoped token: read the body to check what it asks for, then pass it on.
    let path = req.uri().path().to_string();
    let (parts, body) = req.into_parts();
    let bytes = axum::body::to_bytes(body, HOOK_BODY_LIMIT)
        .await
        .map_err(|_| axum::http::StatusCode::PAYLOAD_TOO_LARGE)?;
    let json: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| axum::http::StatusCode::BAD_REQUEST)?;
    let allowed = match &scope {
        Scope::Hook(pane) => hook_tokens::allowed(&path, &json, pane),
        Scope::Phone => phone::allowed(&path, &json),
    };
    if !allowed {
        tracing::warn!("Scoped token refused on {path}");
        return Err(axum::http::StatusCode::FORBIDDEN);
    }
    let req = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
    Ok(next.run(req).await)
}

pub fn api_router(adapters: ApiState, auth_token: Arc<String>) -> Router {
    let auth = AuthState {
        token: auth_token,
        hook_tokens: adapters.hook_tokens.clone(),
        phone_token: adapters.phone_token.clone(),
    };
    let protected = Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/api/status", post(handle_status))
        .route_layer(axum::middleware::from_fn_with_state(auth, auth_middleware))
        .with_state(adapters);

    // The page carries no data; everything it shows comes through /rpc with the phone token.
    let page = Router::new().route(
        "/m",
        axum::routing::get(|| async {
            (
                [
                    (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
                    (axum::http::header::CACHE_CONTROL, "no-store"),
                ],
                phone::PAGE,
            )
        }),
    );

    Router::new().merge(protected).merge(page)
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::adapters::{AgentUpdate, OutputAdapter};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    /// Records every update it receives.
    pub(crate) struct TestAdapter {
        pub(crate) updates: Arc<Mutex<Vec<AgentUpdate>>>,
    }

    #[async_trait]
    impl OutputAdapter for TestAdapter {
        async fn update(&self, update: &AgentUpdate) -> anyhow::Result<()> {
            self.updates.lock().unwrap().push(update.clone());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_valid_session_takes_only_what_is_safe_to_type() {
        assert!(super::valid_session("0d5f3c1e-8a2b-4c6d-9e0f-1a2b3c4d5e6f"));
        assert!(super::valid_session("019a2b3c_session"));
        for id in [
            "",
            "short",
            "a b c d e f",
            "id;rm -rf ~",
            "$(reboot)xx",
            &"x".repeat(65),
        ] {
            assert!(!super::valid_session(id), "{id}");
        }
    }

    use super::*;

    #[tokio::test]
    async fn test_auth_middleware_rejects_missing_and_wrong_tokens() {
        use tower::ServiceExt;

        let token = Arc::new("secret-token-123".to_string());
        let state = ApiState::new(Vec::new(), 0);
        let body = r#"{"jsonrpc":"2.0","method":"agentState/list","id":1}"#;
        let call = |auth: Option<&'static str>| {
            let router = api_router(state.clone(), token.clone());
            async move {
                let mut req = axum::http::Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("content-type", "application/json");
                if let Some(value) = auth {
                    req = req.header("Authorization", value);
                }
                let req = req.body(axum::body::Body::from(body)).unwrap();
                router.oneshot(req).await.unwrap().status()
            }
        };

        assert_eq!(call(None).await, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(
            call(Some("Bearer secret-token-12")).await,
            axum::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(Some("secret-token-123")).await,
            axum::http::StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            call(Some("Bearer secret-token-123")).await,
            axum::http::StatusCode::OK
        );
    }

    #[tokio::test]
    async fn test_hook_token_reaches_only_its_own_pane() {
        use axum::http::StatusCode;
        use tower::ServiceExt;

        let state = ApiState::new(Vec::new(), 0);
        let send = |auth: String, uri: &'static str, body: String| {
            let router = api_router(state.clone(), Arc::new("daemon".into()));
            async move {
                let req = axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header("Authorization", format!("Bearer {auth}"))
                    .body(axum::body::Body::from(body))
                    .unwrap();
                let res = router.oneshot(req).await.unwrap();
                let status = res.status();
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
                )
            }
        };
        let rpc = |method: &str, pane: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{{"pane_id":"{pane}","tool":"Bash","line":"ls"}}}}"#
            )
        };
        let status = |pane: &str| format!(r#"{{"pane_id":"{pane}","state":"working"}}"#);

        let (code, minted) = send("daemon".into(), "/rpc", rpc("hook/token", "%7")).await;
        assert_eq!(code, StatusCode::OK);
        let hook = minted["result"]["token"].as_str().unwrap().to_string();

        // Its own pane: status and held prompts get through.
        assert_eq!(
            send(hook.clone(), "/api/status", status("%7")).await.0,
            StatusCode::OK
        );
        let (code, held) = send(hook.clone(), "/rpc", rpc("approval/hold", "%7")).await;
        assert_eq!(code, StatusCode::OK);
        assert!(held.get("result").is_some(), "{held}");

        // Anything else is refused before it runs.
        assert_eq!(
            send(hook.clone(), "/api/status", status("%8")).await.0,
            StatusCode::FORBIDDEN
        );
        for method in ["approval/hold", "queue/push", "agentState/update"] {
            let pane = if method == "approval/hold" {
                "%8"
            } else {
                "%7"
            };
            assert_eq!(
                send(hook.clone(), "/rpc", rpc(method, pane)).await.0,
                StatusCode::FORBIDDEN
            );
        }
        assert_eq!(
            send(hook.clone(), "/rpc", rpc("hook/token", "%7")).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            send(hook.clone(), "/rpc", "not json".into()).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            send("guess".into(), "/api/status", status("%7")).await.0,
            StatusCode::UNAUTHORIZED
        );

        // The daemon token still reaches everything, and a bad pane id mints nothing.
        assert_eq!(
            send("daemon".into(), "/api/status", status("%8")).await.0,
            StatusCode::OK
        );
        let (_, bad) = send("daemon".into(), "/rpc", rpc("hook/token", "api:1")).await;
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn test_phone_token_reads_and_answers_only() {
        use axum::http::StatusCode;
        use tower::ServiceExt;

        let state = ApiState::new(Vec::new(), 0);
        let send = |auth: Option<String>, method: &'static str, uri: &'static str, body: String| {
            let router = api_router(state.clone(), Arc::new("daemon".into()));
            async move {
                let mut req = axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("content-type", "application/json");
                if let Some(auth) = auth {
                    req = req.header("Authorization", format!("Bearer {auth}"));
                }
                let res = router
                    .oneshot(req.body(axum::body::Body::from(body)).unwrap())
                    .await
                    .unwrap();
                let status = res.status();
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                (status, String::from_utf8_lossy(&bytes).into_owned())
            }
        };
        let rpc = |method: &str| {
            format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"{method}","params":{{"pane_id":"%1","id":1,"decision":"deny","line":"ls","risk_ack":[]}}}}"#
            )
        };

        let (code, body) = send(Some("daemon".into()), "POST", "/rpc", rpc("phone/token")).await;
        assert_eq!(code, StatusCode::OK);
        let token = serde_json::from_str::<serde_json::Value>(&body).unwrap()["result"]["token"]
            .as_str()
            .unwrap()
            .to_string();
        let (_, again) = send(Some("daemon".into()), "POST", "/rpc", rpc("phone/token")).await;
        assert!(
            again.contains(&token),
            "issuing again keeps the paired token"
        );

        let (code, body) = send(Some(token.clone()), "POST", "/rpc", rpc("phone/snapshot")).await;
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains(r#""agents":[]"#), "{body}");
        // Reaches the handler: nothing is held, so the answer is refused there, not by auth.
        let (code, body) = send(Some(token.clone()), "POST", "/rpc", rpc("approval/answer")).await;
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("error"), "{body}");

        for method in [
            "queue/push",
            "tmux.send_keys",
            "phone/token",
            "hook/token",
            "agentState/list",
        ] {
            let (code, _) = send(Some(token.clone()), "POST", "/rpc", rpc(method)).await;
            assert_eq!(code, StatusCode::FORBIDDEN, "{method}");
        }
        let status = r#"{"pane_id":"%1","state":"idle"}"#.to_string();
        assert_eq!(
            send(Some(token.clone()), "POST", "/api/status", status)
                .await
                .0,
            StatusCode::FORBIDDEN
        );

        // The page needs no token and carries no data.
        let (code, page) = send(None, "GET", "/m", String::new()).await;
        assert_eq!(code, StatusCode::OK);
        assert!(page.contains("phone/snapshot") && !page.contains(&token));
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(constant_time_eq(b"", b""));
    }

    async fn post_rpc(body: &'static str) -> (axum::http::StatusCode, serde_json::Value) {
        use tower::ServiceExt;
        let router = api_router(ApiState::new(Vec::new(), 0), Arc::new("t".into()));
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/rpc")
            .header("Authorization", "Bearer t")
            .body(axum::body::Body::from(body))
            .unwrap();
        let res = router.oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn test_rpc_follows_json_rpc_2_0() {
        use axum::http::StatusCode;
        use serde_json::json;

        // String ids are echoed back as strings.
        let (status, res) =
            post_rpc(r#"{"jsonrpc":"2.0","method":"agentState/list","id":"abc"}"#).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(res, json!({ "jsonrpc": "2.0", "result": {}, "id": "abc" }));

        let (_, res) = post_rpc("{not json").await;
        assert_eq!(res["error"]["code"], -32700);
        assert_eq!(res["id"], serde_json::Value::Null);

        let (_, res) = post_rpc(r#"{"jsonrpc":"1.0","method":"agentState/list","id":4}"#).await;
        assert_eq!(res["error"]["code"], -32600);
        assert_eq!(res["id"], 4);
        let (_, res) = post_rpc(r#"{"jsonrpc":"2.0","id":5}"#).await;
        assert_eq!(res["error"]["code"], -32600);
        let (_, res) = post_rpc("[]").await;
        assert_eq!(res["error"]["code"], -32600);

        // A notification gets no reply, alone or inside a batch.
        let (status, _) = post_rpc(r#"{"jsonrpc":"2.0","method":"agentState/list"}"#).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (_, res) = post_rpc(
            r#"[{"jsonrpc":"2.0","method":"agentState/list","id":1},
                {"jsonrpc":"2.0","method":"agentState/list"},
                {"jsonrpc":"2.0","method":"nope","id":2}]"#,
        )
        .await;
        let batch = res.as_array().unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(batch[0]["id"], 1);
        assert_eq!(batch[1]["error"]["code"], -32601);
    }

    mod properties {
        use super::*;
        use crate::test_strategies::{arb_json, arb_text};
        use proptest::prelude::*;
        use serde_json::Value;

        /// Posts `body` to /rpc with a valid token on a fresh daemon state.
        fn post(body: Vec<u8>) -> (axum::http::StatusCode, Vec<u8>) {
            use tower::ServiceExt;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let router = api_router(ApiState::new(Vec::new(), 0), Arc::new("t".into()));
                let req = axum::http::Request::builder()
                    .method("POST")
                    .uri("/rpc")
                    .header("Authorization", "Bearer t")
                    .body(axum::body::Body::from(body))
                    .unwrap();
                let res = router.oneshot(req).await.unwrap();
                let status = res.status();
                let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                    .await
                    .unwrap();
                (status, bytes.to_vec())
            })
        }

        fn check_response(r: &Value) -> Result<(), TestCaseError> {
            prop_assert_eq!(&r["jsonrpc"], "2.0");
            let has_result = r.get("result").is_some();
            let has_error = r.get("error").is_some();
            prop_assert!(
                has_result != has_error,
                "exactly one of result/error: {}",
                r
            );
            if has_error {
                let code = r["error"]["code"].as_i64().unwrap();
                prop_assert!(
                    [-32700, -32600, -32601, -32602, -32000].contains(&code),
                    "{}",
                    r
                );
            }
            Ok(())
        }

        /// Whatever arrives, /rpc answers 200 with JSON-RPC response objects or 204.
        fn check(status: axum::http::StatusCode, body: &[u8]) -> Result<(), TestCaseError> {
            use axum::http::StatusCode;
            if status == StatusCode::NO_CONTENT {
                prop_assert!(body.is_empty());
                return Ok(());
            }
            prop_assert_eq!(status, StatusCode::OK);
            let value: Value = serde_json::from_slice(body).unwrap();
            match &value {
                Value::Array(batch) => {
                    prop_assert!(!batch.is_empty());
                    for r in batch {
                        check_response(r)?;
                    }
                }
                single => check_response(single)?,
            }
            Ok(())
        }

        fn arb_request() -> impl Strategy<Value = Value> {
            // Methods that return without blocking and without starting a tmux server.
            let method = prop::sample::select(vec![
                "agentState/update",
                "agentState/list",
                "agentState/dismiss",
                "tmux.list_panes",
                "tmux.capture_pane",
                "nope",
            ]);
            (
                prop::option::of(prop_oneof![Just("2.0".to_string()), arb_text()]),
                prop::option::of(method),
                arb_json(),
                prop::option::of(arb_json()),
            )
                .prop_map(|(jsonrpc, method, params, id)| {
                    let mut req = serde_json::Map::new();
                    if let Some(v) = jsonrpc {
                        req.insert("jsonrpc".into(), v.into());
                    }
                    if let Some(m) = method {
                        req.insert("method".into(), m.into());
                    }
                    req.insert("params".into(), params);
                    if let Some(id) = id {
                        req.insert("id".into(), id);
                    }
                    Value::Object(req)
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(128))]

            #[test]
            fn arbitrary_bytes_get_a_json_rpc_answer(body in prop::collection::vec(any::<u8>(), 0..256)) {
                let (status, out) = post(body);
                check(status, &out)?;
            }

            #[test]
            fn arbitrary_json_gets_a_json_rpc_answer(body in arb_json()) {
                let (status, out) = post(serde_json::to_vec(&body).unwrap());
                check(status, &out)?;
            }

            #[test]
            fn requests_and_batches_get_a_json_rpc_answer(
                batch in prop::collection::vec(arb_request(), 1..4),
                single in any::<bool>(),
            ) {
                let body = if single { batch[0].clone() } else { Value::Array(batch.clone()) };
                let (status, out) = post(serde_json::to_vec(&body).unwrap());
                check(status, &out)?;
                // One response per request, except valid notifications (no id). An invalid
                // request gets an error even without an id, as the spec requires.
                let is_valid = |r: &Value| r["jsonrpc"] == "2.0" && r["method"].is_string();
                let with_id = batch
                    .iter()
                    .filter(|r| r.get("id").is_some() || !is_valid(r))
                    .count();
                if !single && status == axum::http::StatusCode::OK {
                    let replies: Value = serde_json::from_slice(&out).unwrap();
                    prop_assert_eq!(replies.as_array().unwrap().len(), with_id);
                }
            }
        }
    }
}
