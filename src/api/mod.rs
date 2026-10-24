//! HTTP control plane: the authenticated `/rpc` and `/api/status` routes.

mod rpc;
mod state;

pub use rpc::{RpcError, RpcRequest, RpcResponse};
pub use state::{ApiState, PaneStateInfo, StallRecord};

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

/// Compares without an early exit, so response time does not reveal how much of a
/// guessed token matched.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub async fn auth_middleware(
    State(token): State<Arc<String>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, axum::http::StatusCode> {
    let authenticated = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .is_some_and(|b| constant_time_eq(b.trim().as_bytes(), token.as_bytes()));

    if authenticated {
        Ok(next.run(req).await)
    } else {
        tracing::warn!("Unauthorized HTTP request to {}", req.uri().path());
        Err(axum::http::StatusCode::UNAUTHORIZED)
    }
}

pub fn api_router(adapters: ApiState, auth_token: Arc<String>) -> Router {
    let protected = Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/api/status", post(handle_status))
        .route_layer(axum::middleware::from_fn_with_state(
            auth_token,
            auth_middleware,
        ))
        .with_state(adapters);

    Router::new().merge(protected)
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
