//! CloudiyOS gateway — the bridge the browser cannot cross on its own.
//!
//! The browser speaks HTTP/WebSocket to `127.0.0.1`; this gateway speaks QUIC
//! (iroh) to the P2P network, under the machine's stable **client** identity.
//! It exposes a small local API — discover providers, provision/stop a VM,
//! run a kernel, and (over WebSocket) an interactive shell — plus a built-in
//! terminal page so the whole path browser → gateway → provider VM works with
//! no external assets.

use anyhow::Context as _;
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
use tracing::{info, warn};

struct Gateway {
    endpoint: iroh::Endpoint,
    id: String,
}
type Shared = Arc<Gateway>;

/// Serializes model-worker provisioning (container start / model pull) across
/// the whole process, whether an endpoint runs on the local gateway or is
/// served by this node acting as a remote provider. Process-global so both
/// call sites share one lock without threading a handle through.
fn worker_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub async fn serve(bind: SocketAddr, web_dir: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let secret = cloudiy_common::load_or_create_client_key()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret)
        .bind()
        .await?;
    let id = endpoint.id().to_string();
    let state: Shared = Arc::new(Gateway { endpoint, id });

    let mut app = Router::new()
        .route("/api/id", get(get_id))
        .route("/api/providers", get(get_providers))
        .route("/api/info", get(get_info))
        .route("/api/run", post(run_kernel))
        // Serverless model inference (App Store Public Endpoints).
        .route("/api/endpoint", post(run_endpoint))
        .route("/api/vm/up", post(vm_up))
        .route("/api/vm/status", get(vm_status))
        .route("/api/vm/down", post(vm_down))
        .route("/api/shell", get(shell_ws))
        // Large generated media (video) served from the shared volume so it
        // never has to cross the 8 MiB protocol frame.
        .route("/media/:name", get(serve_media))
        // Built-in xterm.js terminal, always available.
        .route("/terminal", get(terminal_page));

    // When a web/ directory is given, serve the real CloudiyOS UI at the
    // gateway origin so vm.html reaches /api/* and the WS same-origin (no
    // mixed-content, no CORS). Otherwise the built-in terminal is the root.
    match &web_dir {
        Some(dir) if dir.is_dir() => {
            let index = dir.join("vm.html");
            let serve_dir = tower_http::services::ServeDir::new(dir)
                .fallback(tower_http::services::ServeFile::new(index));
            app = app.fallback_service(serve_dir);
            info!("   Serving CloudiyOS from {}", dir.display());
        }
        Some(dir) => {
            warn!(
                "--web-dir {} is not a directory; serving built-in terminal",
                dir.display()
            );
            app = app.route("/", get(terminal_page));
        }
        None => {
            app = app.route("/", get(terminal_page));
        }
    }

    // The gateway holds the machine's P2P identity and drives real VMs, so it
    // must not be a confused deputy for a web page in the same browser. It
    // binds to loopback; this guard additionally rejects any request whose
    // `Origin` is a real site (anti-CSRF) or whose `Host` isn't loopback
    // (anti-DNS-rebinding) — replacing the previous permissive CORS (M2).
    let app = app
        .layer(axum::middleware::from_fn(guard_local_origin))
        .with_state(state);

    let listener = TcpListener::bind(bind).await?;
    info!("🖥️  CloudiyOS gateway on http://{bind}");
    if web_dir.as_ref().is_some_and(|d| d.is_dir()) {
        info!("   Open http://{bind}/vm.html for CloudiyOS (terminal at /terminal).");
    } else {
        info!("   Open http://{bind} for a live terminal.");
    }
    axum::serve(listener, app).await?;
    Ok(())
}

// ------------------------------------------------------------------ helpers

/// True for a loopback hostname (bare host, no port/scheme).
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]")
}

/// True if an `Origin`/`Host` header value points at loopback. Strips an
/// optional scheme and port; an `Origin` of `null` (opaque origins such as
/// `file://` or sandboxed frames) is treated as non-loopback and rejected.
fn header_host_is_loopback(value: &str) -> bool {
    let without_scheme = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))
        .unwrap_or(value);
    // IPv6 literal in brackets: keep the bracketed form for matching.
    let host = if let Some(rest) = without_scheme.strip_prefix('[') {
        rest.split_once(']')
            .map(|(h, _)| format!("[{h}]"))
            .unwrap_or_else(|| without_scheme.to_string())
    } else {
        without_scheme
            .split_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| without_scheme.to_string())
    };
    let host = host.split('/').next().unwrap_or(&host);
    is_loopback_host(host)
}

