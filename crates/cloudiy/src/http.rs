//! HTTP front (axum): kept for the browser marketplace/dashboard, which
//! cannot speak raw QUIC. Thin adapters over [`crate::core`]; the P2P front
//! in [`crate::p2p`] is the primary transport for CLI consumers.

use axum::{
    extract::{Json, Path, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Router,
};
use cloudiy_common::{JobRequest, StatusResponse};
use serde::Serialize;
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

use crate::core::{self, SharedState};

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    uptime_secs: i64,
}

async fn health(State(state): State<SharedState>) -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        uptime_secs: (chrono::Utc::now() - state.started_at).num_seconds(),
    })
}

async fn node_info(State(state): State<SharedState>) -> Json<cloudiy_common::NodeInfo> {
    Json(core::node_info(&state))
}

async fn submit_job(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<JobRequest>,
) -> Response {
    let payment_header = headers
        .get("x-payment")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    let outcome = match core::submit_guarded(state, req, payment_header).await {
        Ok(o) => o,
        Err(message) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({ "error": message })),
            )
                .into_response()
        }
    };

    match outcome {
        core::SubmitOutcome::PaymentRequired(requirements) => {
            (StatusCode::PAYMENT_REQUIRED, Json(requirements)).into_response()
        }
        core::SubmitOutcome::Completed(response) => {
            let receipt = response.payment_receipt.clone();
            let mut res = (StatusCode::OK, Json(response)).into_response();
            if let Some(v) = receipt.and_then(|r| HeaderValue::from_str(&r).ok()) {
                res.headers_mut().insert("x-payment-response", v);
            }
            res
        }
    }
}

async fn get_status(
    State(state): State<SharedState>,
    Path(job_id): Path<String>,
) -> Json<StatusResponse> {
    Json(core::job_status(&state, job_id))
}

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/info", get(node_info))
        .route("/submit", post(submit_job))
        .route("/status/:job_id", get(get_status))
        // Cap request bodies so a malicious consumer can't exhaust memory.
        .layer(RequestBodyLimitLayer::new(core::MAX_BODY_BYTES))
        // NOTE: permissive CORS is convenient for the browser marketplace
        // during the MVP, but lock this down to known origins before mainnet.
        .layer(CorsLayer::permissive())
        .with_state(state)
}
