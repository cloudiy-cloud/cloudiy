//! CloudiyOS gateway — the bridge the browser cannot cross on its own.
//!
//! The browser speaks HTTP/WebSocket to `127.0.0.1`; this gateway speaks QUIC
//! (iroh) to the P2P network, under the machine's stable **client** identity.
//! It exposes a small local API — discover providers, provision/stop a VM,
//! run a kernel, and (over WebSocket) an interactive shell — plus a built-in
//! terminal page so the whole path browser → gateway → provider VM works with
//! no external assets.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    response::{Html, IntoResponse},
    routing::{get, post},
    Json, Router,
};
use cloudiy_common::proto::{self, Request, Response, SessionFrame};
use cloudiy_common::JobRequest;
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

struct Gateway {
    endpoint: iroh::Endpoint,
    id: String,
}
type Shared = Arc<Gateway>;

pub async fn serve(bind: SocketAddr) -> anyhow::Result<()> {
    let secret = cloudiy_common::load_or_create_client_key()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret)
        .bind()
        .await?;
    let id = endpoint.id().to_string();
    let state: Shared = Arc::new(Gateway { endpoint, id });

    let app = Router::new()
        .route("/", get(terminal_page))
        .route("/api/id", get(get_id))
        .route("/api/providers", get(get_providers))
        .route("/api/info", get(get_info))
        .route("/api/run", post(run_kernel))
        .route("/api/vm/up", post(vm_up))
        .route("/api/vm/status", get(vm_status))
        .route("/api/vm/down", post(vm_down))
        .route("/api/shell", get(shell_ws))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let listener = TcpListener::bind(bind).await?;
    info!("🖥️  CloudiyOS gateway on http://{bind}");
    info!("   Open http://{bind} in a browser for a live terminal.");
    axum::serve(listener, app).await?;
    Ok(())
}

// ------------------------------------------------------------------ helpers

fn req(token: Option<String>, payment: Option<String>) -> JobRequest {
    JobRequest {
        job_id: uuid::Uuid::new_v4().to_string(),
        kernel: String::new(),
        input_data: vec![],
        params: Default::default(),
        auth_token: token.unwrap_or_default(),
        consumer_pubkey: None,
        payment,
    }
}