/// Reject cross-site drivers of the local gateway (anti-CSRF) and requests
/// reaching us under a non-loopback `Host` (anti-DNS-rebinding). A request
/// with no `Origin` (direct navigation, curl) is allowed; a present `Origin`
/// must be loopback.
async fn guard_local_origin(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    let headers = req.headers();
    if let Some(host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        if !header_host_is_loopback(host) {
            return (StatusCode::FORBIDDEN, "non-loopback Host rejected").into_response();
        }
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|h| h.to_str().ok()) {
        if !header_host_is_loopback(origin) {
            return (StatusCode::FORBIDDEN, "cross-site Origin rejected").into_response();
        }
    }
    next.run(req).await
}

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
        Ok(Response::Info(info)) => serde_json::to_value(info)
            .map(Json)
            .unwrap_or_else(|_| err("failed to serialize response")),
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
            .with(
                ResourceKind::Cpu,
                (b.cpu.unwrap_or(1.0) * 1000.0).round() as u64,
            )
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
        Ok(Response::Vm(info)) => serde_json::to_value(info)
            .map(Json)
            .unwrap_or_else(|_| err("failed to serialize response")),
        Ok(Response::PaymentRequired { requirements }) => {
            Json(json!({ "payment_required": requirements }))
        }
        Ok(Response::Error { message }) => err(message),
        Ok(_) => err("unexpected response"),
        Err(e) => err(e),
    }
}

