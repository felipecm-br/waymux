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
#[allow(dead_code)]
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
}