async fn rpc(state: &Gateway, to: &str, request: Request) -> anyhow::Result<Response> {
    let id: iroh::EndpointId = to.parse().map_err(|_| anyhow::anyhow!("invalid node id"))?;
    let conn = state.endpoint.connect(id, proto::ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &request).await?;
    let resp = proto::read_msg::<Response>(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    Ok(resp)
}

fn err(msg: impl std::fmt::Display) -> Json<serde_json::Value> {
    Json(json!({ "error": msg.to_string() }))
}

// ------------------------------------------------------------------ routes

async fn get_id(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(json!({ "id": s.id }))
}

#[derive(Deserialize)]
struct Via {
    via: String,
}

async fn get_providers(State(s): State<Shared>, Query(q): Query<Via>) -> Json<serde_json::Value> {
    match rpc(&s, &q.via, Request::Providers).await {
        Ok(Response::Providers(list)) => {
            let now = chrono::Utc::now().timestamp();
            let verified: Vec<_> = list
                .into_iter()
                .filter_map(|sa| cloudiy_common::verify_announcement(&sa, now).ok())
                .collect();
            Json(json!({ "providers": verified }))
        }
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct ToParam {
    to: String,
}

async fn get_info(State(s): State<Shared>, Query(q): Query<ToParam>) -> Json<serde_json::Value> {
    match rpc(&s, &q.to, Request::Info).await {
        Ok(Response::Info(info)) => Json(serde_json::to_value(info).unwrap()),
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct RunBody {
    to: String,
    kernel: String,
    data: String,
    token: Option<String>,
}

async fn run_kernel(State(s): State<Shared>, Json(b): Json<RunBody>) -> Json<serde_json::Value> {
    let mut request = req(b.token, None);
    request.kernel = b.kernel;
    request.input_data = b.data.into_bytes();
    match rpc(&s, &b.to, Request::Submit(request)).await {
        Ok(Response::Job(r)) => Json(json!({
            "status": r.status,
            "output": String::from_utf8_lossy(&r.output_data),
            "signed_by": r.signed_by,
        })),
        Ok(Response::PaymentRequired { requirements }) => {
            Json(json!({ "payment_required": requirements }))
        }
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct VmUpBody {
    to: String,
    image: Option<String>,
    cpu: Option<f64>,
    memory_mb: Option<u64>,
    #[serde(default)]
    ports: Vec<u16>,
    token: Option<String>,
    #[serde(default)]
    x402_demo: bool,
}

async fn vm_up(State(s): State<Shared>, Json(b): Json<VmUpBody>) -> Json<serde_json::Value> {
    use cloudiy_protocol::{ResourceKind, ResourceVector, WorkloadSpec};
    let spec = WorkloadSpec {
        image: b.image,
        resources: ResourceVector::new()
            .with(ResourceKind::Cpu, (b.cpu.unwrap_or(1.0) * 1000.0).round() as u64)
            .with(ResourceKind::Memory, b.memory_mb.unwrap_or(1024)),
        ports: b.ports,
        ..Default::default()
    };
    let payment = b.x402_demo.then(cloudiy_sdk::demo_payment_payload);
    let request = Request::StartVm {
        request: req(b.token, payment),
        spec,
    };
    match rpc(&s, &b.to, request).await {
        Ok(Response::Vm(info)) => Json(serde_json::to_value(info).unwrap()),
        Ok(Response::PaymentRequired { requirements }) => {
            Json(json!({ "payment_required": requirements }))
        }
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

async fn vm_status(State(s): State<Shared>, Query(q): Query<ToParam>) -> Json<serde_json::Value> {
    match rpc(&s, &q.to, Request::VmStatus { request: req(None, None) }).await {
        Ok(Response::Vm(info)) => Json(serde_json::to_value(info).unwrap()),
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct VmDownBody {
    to: String,
    #[serde(default)]
    wipe: bool,
}

async fn vm_down(State(s): State<Shared>, Json(b): Json<VmDownBody>) -> Json<serde_json::Value> {
    match rpc(&s, &b.to, Request::StopVm { request: req(None, None), wipe: b.wipe }).await {
        Ok(Response::Ack) => Json(json!({ "ok": true })),
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

// -------------------------------------------------------------- shell (WS)

#[derive(Deserialize)]
struct ShellParams {
    to: String,
    token: Option<String>,
}

async fn shell_ws(
    ws: WebSocketUpgrade,
    Query(p): Query<ShellParams>,
    State(s): State<Shared>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| shell_bridge(socket, s, p))
}

/// Bridge a browser WebSocket to a provider VM shell over QUIC. Reads and
/// writes are multiplexed with `select!` so a single task owns both ends.
async fn shell_bridge(mut socket: WebSocket, state: Shared, p: ShellParams) {
    let id: iroh::EndpointId = match p.to.parse() {
        Ok(id) => id,
        Err(_) => {
            let _ = socket.send(Message::Text("invalid node id\r\n".into())).await;
            return;
        }
    };
    let conn = match state.endpoint.connect(id, proto::ALPN).await {
        Ok(c) => c,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("connect failed: {e}\r\n")))
                .await;
            return;
        }
    };
    let (mut send, mut recv) = match conn.open_bi().await {
        Ok(s) => s,
        Err(e) => {
            let _ = socket
                .send(Message::Text(format!("stream failed: {e}\r\n")))
                .await;
            return;
        }
    };
    let open = Request::OpenSession {
        request: req(p.token, None),
        command: vec![],
    };
    if proto::write_msg(&mut send, &open).await.is_err() {
        return;
    }
    match proto::read_msg::<Response>(&mut recv).await {
        Ok(Response::SessionOpened { .. }) => {
            let _ = socket
                .send(Message::Text(
                    "\u{1b}[38;2;204;255;51m● connected to VM\u{1b}[0m\r\n".into(),
                ))
                .await;
        }
        Ok(Response::Error { message }) => {
            let _ = socket.send(Message::Text(format!("{message}\r\n"))).await;
            return;
        }
        _ => return,
    }

    loop {
        tokio::select! {
            ws_msg = socket.recv() => {
                match ws_msg {
                    Some(Ok(Message::Binary(b))) => {
                        if proto::write_session_frame(&mut send, &SessionFrame::Data(b)).await.is_err() { break }
                    }
                    Some(Ok(Message::Text(t))) => {
                        if proto::write_session_frame(&mut send, &SessionFrame::Data(t.into_bytes())).await.is_err() { break }
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        let _ = proto::write_session_frame(&mut send, &SessionFrame::Eof).await;
                        break;
                    }
                    _ => {}
                }
            }
            frame = proto::read_session_frame(&mut recv) => {
                match frame {
                    Ok(Some(SessionFrame::Data(d))) => {
                        if socket.send(Message::Binary(d)).await.is_err() { break }
                    }
                    Ok(Some(SessionFrame::Exit(code))) => {
                        let _ = socket.send(Message::Text(format!(
                            "\r\n\u{1b}[38;2;204;255;51m● session ended{}\u{1b}[0m\r\n",
                            code.map(|c| format!(" (exit {c})")).unwrap_or_default()
                        ))).await;
                        break;
                    }
                    Ok(Some(SessionFrame::Error(m))) => {
                        let _ = socket.send(Message::Text(format!("error: {m}\r\n"))).await;
                        break;
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
    }
    conn.close(0u32.into(), b"done");
    let _ = socket.send(Message::Close(None)).await;
    warn!("shell bridge closed");
}

async fn terminal_page() -> impl IntoResponse {
    Html(include_str!("terminal.html"))
}