async fn vm_status(State(s): State<Shared>, Query(q): Query<ToParam>) -> Json<serde_json::Value> {
    match rpc(
        &s,
        &q.to,
        Request::VmStatus {
            request: req(None, None),
        },
    )
    .await
    {
        Ok(Response::Vm(info)) => serde_json::to_value(info)
            .map(Json)
            .unwrap_or_else(|_| err("failed to serialize response")),
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
    match rpc(
        &s,
        &b.to,
        Request::StopVm {
            request: req(None, None),
            wipe: b.wipe,
        },
    )
    .await
    {
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
    #[serde(default)]
    cols: u16,
    #[serde(default)]
    rows: u16,
}

#[derive(Deserialize)]
struct ResizeMsg {
    cols: u16,
    rows: u16,
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
            let _ = socket
                .send(Message::Text("invalid node id\r\n".into()))
                .await;
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
        cols: p.cols,
        rows: p.rows,
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
                    // Binary = raw terminal data (keystrokes).
                    Some(Ok(Message::Binary(b))) => {
                        if proto::write_session_frame(&mut send, &SessionFrame::Data(b)).await.is_err() { break }
                    }
                    // Text = control channel; a {cols,rows} JSON is a resize.
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(r) = serde_json::from_str::<ResizeMsg>(&t) {
                            if proto::write_session_frame(&mut send, &SessionFrame::Resize { cols: r.cols, rows: r.rows }).await.is_err() { break }
                        } else if proto::write_session_frame(&mut send, &SessionFrame::Data(t.into_bytes())).await.is_err() { break }
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

// --------------------------------------------------- model inference (real)

/// Text endpoints served by a local Ollama worker. Advertised catalog names
/// (llama 3.3 70b, vLLM…) map to a model the node can actually host; the
/// response reports the model that really ran, never the catalog label.
fn ollama_model_for(key: &str) -> Option<&'static str> {
    match key {
        // Language endpoints from vm.html's ENDPOINTS list.
        "llama-ep" | "ollama" | "vllm" => Some("llama3.2:1b"),
        "qwen-coder" => Some("qwen2.5-coder:1.5b"),
        _ => None,
    }
}

/// Image endpoints and the GPU worker image that serves them. Returns
/// `(docker_image, http_path)`. These only run on a node exposing an NVIDIA
/// GPU to Docker; on any other host `run_endpoint` reports that honestly
/// instead of pretending. Wiring is complete; first real run awaits an
/// NVIDIA node joining the network.
fn image_worker_for(key: &str) -> Option<(&'static str, &'static str)> {
    match key {
        // Stable Diffusion family via a worker that exposes an HTTP API.
        "sdxl" | "flux2" | "z-image" => {
            Some(("ghcr.io/cloudiy/worker-sdxl:latest", "/sdapi/v1/txt2img"))
        }
        "nano-banana" | "qwen-edit" => {
            Some(("ghcr.io/cloudiy/worker-sdxl:latest", "/sdapi/v1/img2img"))
        }
        _ => None,
    }
}

/// Video endpoints and their GPU worker. Video generation needs high VRAM
/// (24–48 GB) AND the result is too large for the 8 MiB protocol frame, so the
/// worker writes the file to a shared volume and the gateway serves it at
/// `/media/<id>.mp4` (see [`serve_media`]) rather than returning it inline.
fn video_worker_for(key: &str) -> Option<&'static str> {
    match key {
        "hailuo-fast" | "hailuo-std" | "veo-fast" | "p-video" | "vidu-t2v" | "vidu-i2v"
        | "kling" => Some("ghcr.io/cloudiy/worker-ltx:latest"),
        _ => None,
    }
}

/// Audio endpoints and their worker image. Text-to-audio (music, speech) fits
/// the prompt model and would run on an audio worker; speech-to-text (whisper)
/// needs an uploaded audio file, which the prompt playground can't supply yet.
/// Like image/video, the wiring is in place and the first real run awaits a
/// worker node — [`run_endpoint`] reports this honestly instead of pretending.
fn audio_worker_for(key: &str) -> Option<(&'static str, &'static str)> {
    match key {
        // Prompt in, audio out.
        "stable-audio" => Some(("ghcr.io/cloudiy/worker-audio:latest", "text-to-audio")),
        "chatterbox" => Some(("ghcr.io/cloudiy/worker-tts:latest", "text-to-speech")),
        // Speech-to-text transcribes an uploaded file, not a text prompt.
        "whisper-ep" => Some(("ghcr.io/cloudiy/worker-whisper:latest", "speech-to-text")),
        _ => None,
    }
}

/// Directory the video worker writes finished clips to; the gateway serves
/// them from here so large files never cross the QUIC frame.
fn media_dir() -> std::path::PathBuf {
    std::env::temp_dir().join("cloudiy-media")
}

/// True when Docker on this host can expose an NVIDIA GPU to containers
/// (`--gpus all` will work). Drives the honest "needs a GPU node" gate — it is
/// never hardcoded. False on macOS (no GPU passthrough) and CPU-only Linux.
async fn gpu_available() -> bool {
    // Docker reports the nvidia runtime when the toolkit is installed.
    if let Ok(o) = docker(&["info", "--format", "{{.Runtimes}}"]).await {
        if String::from_utf8_lossy(&o.stdout).contains("nvidia") {
            return true;
        }
    }
    // Fallback: an nvidia-smi on PATH means a driver is present.
    tokio::process::Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

const OLLAMA_WORKER: &str = "cloudiy-wk-ollama";
/// Host port for the containerized worker. 11435 (not the default 11434) so it
/// never collides with a native Ollama the provider may already run.
const OLLAMA_PORT: &str = "11435";
const OLLAMA_URL: &str = "http://127.0.0.1:11435";

async fn docker(args: &[&str]) -> anyhow::Result<std::process::Output> {
    Ok(tokio::process::Command::new("docker")
        .args(args)
        .output()
        .await?)
}

/// Bring the Ollama worker up (once) and ensure the model is pulled. Returns
/// `true` when this was a cold start (model just provisioned), `false` warm.
async fn ensure_ollama(model: &str) -> anyhow::Result<bool> {
    let running = docker(&["inspect", "-f", "{{.State.Running}}", OLLAMA_WORKER])
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false);

    if !running {
        let _ = docker(&["rm", "-f", OLLAMA_WORKER]).await;
        let publish = format!("127.0.0.1:{OLLAMA_PORT}:11434");
        let out = docker(&[
            "run",
            "-d",
            "--name",
            OLLAMA_WORKER,
            "-p",
            &publish,
            "ollama/ollama",
        ])
        .await?;
        anyhow::ensure!(
            out.status.success(),
            "worker start failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Wait for the model server to accept connections. A per-request
        // timeout keeps a hung upstream from stalling the poll loop.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        for _ in 0..40 {
            if client
                .get(format!("{OLLAMA_URL}/api/version"))
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }

    let has_model = docker(&["exec", OLLAMA_WORKER, "ollama", "list"])
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(model))
        .unwrap_or(false);
    if !has_model {
        let out = docker(&["exec", OLLAMA_WORKER, "ollama", "pull", model]).await?;
        anyhow::ensure!(
            out.status.success(),
            "model pull failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        return Ok(true);
    }
    Ok(false)
}

#[derive(Deserialize)]
struct EndpointBody {
    /// Endpoint key from the App Store catalog (e.g. `llama-ep`).
    key: String,
    prompt: String,
    /// Optional remote provider (EndpointId) that hosts the model. When set,
    /// the gateway routes the run to that node over iroh (payment enforced
    /// provider-side); when absent, the model runs on this gateway host.
    #[serde(default)]
    to: Option<String>,
    /// Dev token (test admission) forwarded to the provider.
    #[serde(default)]
    token: Option<String>,
    /// x402/escrow payment payload (base64 JSON) forwarded to the provider.
    #[serde(default)]
    payment: Option<String>,
}

/// HTTP entry point for a model run. With `to`, routes to a remote provider
/// over iroh (that node runs the worker and settles payment); otherwise runs
/// the model on this gateway host via [`serve_endpoint`].
async fn run_endpoint(State(s): State<Shared>, Json(b): Json<EndpointBody>) -> Json<serde_json::Value> {
    if b.prompt.trim().is_empty() {
        return err("prompt is required");
    }

    if let Some(to) = b.to.as_deref().filter(|t| !t.is_empty()) {
        let mut request = req(b.token, b.payment);
        request.kernel = format!("endpoint:{}", b.key);
        let rpc_req = Request::RunEndpoint {
            request,
            key: b.key.clone(),
            prompt: b.prompt.clone(),
        };
        return match rpc(&s, to, rpc_req).await {
            Ok(Response::Job(r)) => {
                // The provider returns the model output as JSON bytes, signed
                // with its node key (r.signature / r.signed_by).
                let mut out: serde_json::Value = serde_json::from_slice(&r.output_data)
                    .unwrap_or_else(|_| json!({ "output": String::from_utf8_lossy(&r.output_data) }));
                if let Some(sig) = r.signature {
                    out["signature"] = json!(sig);
                }
                if let Some(by) = r.signed_by {
                    out["signed_by"] = json!(by);
                }
                out["settled_via"] = json!("remote-provider");
                Json(out)
            }
            Ok(Response::PaymentRequired { requirements }) => {
                Json(json!({ "payment_required": requirements }))
            }
            Ok(Response::Error { message }) => err(message),
            Ok(_) => err("unexpected response"),
            Err(e) => err(e),
        };
    }

    Json(serve_endpoint(&b.key, &b.prompt).await)
}

/// Run a catalog model on THIS host and return its output as a JSON value.
/// Shared by the local gateway path and by a provider node serving a remote
/// [`Request::RunEndpoint`]. Text runs on a local Ollama worker (real today);
/// image/video need an NVIDIA GPU and report honestly when absent; audio is
/// wired but awaits a worker. Always returns a value (errors carry an
/// `"error"` field) so the caller can relay it verbatim.
pub(crate) async fn serve_endpoint(key: &str, prompt: &str) -> serde_json::Value {
    if prompt.trim().is_empty() {
        return json!({ "error": "prompt is required" });
    }

    // Text endpoints run on the local CPU Ollama worker (real today).
    if let Some(model) = ollama_model_for(key) {
        let cold = {
            let _guard = worker_lock().lock().await;
            match ensure_ollama(model).await {
                Ok(c) => c,
                Err(e) => return json!({ "error": format!("provisioning failed: {e}") }),
            }
        };
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{OLLAMA_URL}/api/generate"))
            .json(&json!({ "model": model, "prompt": prompt, "stream": false }))
            .timeout(std::time::Duration::from_secs(120))
            .send()
            .await;
        let body: serde_json::Value = match resp {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => return json!({ "error": format!("bad model response: {e}") }),
            },
            Err(e) => return json!({ "error": format!("inference failed: {e}") }),
        };
        return json!({
            "kind": "text",
            "output": body["response"].as_str().unwrap_or_default().trim(),
            "model": model,
            "tokens": body["eval_count"],
            "cold_start": cold,
            "settled_via": "local-node",
        });
    }

    // Image endpoints need a real GPU. The gate is driven by actual hardware
    // detection, never hardcoded: on a GPU node this provisions the SDXL
    // worker and returns a base64 PNG; on a CPU/macOS host it says so plainly.
    if let Some((worker_image, api_path)) = image_worker_for(key) {
        if !gpu_available().await {
            return gpu_required(key, worker_image, "image");
        }
        return match run_image_worker(worker_image, api_path, prompt).await {
            Ok(v) => v,
            Err(e) => json!({ "error": format!("image inference failed: {e}") }),
        };
    }

    // Video: GPU-gated like image, but delivered as a file URL.
    if let Some(worker_image) = video_worker_for(key) {
        if !gpu_available().await {
            return gpu_required(key, worker_image, "video");
        }
        return match run_video_worker(worker_image, prompt).await {
            Ok(v) => v,
            Err(e) => json!({ "error": format!("video inference failed: {e}") }),
        };
    }

    // Audio endpoints: honest about what is and isn't wired, never faked.
    if let Some((worker, task)) = audio_worker_for(key) {
        return audio_pending(key, worker, task);
    }

    json!({ "error": format!("unknown endpoint '{key}'") })
}

/// Honest response for an audio model while no audio worker node is serving it.
/// Speech-to-text is called out separately because it needs an uploaded audio
/// file, which the prompt-based playground cannot provide.
fn audio_pending(key: &str, worker: &str, task: &str) -> serde_json::Value {
    let error = if task == "speech-to-text" {
        format!(
            "'{key}' transcribes uploaded audio — the prompt playground can't \
             supply an audio file yet. Call the API with an audio input instead."
        )
    } else {
        format!(
            "'{key}' is an audio {task} model — awaiting an audio worker node \
             ({worker}). Run `cloudiy share` on a node serving it."
        )
    };
    json!({ "error": error, "needs": "audio-worker", "worker": worker })
}

/// Honest response for a GPU model when this node has no NVIDIA GPU.
fn gpu_required(key: &str, worker: &str, kind: &str) -> serde_json::Value {
    json!({
        "error": format!(
            "'{key}' is a GPU {kind} model — this node has no NVIDIA GPU. \
             Run `cloudiy share` on a Linux + NVIDIA machine to serve it."
        ),
        "needs": "nvidia-gpu",
        "worker": worker,
    })
}

const IMAGE_WORKER: &str = "cloudiy-wk-sdxl";
const IMAGE_PORT: &str = "7860";
const IMAGE_URL: &str = "http://127.0.0.1:7860";

/// Provision the SDXL worker (GPU) and generate one image, returned as a
/// base64 PNG. Only reached when [`gpu_available`] is true — untested on this
/// machine (no NVIDIA), validated on the first GPU node that joins.
async fn run_image_worker(
    worker_image: &str,
    api_path: &str,
    prompt: &str,
) -> anyhow::Result<serde_json::Value> {
    let cold = {
        let _guard = worker_lock().lock().await;
        let running = docker(&["inspect", "-f", "{{.State.Running}}", IMAGE_WORKER])
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        if !running {
            let _ = docker(&["rm", "-f", IMAGE_WORKER]).await;
            let publish = format!("127.0.0.1:{IMAGE_PORT}:7860");
            let out = docker(&[
                "run",
                "-d",
                "--name",
                IMAGE_WORKER,
                "--gpus",
                "all",
                "-p",
                &publish,
                worker_image,
            ])
            .await?;
            anyhow::ensure!(
                out.status.success(),
                "worker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            // Model load on first boot is slow — wait generously, but bound
            // each poll so a hung upstream can't stall the loop.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            for _ in 0..120 {
                if client
                    .get(format!("{IMAGE_URL}/sdapi/v1/progress"))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            true
        } else {
            false
        }
    };

    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(format!("{IMAGE_URL}{api_path}"))
        .json(&json!({ "prompt": prompt, "steps": 30, "width": 1024, "height": 1024 }))
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await?
        .json()
        .await?;
    // The SD API returns base64 PNGs under `images[0]`.
    let b64 = resp["images"][0].as_str().unwrap_or_default().to_string();
    Ok(json!({
        "kind": "image",
        "image_b64": b64,
        "model": worker_image,
        "cold_start": cold,
        "settled_via": "gpu-node",
    }))
}

const VIDEO_WORKER: &str = "cloudiy-wk-ltx";
const VIDEO_PORT: &str = "7861";
const VIDEO_URL: &str = "http://127.0.0.1:7861";

/// Provision the video worker (GPU) and generate one clip. The worker writes
/// the .mp4 into the shared media volume; the gateway returns a URL served by
/// [`serve_media`] — the file never crosses the 8 MiB protocol frame. Only
/// reached when [`gpu_available`]; untested here (no NVIDIA), validated on the
/// first GPU node.
async fn run_video_worker(
    worker_image: &str,
    prompt: &str,
) -> anyhow::Result<serde_json::Value> {
    let dir = media_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .with_context(|| format!("creating media dir {}", dir.display()))?;

    let cold = {
        let _guard = worker_lock().lock().await;
        let running = docker(&["inspect", "-f", "{{.State.Running}}", VIDEO_WORKER])
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        if !running {
            let _ = docker(&["rm", "-f", VIDEO_WORKER]).await;
            let publish = format!("127.0.0.1:{VIDEO_PORT}:7860");
            let mount = format!("{}:/out", dir.display());
            let out = docker(&[
                "run",
                "-d",
                "--name",
                VIDEO_WORKER,
                "--gpus",
                "all",
                "-p",
                &publish,
                "-v",
                &mount,
                worker_image,
            ])
            .await?;
            anyhow::ensure!(
                out.status.success(),
                "worker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let client = reqwest::Client::new();
            for _ in 0..150 {
                if client
                    .get(format!("{VIDEO_URL}/health"))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false)
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
            true
        } else {
            false
        }
    };

    let clip_id = uuid::Uuid::new_v4().to_string();
    let client = reqwest::Client::new();
    // Worker writes /out/<clip_id>.mp4; video generation is slow, allow 10 min.
    let resp: serde_json::Value = client
        .post(format!("{VIDEO_URL}/generate"))
        .json(&json!({ "prompt": prompt, "out": format!("{clip_id}.mp4"), "duration": 5 }))
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?
        .json()
        .await?;
    anyhow::ensure!(
        resp["ok"].as_bool().unwrap_or(false),
        "worker error: {resp}"
    );

    Ok(json!({
        "kind": "video",
        "media_url": format!("/media/{clip_id}.mp4"),
        "model": worker_image,
        "cold_start": cold,
        "settled_via": "gpu-node",
    }))
}

/// Serve a finished media file from the shared volume. Path traversal is
/// blocked by rejecting any component that is not a plain file name.
async fn serve_media(axum::extract::Path(name): axum::extract::Path<String>) -> impl IntoResponse {
    use axum::http::StatusCode;
    if name.contains('/') || name.contains("..") {
        return (StatusCode::BAD_REQUEST, "bad name").into_response();
    }
    let path = media_dir().join(&name);
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let ct = if name.ends_with(".mp4") {
                "video/mp4"
            } else {
                "application/octet-stream"
            };
            ([(axum::http::header::CONTENT_TYPE, ct)], bytes).into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn terminal_page() -> impl IntoResponse {
    Html(include_str!("terminal.html"))
}

#[cfg(test)]
mod tests {
    use super::header_host_is_loopback;

    #[test]
    fn accepts_loopback_origins_and_hosts() {
        for v in [
            "http://127.0.0.1:7000",
            "http://localhost:8080",
            "https://localhost",
            "127.0.0.1:9000",
            "localhost",
            "http://[::1]:7000",
            "[::1]",
        ] {
            assert!(header_host_is_loopback(v), "should accept {v}");
        }
    }

    #[test]
    fn rejects_cross_site_and_rebinding() {
        for v in [
            "http://evil.com",
            "https://evil.com:7000",
            "http://127.0.0.1.evil.com",
            "http://localhost.evil.com",
            "null",
            "http://169.254.169.254",
        ] {
            assert!(!header_host_is_loopback(v), "should reject {v}");
        }
    }
}
