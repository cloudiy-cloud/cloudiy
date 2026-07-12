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

// ---- model lifecycle: warm set + on-demand servable list (BE#1/#3) --------

/// Endpoint keys currently warm on this node (their worker is up), with the
/// last time each was served. A model becomes warm on first use and stays
/// warm until [`evict_idle`] stops it — nothing is pre-installed.
fn warm_reg() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>> {
    static R: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Mark an endpoint key as warm (just served). Called after the worker for it
/// is confirmed up, so announcements and the scheduler can prefer this node.
pub(crate) fn mark_warm(key: &str) {
    if let Ok(mut m) = warm_reg().lock() {
        m.insert(key.to_string(), std::time::Instant::now());
    }
}

/// The endpoint keys currently warm on this node.
pub(crate) fn warm_models() -> Vec<String> {
    warm_reg()
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Drop warm entries idle longer than `ttl` from the registry (a caller may
/// then stop the backing container to reclaim VRAM). Returns the evicted keys.
/// Full VRAM-aware eviction is future work; this bounds the warm set by age.
pub(crate) fn evict_idle(ttl: std::time::Duration) -> Vec<String> {
    let now = std::time::Instant::now();
    let mut evicted = Vec::new();
    if let Ok(mut m) = warm_reg().lock() {
        m.retain(|k, &mut last| {
            let keep = now.duration_since(last) < ttl;
            if !keep {
                evicted.push(k.clone());
            }
            keep
        });
    }
    evicted
}

/// Endpoint keys this node is willing and able to serve, given its hardware.
/// Text runs on a CPU Ollama worker (always); image/video need an NVIDIA GPU,
/// so they are announced only when one is present. The worker image + weights
/// are pulled on demand, so "servable" does not mean "pre-installed".
pub(crate) async fn servable_models() -> Vec<String> {
    // Opt-in: a node announces only the models the operator has installed via
    // My Nodes / App Store (persisted hosted set) — not everything by default.
    // A casual provider hosts nothing and stores no model bytes. GPU-only
    // models are announced solely when an NVIDIA GPU is actually present.
    let gpu = gpu_available().await;
    let mut v: Vec<String> = hosted_models()
        .into_iter()
        .filter(|k| match catalog_entry(k) {
            Some((_, _, needs_gpu, _)) => !needs_gpu || gpu,
            None => true,
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

// ---- model hosting catalog + opt-in installed ("hosted") set --------------

/// The full model-endpoint catalog this build can host, in one place:
/// `(key, worker image, needs_gpu, category)`. Keys mirror os.html's ENDPOINTS.
/// Single source of truth for install / list / announce.
fn model_catalog() -> &'static [(&'static str, &'static str, bool, &'static str)] {
    &[
        ("llama-ep", "ollama/ollama", false, "language"),
        (
            "whisper-ep",
            "onerahmet/openai-whisper-asr-webservice:latest",
            false,
            "audio",
        ),
        (
            "chatterbox",
            "ghcr.io/cloudiy/worker-tts:latest",
            false,
            "audio",
        ),
        (
            "stable-audio",
            "ghcr.io/cloudiy/worker-audio:latest",
            false,
            "audio",
        ),
        ("sdxl", "ghcr.io/cloudiy/worker-sdxl:latest", true, "image"),
        ("flux2", "ghcr.io/cloudiy/worker-sdxl:latest", true, "image"),
        (
            "z-image",
            "ghcr.io/cloudiy/worker-sdxl:latest",
            true,
            "image",
        ),
        (
            "nano-banana",
            "ghcr.io/cloudiy/worker-sdxl:latest",
            true,
            "image",
        ),
        (
            "qwen-edit",
            "ghcr.io/cloudiy/worker-sdxl:latest",
            true,
            "image",
        ),
        (
            "hailuo-fast",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        (
            "hailuo-std",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        (
            "veo-fast",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        (
            "p-video",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        (
            "vidu-t2v",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        (
            "vidu-i2v",
            "ghcr.io/cloudiy/worker-ltx:latest",
            true,
            "video",
        ),
        ("kling", "ghcr.io/cloudiy/worker-ltx:latest", true, "video"),
    ]
}

fn catalog_entry(key: &str) -> Option<(&'static str, &'static str, bool, &'static str)> {
    model_catalog().iter().copied().find(|(k, ..)| *k == key)
}

fn hosted_path() -> std::path::PathBuf {
    cloudiy_common::config_dir().join("hosted_models.json")
}

fn hosted_reg() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    R.get_or_init(|| {
        let set = std::fs::read_to_string(hosted_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<String>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        std::sync::Mutex::new(set)
    })
}

/// Endpoint keys the operator has installed to host on this node (persisted).
fn hosted_models() -> Vec<String> {
    hosted_reg()
        .lock()
        .map(|m| m.iter().cloned().collect())
        .unwrap_or_default()
}

/// Add or remove a key from the hosted set and persist it.
fn set_hosted(key: &str, on: bool) {
    if let Ok(mut m) = hosted_reg().lock() {
        if on {
            m.insert(key.to_string());
        } else {
            m.remove(key);
        }
        let mut v: Vec<&String> = m.iter().collect();
        v.sort();
        if let Ok(s) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::create_dir_all(cloudiy_common::config_dir());
            let _ = std::fs::write(hosted_path(), s);
        }
    }
}

async fn image_present(image: &str) -> bool {
    docker(&["image", "inspect", image])
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

async fn image_size_bytes(image: &str) -> Option<u64> {
    let o = docker(&["image", "inspect", image, "--format", "{{.Size}}"])
        .await
        .ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8_lossy(&o.stdout).trim().parse().ok()
}

// ---- supply-chain: pin worker images by digest ----------------------------

/// Reviewed worker-image digests shipped with this build (worker_digests.json),
/// keyed by the catalog image ref. Pulling by digest makes a repointed tag
/// unable to substitute a different image. Third-party images are pinned by
/// hand; Cloudiy workers get theirs from build-workers.sh on push.
fn worker_digests() -> &'static std::collections::HashMap<String, String> {
    static M: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    M.get_or_init(|| {
        let raw = include_str!("../worker_digests.json");
        serde_json::from_str::<std::collections::HashMap<String, String>>(raw)
            .unwrap_or_default()
            .into_iter()
            .filter(|(k, _)| !k.starts_with('_')) // drop the "_comment" key
            .collect()
    })
}

/// The pinned digest (`sha256:...`) for a catalog image ref, if we ship one.
fn pinned_digest(image: &str) -> Option<&'static str> {
    worker_digests().get(image).map(|s| s.as_str())
}

/// Strip a trailing `:tag` from an image ref, preserving a registry host that
/// itself contains a colon+port (only strips when the part after `:` is a tag,
/// i.e. contains no `/`).
fn repo_of(image: &str) -> &str {
    match image.rsplit_once(':') {
        Some((repo, tag)) if !tag.contains('/') => repo,
        _ => image,
    }
}

/// A pull-safe ref for an image: `repo@sha256:...` when pinned, else the ref
/// unchanged. Content-addressed pulls are tamper-evident (docker verifies).
fn pinned_ref(image: &str) -> String {
    match pinned_digest(image) {
        Some(d) => format!("{}@{}", repo_of(image), d),
        None => image.to_string(),
    }
}

/// Pull a worker image, by pinned digest when we have one. On a digest pull the
/// image lands untagged, so we re-tag it with the catalog ref that the run_*
/// helpers use. `CLOUDIY_REQUIRE_PINNED_IMAGES` makes an unpinned image a hard
/// error (supply-chain strict mode).
async fn pull_worker(image: &str) -> anyhow::Result<()> {
    check_pinned(image)?; // strict mode: refuse unpinned when required
    let reference = pinned_ref(image);
    let pinned = reference != image; // i.e. we pulled by @sha256
    let out = docker(&["pull", reference.as_str()]).await?;
    anyhow::ensure!(
        out.status.success(),
        "pull failed: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    if pinned {
        // Digest pull is untagged — tag it so tag-based lookups/runs resolve.
        let _ = docker(&["tag", reference.as_str(), image]).await;
    }
    verify_image_signature(image).await?; // optional cosign (env-gated)
    Ok(())
}

/// Pick the best provider for an endpoint from verified announcements: only
/// those that serve `key`, warm ones first, then least utilized, then cheapest.
/// This is how a caller fills the `to` for a routed run (BE#2).
pub(crate) fn select_endpoint_provider<'a>(
    providers: &'a [cloudiy_protocol::ProviderAnnouncement],
    key: &str,
) -> Option<&'a cloudiy_protocol::ProviderAnnouncement> {
    providers
        .iter()
        .filter(|p| p.served_models.iter().any(|m| m == key))
        .min_by(|a, b| {
            let aw = a.warm_models.iter().any(|m| m == key);
            let bw = b.warm_models.iter().any(|m| m == key);
            bw.cmp(&aw) // warm (true) sorts before cold (false)
                .then(
                    a.utilization
                        .partial_cmp(&b.utilization)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    a.price_micro_usdc_per_hour
                        .cmp(&b.price_micro_usdc_per_hour),
                )
        })
}

pub async fn serve(bind: SocketAddr, web_dir: Option<std::path::PathBuf>) -> anyhow::Result<()> {
    let secret = cloudiy_common::load_or_create_client_key()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret)
        .bind()
        .await?;
    let id = endpoint.id().to_string();
    let state: Shared = Arc::new(Gateway { endpoint, id });

    // Phase 3: periodic auto-hosting controller. A no-op unless the operator
    // set policy.mode = "auto" with a disk budget (My Nodes); then it polls
    // demand and fills the budget with the most in-demand models.
    {
        let ep = state.endpoint.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let _ = autohost_cycle(&ep).await;
            }
        });
    }

    // Bound the warm set by idle age: a model unused for a while stops being
    // advertised as warm (and could have its container reclaimed). Keeps the
    // "warm" signal honest as load shifts.
    tokio::spawn(async {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
            let _evicted = evict_idle(std::time::Duration::from_secs(1800));
        }
    });

    let mut app = Router::new()
        .route("/api/id", get(get_id))
        .route("/api/providers", get(get_providers))
        // Real machines to rent, from the gateway's directory (Hardware Store).
        .route("/api/machines", get(get_machines))
        // Which providers can serve a given model endpoint (warm ones first).
        .route("/api/endpoint/providers", get(endpoint_providers))
        // Demand oracle (phase 2): per-endpoint interest vs supply, ranked.
        .route("/api/demand", get(get_demand))
        // x402 quote for an endpoint run (provider, payout, mint, price).
        .route("/api/quote", get(get_quote))
        .route("/api/info", get(get_info))
        .route("/api/run", post(run_kernel))
        // Serverless model inference (App Store Public Endpoints).
        .route("/api/endpoint", post(run_endpoint))
        // Token-streaming variant for local text models (progressive output).
        .route("/api/endpoint/stream", post(run_endpoint_stream))
        .route("/api/vm/up", post(vm_up))
        .route("/api/vm/status", get(vm_status))
        .route("/api/vm/down", post(vm_down))
        // Provider model hosting (My Nodes / App Store host tab): list catalog
        // host-status, install (pull + enable) and uninstall (disable + reclaim).
        .route("/api/models", get(models_list))
        .route("/api/models/install", post(models_install))
        .route("/api/models/uninstall", post(models_uninstall))
        // Provider dashboard: identity, earnings, hosted models, disk.
        .route("/api/node", get(node_dashboard))
        // Auto-hosting policy (disk budget) + run one controller cycle now.
        .route("/api/node/policy", post(set_policy))
        .route("/api/node/autohost/run", post(autohost_run))
        .route("/api/shell", get(shell_ws))
        // Large generated media (video) served from the shared volume so it
        // never has to cross the 8 MiB protocol frame.
        .route("/media/:name", get(serve_media))
        // Built-in xterm.js terminal, always available.
        .route("/terminal", get(terminal_page));

    // When a web/ directory is given, serve the real CloudiyOS UI at the
    // gateway origin so os.html reaches /api/* and the WS same-origin (no
    // mixed-content, no CORS). Otherwise the built-in terminal is the root.
    match &web_dir {
        Some(dir) if dir.is_dir() => {
            let index = dir.join("os.html");
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
        info!("   Open http://{bind}/os.html for CloudiyOS (terminal at /terminal).");
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
    dial(&state.endpoint, to, request).await
}

/// One request/response over a fresh iroh stream — Gateway-independent so the
/// provider process (`cloudiy share`) can reuse it (e.g. the auto-host cycle).
pub(crate) async fn dial(
    endpoint: &iroh::Endpoint,
    to: &str,
    request: Request,
) -> anyhow::Result<Response> {
    let id: iroh::EndpointId = to.parse().map_err(|_| anyhow::anyhow!("invalid node id"))?;
    let conn = endpoint.connect(id, proto::ALPN).await?;
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

// ---- provider model hosting API (My Nodes / App Store) --------------------

/// Catalog host-status for every model: installed (in the hosted set), whether
/// its image is present locally, size, GPU need, and whether it can run here.
async fn models_list() -> Json<serde_json::Value> {
    let gpu = gpu_available().await;
    let hosted: std::collections::HashSet<String> = hosted_models().into_iter().collect();
    let warm: std::collections::HashSet<String> = warm_models().into_iter().collect();
    let mut models = Vec::new();
    for (key, image, needs_gpu, cat) in model_catalog().iter().copied() {
        let present = image_present(image).await;
        let size = if present {
            image_size_bytes(image).await
        } else {
            None
        };
        models.push(json!({
            "key": key,
            "image": image,
            "category": cat,
            "gpu_required": needs_gpu,
            "installed": hosted.contains(key),
            "image_present": present,
            "size_bytes": size,
            "warm": warm.contains(key),
            "runnable_here": !needs_gpu || gpu,
            "pinned": pinned_digest(image).is_some(),
        }));
    }
    Json(json!({ "gpu": gpu, "models": models }))
}

#[derive(Deserialize)]
struct ModelKey {
    key: String,
}

/// Install a model to host: pull its worker image (unless already present, e.g.
/// a locally-built one) and add it to the persisted hosted set so the node
/// announces and serves it.
async fn models_install(Json(b): Json<ModelKey>) -> Json<serde_json::Value> {
    let Some((key, image, needs_gpu, _cat)) = catalog_entry(&b.key) else {
        return Json(json!({ "ok": false, "error": "unknown model" }));
    };
    if !image_present(image).await {
        let _guard = worker_lock().lock().await;
        if let Err(e) = pull_worker(image).await {
            return Json(json!({
                "ok": false,
                "error": e.to_string(),
                "hint": "this worker image is not published yet",
            }));
        }
    }
    set_hosted(key, true);
    let size = image_size_bytes(image).await;
    Json(
        json!({ "ok": true, "key": key, "installed": true, "gpu_required": needs_gpu, "size_bytes": size }),
    )
}

/// Uninstall a hosted model: disable it, stop any running worker, and remove
/// its image to reclaim disk (unless another still-hosted model shares it).
async fn models_uninstall(Json(b): Json<ModelKey>) -> Json<serde_json::Value> {
    let Some((key, image, _, _)) = catalog_entry(&b.key) else {
        return Json(json!({ "ok": false, "error": "unknown model" }));
    };
    set_hosted(key, false);
    // Stop containers running this image (best-effort).
    if let Ok(o) = docker(&["ps", "-q", "--filter", &format!("ancestor={image}")]).await {
        for id in String::from_utf8_lossy(&o.stdout).split_whitespace() {
            let _ = docker(&["rm", "-f", id]).await;
        }
    }
    // Reclaim the image only if no other hosted key still needs it.
    let shared = hosted_models().iter().any(|k| {
        catalog_entry(k)
            .map(|(_, img, ..)| img == image)
            .unwrap_or(false)
    });
    let mut image_removed = false;
    if !shared {
        if let Ok(o) = docker(&["rmi", image]).await {
            image_removed = o.status.success();
        }
    }
    Json(json!({ "ok": true, "key": key, "image_removed": image_removed }))
}

/// Provider dashboard: node identity (distinct from this gateway's bridge id),
/// earnings + job count from the local receipts log, hosted models and disk.
async fn node_dashboard(State(s): State<Shared>) -> Json<serde_json::Value> {
    let node_id = cloudiy_common::load_or_create_node_key()
        .ok()
        .map(|k| k.public().to_string());
    let (jobs, earned_micro) = read_receipts_summary();

    let gpu = gpu_available().await;
    let warm: std::collections::HashSet<String> = warm_models().into_iter().collect();
    let mut hosted = Vec::new();
    let mut disk: u64 = 0;
    let mut counted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for key in hosted_models() {
        if let Some((k, image, needs_gpu, cat)) = catalog_entry(&key) {
            let present = image_present(image).await;
            let size = if present {
                image_size_bytes(image).await
            } else {
                None
            };
            if present && counted.insert(image) {
                disk += size.unwrap_or(0);
            }
            hosted.push(json!({
                "key": k,
                "image": image,
                "category": cat,
                "gpu_required": needs_gpu,
                "image_present": present,
                "size_bytes": size,
                "warm": warm.contains(k),
            }));
        }
    }
    let policy = load_policy();
    Json(json!({
        "node_id": node_id,
        "bridge_id": s.id,
        "gpu": gpu,
        "jobs": jobs,
        "earned_micro_usdc": earned_micro,
        "hosted": hosted,
        "hosted_count": hosted.len(),
        "disk_bytes": disk,
        "mode": policy.mode,
        "budget_bytes": policy.budget_bytes,
    }))
}

/// Summarize the local receipts log (~/.config/cloudiy/jobs.jsonl): number of
/// completed jobs and total earned in micro-USDC. Parses generically so it does
/// not couple to the receipt struct — sums any per-job `price_micro_usdc` /
/// `price_usdc` / `amount` field it finds.
fn read_receipts_summary() -> (u64, u64) {
    let path = cloudiy_common::config_dir().join("jobs.jsonl");
    let Ok(text) = std::fs::read_to_string(path) else {
        return (0, 0);
    };
    let mut jobs = 0u64;
    let mut micro = 0u64;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        jobs += 1;
        let job = v.get("job").unwrap_or(&v);
        if let Some(m) = job.get("price_micro_usdc").and_then(|x| x.as_u64()) {
            micro += m;
        } else if let Some(u) = job.get("price_usdc").and_then(|x| x.as_f64()) {
            micro += (u * 1_000_000.0) as u64;
        } else if let Some(a) = job.get("amount").and_then(|x| x.as_u64()) {
            micro += a;
        }
    }
    (jobs, micro)
}

// ---- phase 3: auto-hosting controller (disk budget) -----------------------

/// Provider hosting policy: manual (operator picks) or auto (the node fills a
/// disk budget with the most in-demand models). Persisted next to the hosted set.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct HostPolicy {
    /// "manual" | "auto"
    mode: String,
    /// Disk budget for auto-hosted models, in bytes (0 = unset).
    budget_bytes: u64,
}

impl Default for HostPolicy {
    fn default() -> Self {
        Self {
            mode: "manual".into(),
            budget_bytes: 0,
        }
    }
}

fn policy_path() -> std::path::PathBuf {
    cloudiy_common::config_dir().join("host_policy.json")
}

fn load_policy() -> HostPolicy {
    std::fs::read_to_string(policy_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_policy(p: &HostPolicy) {
    let _ = std::fs::create_dir_all(cloudiy_common::config_dir());
    if let Ok(s) = serde_json::to_string_pretty(p) {
        let _ = std::fs::write(policy_path(), s);
    }
}

/// In-memory install timestamps for min-residency (anti-thrash). Reset on
/// restart — it only bounds churn within a run, which is enough.
fn installed_at() -> &'static std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>
{
    static R: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
    > = std::sync::OnceLock::new();
    R.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Rough on-disk size estimate for budget planning before an image is pulled.
fn est_size(image: &str, category: &str) -> u64 {
    if image.contains("ollama") {
        return 5_000_000_000;
    }
    match category {
        "video" => 15_000_000_000,
        "image" => 12_000_000_000,
        "audio" => 5_500_000_000,
        _ => 3_000_000_000,
    }
}

/// Per-request price (USDC) for a catalog endpoint, mirroring os.html's
/// ENDPOINTS. Used to weight auto-hosting by expected revenue, not raw demand,
/// so the budget favours what actually earns. (RFC-0007 posts price in the
/// protocol; this table is the node-side default until quotes drive it.)
fn endpoint_price(key: &str) -> f64 {
    match key {
        "veo-fast" => 0.32,
        "kling" => 0.28,
        "hailuo-std" => 0.24,
        "vidu-i2v" => 0.21,
        "vidu-t2v" => 0.19,
        "hailuo-fast" => 0.18,
        "p-video" => 0.15,
        "stable-audio" => 0.06,
        "flux2" => 0.05,
        "nano-banana" => 0.04,
        "qwen-edit" => 0.03,
        "chatterbox" => 0.02,
        "sdxl" => 0.02,
        "z-image" => 0.012,
        "whisper-ep" => 0.006,
        "llama-ep" => 0.004,
        _ => 0.02,
    }
}

/// Minimum time an auto-installed model stays before it can be auto-evicted.
const MIN_RESIDENCY_SECS: u64 = 1800;

/// One auto-hosting cycle: poll demand, rank candidates by supply-adjusted
/// demand per byte, greedily fill the disk budget (manual pins always kept),
/// then install the newly admitted and uninstall the dropped (respecting
/// min-residency and warmth). Returns a JSON summary of what it did.
pub(crate) async fn autohost_cycle(endpoint: &iroh::Endpoint) -> serde_json::Value {
    let policy = load_policy();
    if policy.mode != "auto" || policy.budget_bytes == 0 {
        return json!({ "ran": false, "reason": "auto mode off or no budget" });
    }
    let gpu = gpu_available().await;

    // 1. Demand across directories: key -> best supply-adjusted score.
    let mut demand: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for dir in cloudiy_common::resolve_directories(vec![]) {
        if let Ok(Response::Demand(list)) = dial(endpoint, &dir, Request::Demand).await {
            for e in list {
                let s = e.recent_interest as f64 / (e.providers as f64 + 1.0);
                let cur = demand.entry(e.key).or_insert(0.0);
                *cur = cur.max(s);
            }
        }
    }

    // 2. Candidates: catalog entries runnable here, with demand, sized.
    struct Cand {
        key: &'static str,
        image: &'static str,
        size: u64,
        score: f64,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for (key, image, needs_gpu, cat) in model_catalog().iter().copied() {
        if needs_gpu && !gpu {
            continue;
        }
        let d = demand.get(key).copied().unwrap_or(0.0);
        if d <= 0.0 {
            continue;
        }
        // Expected-revenue signal = demand × per-request price; ranking then
        // divides by size (revenue density per byte of disk).
        let score = d * endpoint_price(key);
        let size = image_size_bytes(image)
            .await
            .unwrap_or_else(|| est_size(image, cat));
        cands.push(Cand {
            key,
            image,
            size,
            score,
        });
    }
    // Rank by demand-per-byte (knapsack density).
    cands.sort_by(|a, b| (b.score / b.size as f64).total_cmp(&(a.score / a.size as f64)));

    // 3. Fill the budget: manual pins first (always kept), then by density.
    // Manual pins = hosted keys the controller did NOT auto-install (so auto
    // models stay evictable and don't ossify into permanent pins).
    let auto_keys: std::collections::HashSet<String> = installed_at()
        .lock()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let manual: std::collections::HashSet<String> = hosted_models()
        .into_iter()
        .filter(|k| !auto_keys.contains(k))
        .collect();
    let mut target: std::collections::HashSet<String> = manual.clone();
    let mut used: u64 = 0;
    let mut counted: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // Count disk already committed by pins.
    for (key, image, _, cat) in model_catalog().iter().copied() {
        if manual.contains(key) && counted.insert(image) {
            used += image_size_bytes(image)
                .await
                .unwrap_or_else(|| est_size(image, cat));
        }
    }
    for c in &cands {
        if target.contains(c.key) {
            continue;
        }
        let incr = if counted.contains(c.image) { 0 } else { c.size };
        if used + incr <= policy.budget_bytes {
            target.insert(c.key.to_string());
            used += incr;
            counted.insert(c.image);
        }
    }

    // 4. Apply: install newly admitted, evict dropped (respecting residency).
    let mut installed = Vec::new();
    let mut evicted = Vec::new();
    let warm: std::collections::HashSet<String> = warm_models().into_iter().collect();
    let hosted_now: std::collections::HashSet<String> = hosted_models().into_iter().collect();

    for key in target.difference(&hosted_now) {
        if let Some((k, image, _, _)) = catalog_entry(key) {
            if !image_present(image).await && pull_worker(image).await.is_err() {
                continue; // e.g. not published yet — skip quietly
            }
            set_hosted(k, true);
            installed_at()
                .lock()
                .map(|mut m| m.insert(k.to_string(), std::time::Instant::now()))
                .ok();
            installed.push(k);
        }
    }
    for key in hosted_now.difference(&target) {
        // Never auto-evict a manual pin, a warm model, or one still in residency.
        if warm.contains(key) {
            continue;
        }
        let young = installed_at()
            .lock()
            .ok()
            .and_then(|m| {
                m.get(key)
                    .map(|t| t.elapsed().as_secs() < MIN_RESIDENCY_SECS)
            })
            .unwrap_or(false);
        if young {
            continue;
        }
        if let Some((k, image, _, _)) = catalog_entry(key) {
            set_hosted(k, false);
            installed_at().lock().map(|mut m| m.remove(k)).ok();
            let shared = hosted_models().iter().any(|h| {
                catalog_entry(h)
                    .map(|(_, img, ..)| img == image)
                    .unwrap_or(false)
            });
            if !shared {
                let _ = docker(&["rmi", image]).await;
            }
            evicted.push(k);
        }
    }

    json!({
        "ran": true,
        "budget_bytes": policy.budget_bytes,
        "planned_disk_bytes": used,
        "installed": installed,
        "evicted": evicted,
        "target": target.iter().collect::<Vec<_>>(),
    })
}

#[derive(Deserialize)]
struct PolicyBody {
    mode: Option<String>,
    budget_bytes: Option<u64>,
}

/// Set the hosting policy (mode + disk budget) from My Nodes.
async fn set_policy(Json(b): Json<PolicyBody>) -> Json<serde_json::Value> {
    let mut p = load_policy();
    if let Some(m) = b.mode {
        if m == "manual" || m == "auto" {
            p.mode = m;
        }
    }
    if let Some(budget) = b.budget_bytes {
        p.budget_bytes = budget;
    }
    save_policy(&p);
    Json(json!({ "ok": true, "mode": p.mode, "budget_bytes": p.budget_bytes }))
}

/// Run one auto-hosting cycle now (My Nodes "apply", and a test hook).
async fn autohost_run(State(s): State<Shared>) -> Json<serde_json::Value> {
    Json(autohost_cycle(&s.endpoint).await)
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

/// Real rentable machines: the verified provider announcements from the
/// gateway's directory (resolved from `--via` config / `CLOUDIY_DIRECTORY` /
/// the compiled default), shaped as whole machines for the Hardware Store. A
/// provider *is* a machine, so each entry is a specific node you lease. Empty
/// when no directory is configured or none is reachable — the UI then shows
/// example listings instead.
async fn get_machines(State(s): State<Shared>) -> Json<serde_json::Value> {
    use cloudiy_protocol::ResourceKind::{Cpu, Gpu, Memory, Storage, Vram};
    let dirs = cloudiy_common::resolve_directories(vec![]);
    if dirs.is_empty() {
        return Json(json!({ "machines": [] }));
    }
    let now = chrono::Utc::now().timestamp();
    let mut machines = Vec::new();
    for dir in dirs {
        if let Ok(Response::Providers(list)) = rpc(&s, &dir, Request::Providers).await {
            for sa in list {
                if let Ok(p) = cloudiy_common::verify_announcement(&sa, now) {
                    // What the node offers to the network.
                    let r = &p.resources.shared;
                    machines.push(json!({
                        "node": p.identity.as_str(),
                        "cpu_cores": (r.get(&Cpu) as f64 / 1000.0),   // millicores -> cores
                        "memory_mb": r.get(&Memory),
                        "vram_mb": r.get(&Vram),
                        "storage_mb": r.get(&Storage),
                        "has_gpu": r.get(&Gpu) > 0 || r.get(&Vram) > 0,
                        "price_per_hour": p.price_micro_usdc_per_hour as f64 / 1_000_000.0,
                        "utilization": p.utilization,
                        "region": p.region,
                    }));
                }
            }
        }
    }
    Json(json!({ "machines": machines }))
}

#[derive(Deserialize)]
struct EndpointProvidersQuery {
    /// Directory node to list announcements from. Omitted: the gateway's
    /// configured directories (CLOUDIY_DIRECTORY / compiled default).
    #[serde(default)]
    via: Option<String>,
    /// Catalog model key to filter by (e.g. `flux2`).
    key: String,
}

/// Verified providers that announce serving `key`, gathered from the given (or
/// configured) directories, plus `best` per [`select_endpoint_provider`].
async fn discover_endpoint_providers(
    s: &Gateway,
    via: Option<String>,
    key: &str,
) -> (Vec<cloudiy_protocol::ProviderAnnouncement>, Option<String>) {
    let dirs = match via.filter(|v| !v.is_empty()) {
        Some(v) => vec![v],
        None => cloudiy_common::resolve_directories(vec![]),
    };
    let now = chrono::Utc::now().timestamp();
    let mut verified: Vec<cloudiy_protocol::ProviderAnnouncement> = Vec::new();
    for dir in dirs {
        // Demand oracle (phase 2): signal that a consumer is looking for `key`,
        // so the directory can tell providers what to auto-host. Best-effort.
        let _ = rpc(
            s,
            &dir,
            Request::EndpointInterest {
                key: key.to_string(),
            },
        )
        .await;
        if let Ok(Response::Providers(list)) = rpc(s, &dir, Request::Providers).await {
            verified.extend(
                list.into_iter()
                    .filter_map(|sa| cloudiy_common::verify_announcement(&sa, now).ok())
                    .filter(|p| p.served_models.iter().any(|m| m == key)),
            );
        }
    }
    let best = select_endpoint_provider(&verified, key).map(|p| p.identity.as_str().to_string());
    (verified, best)
}

/// Providers that can serve a given model endpoint, verified and filtered, with
/// `best` = the EndpointId to route to (warm first, then least-utilized, then
/// cheapest). The UI uses `best` as the `to` for a routed run.
async fn endpoint_providers(
    State(s): State<Shared>,
    Query(q): Query<EndpointProvidersQuery>,
) -> Json<serde_json::Value> {
    let (providers, best) = discover_endpoint_providers(&s, q.via, &q.key).await;
    Json(json!({ "providers": providers, "best": best }))
}

#[derive(Deserialize)]
struct ViaOpt {
    #[serde(default)]
    via: Option<String>,
}

/// Demand oracle (phase 2): merge the demand table from the configured
/// directories and rank by a supply-adjusted score. The auto-hosting controller
/// (phase 3) and My Nodes both read this to decide what is worth hosting.
async fn get_demand(State(s): State<Shared>, Query(q): Query<ViaOpt>) -> Json<serde_json::Value> {
    let dirs = match q.via.filter(|v| !v.is_empty()) {
        Some(v) => vec![v],
        None => cloudiy_common::resolve_directories(vec![]),
    };
    // Merge across directories: sum interest, take max supply seen.
    let mut merged: std::collections::HashMap<String, (u32, u32)> =
        std::collections::HashMap::new();
    for dir in dirs {
        if let Ok(Response::Demand(list)) = rpc(&s, &dir, Request::Demand).await {
            for e in list {
                let ent = merged.entry(e.key).or_insert((0, 0));
                ent.0 += e.recent_interest;
                ent.1 = ent.1.max(e.providers);
            }
        }
    }
    let mut rows: Vec<(String, u32, u32, f64)> = merged
        .into_iter()
        .map(|(key, (interest, providers))| {
            let score = interest as f64 / (providers as f64 + 1.0);
            (key, interest, providers, score)
        })
        .collect();
    rows.sort_by(|a, b| b.3.total_cmp(&a.3));
    let demand: Vec<_> = rows
        .into_iter()
        .map(|(key, interest, providers, score)| {
            json!({ "key": key, "recent_interest": interest, "providers": providers, "score": score })
        })
        .collect();
    Json(json!({ "demand": demand }))
}

#[derive(Deserialize)]
struct QuoteQuery {
    /// Catalog model key to quote (e.g. `flux2`).
    key: String,
    /// Provider to quote; omitted = the best provider for `key` by discovery.
    #[serde(default)]
    to: Option<String>,
}

/// x402 quote for running a model endpoint: resolves a provider (explicit `to`
/// or discovery `best`) and returns everything the browser needs to fund an
/// escrow for it — payout wallet, node key, USDC mint, escrow program, price.
async fn get_quote(
    State(s): State<Shared>,
    Query(q): Query<QuoteQuery>,
) -> Json<serde_json::Value> {
    let target = match q.to.filter(|t| !t.is_empty()) {
        Some(t) => Some(t),
        None => discover_endpoint_providers(&s, None, &q.key).await.1,
    };
    let Some(node) = target else {
        return err("no provider is announcing this model right now");
    };
    match rpc(&s, &node, Request::Info).await {
        Ok(Response::Info(i)) => Json(json!({
            "node": node,
            "price_usdc": i.price_usdc,
            "usdc_mint": i.usdc_mint,
            "escrow_program": i.escrow_program,
            "payout": i.solana_pubkey,
            "node_key": i.endpoint_id,
            "network": i.network,
        })),
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
        // Language endpoints from os.html's ENDPOINTS list.
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
fn audio_worker_for(_key: &str) -> Option<(&'static str, &'static str)> {
    // Placeholder for future audio models still awaiting a worker image. The
    // served ones (whisper-ep, chatterbox, stable-audio) are handled directly
    // in `serve_endpoint`; `audio_pending` covers anything not yet wired.
    None
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

/// Hardening applied to every model-worker container so a compromised model or
/// prompt has minimal blast radius on the provider host: drop ALL Linux
/// capabilities, forbid privilege escalation, and bound the process count
/// (anti fork-bomb). Returned as flags to splice into a `docker run`.
fn worker_hardening() -> Vec<&'static str> {
    vec![
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "512",
    ]
}

/// True when the operator asked for egress-less workers.
fn sealed_mode() -> bool {
    std::env::var("CLOUDIY_WORKER_NO_EGRESS").is_ok()
}

/// A dedicated Docker network with NO route to the internet. Serving a worker on
/// it cuts all outbound (no exfil/callback) while the host, and therefore the
/// gateway, can still reach the container's published port — unlike
/// `--network none`, which would also cut the gateway off. Egress-less serving
/// requires the model to be present already (weights baked, or warmed once by a
/// non-sealed run), since on-demand pulls need egress.
const SEALED_NET: &str = "cloudiy-sealed";
/// Persistent model cache so pulled weights survive restarts and can be warmed
/// once for sealed serving.
const OLLAMA_VOLUME: &str = "cloudiy-ollama-models";

/// Create the egress-less network (idempotent — a second create just errors,
/// which we ignore). Only needed in sealed mode.
async fn ensure_sealed_network() {
    let _ = docker(&["network", "create", "--internal", SEALED_NET]).await;
}

/// Network flags for a worker `docker run`: the sealed (internal) network when
/// egress is off, otherwise Docker's default. Splice into the run args.
fn worker_network() -> Vec<&'static str> {
    if sealed_mode() {
        vec!["--network", SEALED_NET]
    } else {
        vec![]
    }
}

// ---- supply chain: pin worker images; optional custom seccomp -------------

/// Resolve a worker image: an operator can override the default via env
/// (e.g. `CLOUDIY_OLLAMA_IMAGE=ollama/ollama@sha256:...`) to pin a reviewed
/// digest without recompiling.
fn worker_image_ref(env_key: &str, default: &str) -> String {
    std::env::var(env_key).unwrap_or_else(|_| default.to_string())
}

/// An image reference is pinned when it names an immutable content digest
/// (`repo@sha256:...`) rather than a mutable tag.
fn image_is_pinned(image: &str) -> bool {
    image.contains("@sha256:")
}

/// Fail closed when `CLOUDIY_REQUIRE_PINNED_IMAGES` is set and a worker image
/// isn't pinned by digest — a mutable tag (`:latest`) could be repointed at a
/// malicious image between review and run; a digest can't.
fn check_pinned(image: &str) -> anyhow::Result<()> {
    // A manifest entry counts as pinned: install pulls it by digest.
    if std::env::var("CLOUDIY_REQUIRE_PINNED_IMAGES").is_ok()
        && !image_is_pinned(image)
        && pinned_digest(image).is_none()
    {
        anyhow::bail!(
            "CLOUDIY_REQUIRE_PINNED_IMAGES is set but worker image '{image}' is not pinned \
             by digest (@sha256:...); set the matching CLOUDIY_*_IMAGE env to a pinned ref"
        );
    }
    Ok(())
}

/// `--security-opt seccomp=<file>` when the operator supplies a validated
/// profile via `CLOUDIY_SECCOMP_PROFILE`. Otherwise Docker's default seccomp
/// profile stays in force — we never pass `unconfined`.
fn seccomp_arg() -> Option<String> {
    std::env::var("CLOUDIY_SECCOMP_PROFILE")
        .ok()
        .map(|p| format!("seccomp={p}"))
}

/// Optional non-root user for worker containers (`CLOUDIY_WORKER_USER`, e.g.
/// `1000:1000`). Off by default because several worker images assume root
/// (they write to `/root`, the HF cache, …); enable only with an image and
/// volume prepared for a non-root UID.
fn worker_user() -> Option<String> {
    std::env::var("CLOUDIY_WORKER_USER")
        .ok()
        .filter(|u| !u.is_empty())
}

/// Verify a worker image's signature with cosign before running it, when
/// `CLOUDIY_COSIGN_VERIFY` is set (fail closed). Supports a public key
/// (`CLOUDIY_COSIGN_KEY`) or keyless verification (`CLOUDIY_COSIGN_IDENTITY` +
/// `CLOUDIY_COSIGN_ISSUER`). No-op when disabled, so the default path never
/// needs cosign installed.
async fn verify_image_signature(image: &str) -> anyhow::Result<()> {
    if std::env::var("CLOUDIY_COSIGN_VERIFY").is_err() {
        return Ok(());
    }
    let mut cmd = tokio::process::Command::new("cosign");
    cmd.arg("verify");
    if let Ok(key) = std::env::var("CLOUDIY_COSIGN_KEY") {
        cmd.args(["--key", &key]);
    } else if let (Ok(id), Ok(iss)) = (
        std::env::var("CLOUDIY_COSIGN_IDENTITY"),
        std::env::var("CLOUDIY_COSIGN_ISSUER"),
    ) {
        cmd.args([
            "--certificate-identity",
            &id,
            "--certificate-oidc-issuer",
            &iss,
        ]);
    } else {
        anyhow::bail!(
            "CLOUDIY_COSIGN_VERIFY is set but no verifier configured (set CLOUDIY_COSIGN_KEY, \
             or CLOUDIY_COSIGN_IDENTITY + CLOUDIY_COSIGN_ISSUER)"
        );
    }
    cmd.arg(image);
    let out = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("cosign not runnable (is it installed?): {e}"))?;
    anyhow::ensure!(
        out.status.success(),
        "cosign signature verification failed for '{image}': {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
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
        if sealed_mode() {
            ensure_sealed_network().await;
        }
        let publish = format!("127.0.0.1:{OLLAMA_PORT}:11434");
        // Persistent model cache so weights survive restarts (and can be warmed
        // once for sealed serving).
        let volume = format!("{OLLAMA_VOLUME}:/root/.ollama");
        let image = worker_image_ref("CLOUDIY_OLLAMA_IMAGE", "ollama/ollama");
        check_pinned(&image)?;
        verify_image_signature(&image).await?;
        let sec = seccomp_arg();
        let user = worker_user();
        let mut args: Vec<&str> = vec!["run", "-d"];
        args.extend(worker_hardening());
        args.extend(worker_network());
        if let Some(s) = &sec {
            args.extend(["--security-opt", s.as_str()]);
        }
        if let Some(u) = &user {
            args.extend(["--user", u.as_str()]);
        }
        // Read-only root fs: the only writable surfaces are the model volume
        // (/root/.ollama) and a tmpfs /tmp — a compromised model can't tamper
        // with the image. Plus a generous RAM cap so it can't exhaust host
        // memory (llama3.2:1b needs a few GB).
        args.extend([
            "--read-only",
            "--tmpfs",
            "/tmp",
            "--memory",
            "8g",
            "--name",
            OLLAMA_WORKER,
            "-p",
            publish.as_str(),
            "-v",
            volume.as_str(),
            image.as_str(),
        ]);
        let out = docker(&args).await?;
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
        // A pull needs egress; in sealed mode the worker has none. Warm the
        // model once with a non-sealed run, then re-enable sealed serving —
        // the weights persist in the cache volume.
        anyhow::ensure!(
            !sealed_mode(),
            "sealed mode (CLOUDIY_WORKER_NO_EGRESS): model '{model}' is not warmed. \
             Run once without the flag to cache it, then re-enable sealed serving."
        );
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
    /// Job id (UUID) binding this run to a funded escrow — must match the
    /// job_id the escrow was created with, or on-chain verification fails.
    #[serde(default)]
    job_id: Option<String>,
    /// Base64 audio for speech-to-text endpoints (uploads run on this
    /// gateway host; they are not routed over the protocol frame).
    #[serde(default)]
    audio_b64: Option<String>,
}

/// HTTP entry point for a model run. With `to`, routes to a remote provider
/// over iroh (that node runs the worker and settles payment); otherwise runs
/// the model on this gateway host via [`serve_endpoint`].
async fn run_endpoint(
    State(s): State<Shared>,
    Json(b): Json<EndpointBody>,
) -> Json<serde_json::Value> {
    let has_audio = b.audio_b64.as_deref().is_some_and(|a| !a.is_empty());
    if b.prompt.trim().is_empty() && !has_audio {
        return err("prompt is required");
    }

    // Audio uploads run locally; prompt-only runs may route to a provider.
    if let Some(to) = b.to.as_deref().filter(|t| !t.is_empty() && !has_audio) {
        let mut request = req(b.token, b.payment);
        if let Some(jid) = b.job_id.filter(|j| !j.is_empty()) {
            // Bind to the funded escrow's job id (A1/A2 checks provider-side).
            request.job_id = jid;
        }
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
                    .unwrap_or_else(
                        |_| json!({ "output": String::from_utf8_lossy(&r.output_data) }),
                    );
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

    Json(serve_endpoint(&b.key, &b.prompt, b.audio_b64.as_deref()).await)
}

/// Streaming variant of [`run_endpoint`] for local text models: relays the
/// Ollama worker's tokens to the browser as newline-delimited JSON as they are
/// produced, so a long answer appears progressively instead of only after the
/// full response completes. Only the local text path streams; audio, image and
/// remote-provider runs go through [`run_endpoint`] (single response). The same
/// anti-CSRF / loopback guard applies (it is global middleware).
///
/// Wire format (one JSON object per line):
/// - `{"cold_start":bool,"model":"…"}`   preamble (before the first token)
/// - `{"token":"…"}`                      each generated token
/// - `{"done":true,"tokens":N,"model":…}` final marker
/// - `{"error":"…","done":true}`          on failure
async fn run_endpoint_stream(
    State(_s): State<Shared>,
    Json(b): Json<EndpointBody>,
) -> axum::response::Response {
    use tokio_stream::StreamExt as _;

    // Streamable only for a local text model: no remote `to`, no audio.
    let has_audio = b.audio_b64.as_deref().is_some_and(|a| !a.is_empty());
    let remote = b.to.as_deref().is_some_and(|t| !t.is_empty());
    let model = match ollama_model_for(&b.key) {
        Some(m) if !remote && !has_audio => m,
        _ => {
            return (
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "streaming is only for local text models; use /api/endpoint"
                })),
            )
                .into_response();
        }
    };
    if b.prompt.trim().is_empty() {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({ "error": "prompt is required" })),
        )
            .into_response();
    }
    const MAX_PROMPT_BYTES: usize = 16 * 1024;
    if b.prompt.len() > MAX_PROMPT_BYTES {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("prompt too large ({} bytes; max {MAX_PROMPT_BYTES})", b.prompt.len())
            })),
        )
            .into_response();
    }

    // Provision the worker (serialized) before streaming starts.
    let cold = {
        let _guard = worker_lock().lock().await;
        match ensure_ollama(model).await {
            Ok(c) => c,
            Err(e) => {
                return Json(json!({ "error": format!("provisioning failed: {e}") }))
                    .into_response();
            }
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(64);
    let model = model.to_string();
    let key = b.key.clone();
    let prompt = b.prompt.clone();
    tokio::spawn(async move {
        // Preamble: cold-start flag + model so the UI can show "warming up"
        // before the first token.
        let _ = tx
            .send(Ok(format!(
                "{}\n",
                json!({ "cold_start": cold, "model": model.as_str() })
            )))
            .await;

        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{OLLAMA_URL}/api/generate"))
            .json(&json!({
                "model": model.as_str(), "prompt": prompt.as_str(), "stream": true,
                "options": { "num_predict": text_max_tokens() }
            }))
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                let msg = if e.is_timeout() {
                    "inference timed out: the model is running on CPU. Try a shorter \
                     prompt, or serve this model on a GPU node."
                        .to_string()
                } else {
                    format!("inference failed: {e}")
                };
                let _ = tx
                    .send(Ok(format!("{}\n", json!({ "error": msg, "done": true }))))
                    .await;
                return;
            }
        };

        // Ollama streams one JSON object per line; reassemble across chunk
        // boundaries and re-emit just the token text under our own schema.
        let mut stream = resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut total: u64 = 0;
        'outer: while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(_) => break,
            };
            buf.extend_from_slice(&chunk);
            while let Some(pos) = buf.iter().position(|&x| x == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                    continue;
                };
                if let Some(tok) = v.get("response").and_then(|t| t.as_str()) {
                    if !tok.is_empty()
                        && tx
                            .send(Ok(format!("{}\n", json!({ "token": tok }))))
                            .await
                            .is_err()
                    {
                        break 'outer; // client hung up
                    }
                }
                if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    total = v
                        .get("eval_count")
                        .and_then(|c| c.as_u64())
                        .unwrap_or(total);
                    let _ = tx
                        .send(Ok(format!(
                            "{}\n",
                            json!({
                                "done": true, "tokens": total,
                                "model": model.as_str(), "settled_via": "local-node"
                            })
                        )))
                        .await;
                    mark_warm(&key);
                    return;
                }
            }
        }
        // Ended without an explicit done (cutoff / dropped connection).
        let _ = tx
            .send(Ok(format!(
                "{}\n",
                json!({
                    "done": true, "tokens": total,
                    "model": model.as_str(), "settled_via": "local-node"
                })
            )))
            .await;
        mark_warm(&key);
    });

    let body = axum::body::Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx));
    axum::response::Response::builder()
        .header("content-type", "application/x-ndjson")
        .header("cache-control", "no-store")
        .body(body)
        .map(axum::response::IntoResponse::into_response)
        .unwrap_or_else(|_| {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "stream error",
            )
                .into_response()
        })
}

/// Run a catalog model on THIS host and return its output as a JSON value.
/// Shared by the local gateway path and by a provider node serving a remote
/// [`Request::RunEndpoint`]. Text runs on a local Ollama worker (real today);
/// speech-to-text runs on a public CPU Whisper worker when `audio` is given;
/// image/video need an NVIDIA GPU and report honestly when absent. Always
/// returns a value (errors carry an `"error"` field) so the caller can relay
/// it verbatim.
pub(crate) async fn serve_endpoint(
    key: &str,
    prompt: &str,
    audio_b64: Option<&str>,
) -> serde_json::Value {
    let audio_b64 = audio_b64.filter(|a| !a.is_empty());
    if prompt.trim().is_empty() && audio_b64.is_none() {
        return json!({ "error": "prompt is required" });
    }
    // Input cap: bound prompt size so an adversarial request can't blow up
    // memory or the model's context before the worker even runs.
    const MAX_PROMPT_BYTES: usize = 16 * 1024;
    if prompt.len() > MAX_PROMPT_BYTES {
        return json!({
            "error": format!("prompt too large ({} bytes; max {MAX_PROMPT_BYTES})", prompt.len())
        });
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
            // Cap output tokens so a request can't pin the worker generating
            // forever (a resource-exhaustion vector). Default 256 is sized so a
            // full response completes inside the 300s timeout even on a slow
            // CPU (~1 tok/s in a macOS Docker VM), rather than being cut off
            // mid-answer. Operators on faster hardware (GPU) can raise it with
            // CLOUDIY_TEXT_MAX_TOKENS.
            .json(&json!({
                "model": model, "prompt": prompt, "stream": false,
                "options": { "num_predict": text_max_tokens() }
            }))
            // Generous timeout for CPU inference: a 1b model on CPU runs at a
            // few tok/s, so a long answer needs minutes. 300s (the platform
            // default) fits a capped 512-token response with headroom.
            .timeout(std::time::Duration::from_secs(300))
            .send()
            .await;
        let body: serde_json::Value = match resp {
            Ok(r) => match r.json().await {
                Ok(v) => v,
                Err(e) => return json!({ "error": format!("bad model response: {e}") }),
            },
            Err(e) if e.is_timeout() => {
                return json!({ "error": "inference timed out: the model is running \
                    on CPU and this answer took too long. Try a shorter prompt, or \
                    serve this model on a GPU node." });
            }
            Err(e) => return json!({ "error": format!("inference failed: {e}") }),
        };
        mark_warm(key);
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
            Ok(v) => {
                mark_warm(key);
                v
            }
            Err(e) => json!({ "error": format!("image inference failed: {e}") }),
        };
    }

    // Video: GPU-gated like image, but delivered as a file URL.
    if let Some(worker_image) = video_worker_for(key) {
        if !gpu_available().await {
            return gpu_required(key, worker_image, "video");
        }
        return match run_video_worker(worker_image, prompt).await {
            Ok(v) => {
                mark_warm(key);
                v
            }
            Err(e) => json!({ "error": format!("video inference failed: {e}") }),
        };
    }

    // Audio endpoints. Whisper (speech-to-text) runs today on a public CPU
    // worker when the caller supplies audio; TTS runs on the cloudiy Piper
    // worker (CPU) once its image is published; audio *generation*
    // (stable-audio) still awaits a worker and reports honestly.
    match key {
        "whisper-ep" => {
            use base64::Engine as _;
            let Some(a) = audio_b64 else {
                return json!({
                    "error": "whisper transcribes audio — attach an audio file (audio_b64)",
                    "needs": "audio-input",
                });
            };
            // ~24 MB of base64 ≈ 18 MB of audio, matching the catalog's cap.
            if a.len() > 24 * 1024 * 1024 {
                return json!({ "error": "audio too large (max ~18 MB)" });
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(a) else {
                return json!({ "error": "audio_b64 is not valid base64" });
            };
            return match run_whisper_worker(&bytes).await {
                Ok(v) => {
                    mark_warm(key);
                    v
                }
                Err(e) => json!({ "error": format!("transcription failed: {e}") }),
            };
        }
        "chatterbox" => {
            return match run_tts_worker(prompt).await {
                Ok(v) => {
                    mark_warm(key);
                    v
                }
                Err(e) => json!({ "error": format!("tts failed: {e}") }),
            };
        }
        "stable-audio" => {
            return match run_audio_worker(prompt).await {
                Ok(v) => {
                    mark_warm(key);
                    v
                }
                Err(e) => json!({ "error": format!("audio generation failed: {e}") }),
            };
        }
        _ => {}
    }
    if let Some((worker, task)) = audio_worker_for(key) {
        return audio_pending(key, worker, task);
    }

    json!({ "error": format!("unknown endpoint '{key}'") })
}

/// Honest response for an audio model while no audio worker node is serving it.
fn audio_pending(key: &str, worker: &str, task: &str) -> serde_json::Value {
    let error = format!(
        "'{key}' is an audio {task} model — awaiting an audio worker node \
         ({worker}). Run `cloudiy share` on a node serving it."
    );
    json!({ "error": error, "needs": "audio-worker", "worker": worker })
}

const WHISPER_WORKER: &str = "cloudiy-wk-whisper";
const WHISPER_PORT: &str = "9977";
const WHISPER_URL: &str = "http://127.0.0.1:9977";

/// Max output tokens for a local text generation, overridable by env
/// (CLOUDIY_TEXT_MAX_TOKENS). Default 256 keeps a full answer inside the
/// request timeout on a slow CPU; raise it on GPU hardware. Clamped to a sane
/// range so a bad value can't disable the cap (resource-exhaustion guard).
fn text_max_tokens() -> u32 {
    std::env::var("CLOUDIY_TEXT_MAX_TOKENS")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .map(|n| n.clamp(16, 8192))
        .unwrap_or(256)
}

/// Whisper model size, overridable by env. `base` is the default (good CPU
/// latency/quality trade-off); `small`/`medium`/`large-v3` trade more RAM and
/// download for accuracy, `tiny` for speed. Only the known sizes are accepted
/// so a typo can't silently boot a broken worker.
fn whisper_model() -> String {
    let m = std::env::var("CLOUDIY_WHISPER_MODEL")
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_default();
    const KNOWN: &[&str] = &[
        "tiny",
        "tiny.en",
        "base",
        "base.en",
        "small",
        "small.en",
        "medium",
        "medium.en",
        "large",
        "large-v1",
        "large-v2",
        "large-v3",
        "turbo",
    ];
    if KNOWN.contains(&m.as_str()) {
        m
    } else {
        "base".to_string()
    }
}

/// Provision the Whisper ASR worker (a public CPU image, so it is real today
/// like the Ollama text worker) and transcribe one audio clip.
async fn run_whisper_worker(audio: &[u8]) -> anyhow::Result<serde_json::Value> {
    let want_model = whisper_model();
    let cold = {
        let _guard = worker_lock().lock().await;
        let running = docker(&["inspect", "-f", "{{.State.Running}}", WHISPER_WORKER])
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        // If a worker is already up but for a different model size, it is stale
        // (env changed since it booted) — recreate it so we serve what we
        // report rather than the old model.
        let model_matches = !running
            || docker(&[
                "inspect",
                "-f",
                "{{range .Config.Env}}{{println .}}{{end}}",
                WHISPER_WORKER,
            ])
            .await
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|l| l == format!("ASR_MODEL={want_model}"))
            })
            .unwrap_or(false);
        if !running || !model_matches {
            let _ = docker(&["rm", "-f", WHISPER_WORKER]).await;
            if sealed_mode() {
                ensure_sealed_network().await;
            }
            let image = worker_image_ref(
                "CLOUDIY_WHISPER_IMAGE",
                "onerahmet/openai-whisper-asr-webservice:latest",
            );
            check_pinned(&image)?;
            verify_image_signature(&image).await?;
            let sec = seccomp_arg();
            let user = worker_user();
            let publish = format!("127.0.0.1:{WHISPER_PORT}:9000");
            // Model size is env-overridable (CLOUDIY_WHISPER_MODEL); larger
            // models want more RAM, so scale the cap with the choice.
            let asr_env = format!("ASR_MODEL={want_model}");
            let mem = match want_model.as_str() {
                "large" | "large-v1" | "large-v2" | "large-v3" => "10g",
                "medium" | "medium.en" => "6g",
                _ => "4g",
            };
            let mut args: Vec<&str> = vec!["run", "-d"];
            args.extend(worker_hardening());
            args.extend(worker_network());
            if let Some(s) = &sec {
                args.extend(["--security-opt", s.as_str()]);
            }
            if let Some(u) = &user {
                args.extend(["--user", u.as_str()]);
            }
            args.extend([
                "--memory",
                mem,
                "-e",
                asr_env.as_str(),
                "--name",
                WHISPER_WORKER,
                "-p",
                publish.as_str(),
                image.as_str(),
            ]);
            let out = docker(&args).await?;
            anyhow::ensure!(
                out.status.success(),
                "worker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            // First boot downloads the model — wait generously, bounded polls.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            for _ in 0..90 {
                if client
                    .get(format!("{WHISPER_URL}/openapi.json"))
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

    // Hand-rolled multipart (reqwest's multipart feature isn't enabled).
    let boundary = "cloudiy-b7f3a19c4e";
    let mut body = Vec::with_capacity(audio.len() + 256);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"audio_file\"; \
             filename=\"audio\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{WHISPER_URL}/asr?encode=true&task=transcribe&output=json"
        ))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(body)
        .timeout(std::time::Duration::from_secs(300))
        .send()
        .await?;
    let v: serde_json::Value = resp.json().await?;
    let text = v["text"].as_str().unwrap_or_default().trim().to_string();
    anyhow::ensure!(!text.is_empty(), "worker returned no transcription: {v}");
    Ok(json!({
        "kind": "text",
        "output": text,
        "model": format!("whisper {want_model} (CPU)"),
        "cold_start": cold,
        "settled_via": "local-node",
    }))
}

const TTS_WORKER: &str = "cloudiy-wk-tts";
const TTS_PORT: &str = "9978";
const TTS_URL: &str = "http://127.0.0.1:9978";

/// Provision the TTS worker (CPU, Piper — workers/tts) and synthesize one
/// clip, returned as base64 WAV. Until the image is published to GHCR the
/// docker pull fails with a clear error rather than a fake result.
async fn run_tts_worker(prompt: &str) -> anyhow::Result<serde_json::Value> {
    let cold = {
        let _guard = worker_lock().lock().await;
        let running = docker(&["inspect", "-f", "{{.State.Running}}", TTS_WORKER])
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        if !running {
            let _ = docker(&["rm", "-f", TTS_WORKER]).await;
            if sealed_mode() {
                ensure_sealed_network().await;
            }
            let image = worker_image_ref("CLOUDIY_TTS_IMAGE", "ghcr.io/cloudiy/worker-tts:latest");
            check_pinned(&image)?;
            verify_image_signature(&image).await?;
            let sec = seccomp_arg();
            let user = worker_user();
            let publish = format!("127.0.0.1:{TTS_PORT}:8000");
            let mut args: Vec<&str> = vec!["run", "-d"];
            args.extend(worker_hardening());
            args.extend(worker_network());
            if let Some(s) = &sec {
                args.extend(["--security-opt", s.as_str()]);
            }
            if let Some(u) = &user {
                args.extend(["--user", u.as_str()]);
            }
            args.extend([
                "--memory",
                "2g",
                "--name",
                TTS_WORKER,
                "-p",
                publish.as_str(),
                image.as_str(),
            ]);
            let out = docker(&args).await?;
            anyhow::ensure!(
                out.status.success(),
                "worker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            for _ in 0..45 {
                if client
                    .get(format!("{TTS_URL}/health"))
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
    let v: serde_json::Value = client
        .post(format!("{TTS_URL}/tts"))
        .json(&json!({ "text": prompt }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await?
        .json()
        .await?;
    let wav = v["wav_b64"].as_str().unwrap_or_default().to_string();
    anyhow::ensure!(!wav.is_empty(), "worker returned no audio: {v}");
    Ok(json!({
        "kind": "audio",
        "audio_b64": wav,
        "model": "piper (CPU)",
        "cold_start": cold,
        "settled_via": "local-node",
    }))
}

const AUDIO_WORKER: &str = "cloudiy-wk-audio";
const AUDIO_PORT: &str = "9979";
const AUDIO_URL: &str = "http://127.0.0.1:9979";

/// Provision the audio worker (CPU, MusicGen — workers/audio) and generate one
/// clip from a text prompt, returned as base64 WAV. CPU-capable, so it serves
/// the `stable-audio` endpoint on any node. Until the image is published to
/// GHCR the docker run fails with a clear error rather than a fake result.
async fn run_audio_worker(prompt: &str) -> anyhow::Result<serde_json::Value> {
    let cold = {
        let _guard = worker_lock().lock().await;
        let running = docker(&["inspect", "-f", "{{.State.Running}}", AUDIO_WORKER])
            .await
            .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "true")
            .unwrap_or(false);
        if !running {
            let _ = docker(&["rm", "-f", AUDIO_WORKER]).await;
            if sealed_mode() {
                ensure_sealed_network().await;
            }
            let image =
                worker_image_ref("CLOUDIY_AUDIO_IMAGE", "ghcr.io/cloudiy/worker-audio:latest");
            check_pinned(&image)?;
            verify_image_signature(&image).await?;
            let sec = seccomp_arg();
            let user = worker_user();
            let publish = format!("127.0.0.1:{AUDIO_PORT}:8000");
            let mut args: Vec<&str> = vec!["run", "-d"];
            args.extend(worker_hardening());
            args.extend(worker_network());
            if let Some(s) = &sec {
                args.extend(["--security-opt", s.as_str()]);
            }
            if let Some(u) = &user {
                args.extend(["--user", u.as_str()]);
            }
            // MusicGen wants a couple GB of RAM for the small model on CPU.
            args.extend([
                "--memory",
                "6g",
                "--name",
                AUDIO_WORKER,
                "-p",
                publish.as_str(),
                image.as_str(),
            ]);
            let out = docker(&args).await?;
            anyhow::ensure!(
                out.status.success(),
                "worker start failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .unwrap_or_default();
            for _ in 0..60 {
                if client
                    .get(format!("{AUDIO_URL}/health"))
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
    // First call loads the baked model into RAM then generates on CPU — both
    // slow, so allow generously (still bounded, a resource-exhaustion guard).
    let v: serde_json::Value = client
        .post(format!("{AUDIO_URL}/generate"))
        .json(&json!({ "text": prompt }))
        .timeout(std::time::Duration::from_secs(600))
        .send()
        .await?
        .json()
        .await?;
    let wav = v["wav_b64"].as_str().unwrap_or_default().to_string();
    anyhow::ensure!(!wav.is_empty(), "worker returned no audio: {v}");
    Ok(json!({
        "kind": "audio",
        "audio_b64": wav,
        "model": "musicgen-small (CPU)",
        "sample_rate": v["sample_rate"],
        "seconds": v["seconds"],
        "cold_start": cold,
        "settled_via": "local-node",
    }))
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
            if sealed_mode() {
                ensure_sealed_network().await;
            }
            let publish = format!("127.0.0.1:{IMAGE_PORT}:7860");
            let image = worker_image_ref("CLOUDIY_IMAGE_WORKER", worker_image);
            check_pinned(&image)?;
            verify_image_signature(&image).await?;
            let sec = seccomp_arg();
            let user = worker_user();
            let mut args: Vec<&str> = vec!["run", "-d"];
            args.extend(worker_hardening());
            args.extend(worker_network());
            if let Some(s) = &sec {
                args.extend(["--security-opt", s.as_str()]);
            }
            if let Some(u) = &user {
                args.extend(["--user", u.as_str()]);
            }
            // Generous host-RAM cap (the model loads into RAM before the GPU);
            // bounds abuse without starving a normal run. Not --read-only: the
            // webui writes to several /app paths (tune per image on a GPU node).
            args.extend(["--memory", "24g"]);
            args.extend([
                "--gpus",
                "all",
                "--name",
                IMAGE_WORKER,
                "-p",
                publish.as_str(),
                image.as_str(),
            ]);
            let out = docker(&args).await?;
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
async fn run_video_worker(worker_image: &str, prompt: &str) -> anyhow::Result<serde_json::Value> {
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
            if sealed_mode() {
                ensure_sealed_network().await;
            }
            let publish = format!("127.0.0.1:{VIDEO_PORT}:7860");
            let mount = format!("{}:/out", dir.display());
            let image = worker_image_ref("CLOUDIY_VIDEO_WORKER", worker_image);
            check_pinned(&image)?;
            verify_image_signature(&image).await?;
            let sec = seccomp_arg();
            let user = worker_user();
            let mut args: Vec<&str> = vec!["run", "-d"];
            args.extend(worker_hardening());
            args.extend(worker_network());
            if let Some(s) = &sec {
                args.extend(["--security-opt", s.as_str()]);
            }
            if let Some(u) = &user {
                args.extend(["--user", u.as_str()]);
            }
            // Generous host-RAM cap; not --read-only (the worker writes the HF
            // weight cache and the /out clip). Tune per image on a GPU node.
            args.extend(["--memory", "24g"]);
            args.extend([
                "--gpus",
                "all",
                "--name",
                VIDEO_WORKER,
                "-p",
                publish.as_str(),
                "-v",
                mount.as_str(),
                image.as_str(),
            ]);
            let out = docker(&args).await?;
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
    use super::{catalog_entry, endpoint_price, header_host_is_loopback, image_is_pinned, repo_of};

    // Revenue density used by the auto-host controller: demand × price ÷ size.
    fn density(demand: f64, key: &str, size: f64) -> f64 {
        demand * endpoint_price(key) / size
    }

    #[test]
    fn price_weighting_prefers_higher_revenue_per_byte() {
        // Equal demand and size: the pricier endpoint ranks higher.
        let (d, size) = (3.0, 1.8e9);
        assert!(
            density(d, "stable-audio", size) > density(d, "whisper-ep", size),
            "stable-audio (0.06) should outrank whisper (0.006) at equal demand/size"
        );
        // A pricier, smaller model beats a cheaper, larger one on density.
        assert!(density(d, "stable-audio", 1.8e9) > density(d, "llama-ep", 2.77e9));
    }

    #[test]
    fn repo_of_strips_tag_but_keeps_registry() {
        assert_eq!(repo_of("ollama/ollama:latest"), "ollama/ollama");
        assert_eq!(
            repo_of("ghcr.io/cloudiy/worker-sdxl:latest"),
            "ghcr.io/cloudiy/worker-sdxl"
        );
        assert_eq!(repo_of("ollama/ollama"), "ollama/ollama");
    }

    #[test]
    fn catalog_covers_priced_endpoints() {
        // Every priced language/audio endpoint the controller may pick exists.
        for k in ["llama-ep", "whisper-ep", "stable-audio", "flux2"] {
            assert!(catalog_entry(k).is_some(), "{k} missing from catalog");
        }
    }

    #[test]
    fn only_digest_refs_count_as_pinned() {
        assert!(image_is_pinned("ghcr.io/cloudiy/worker-sdxl@sha256:abc123"));
        assert!(image_is_pinned("ollama/ollama@sha256:deadbeef"));
        assert!(!image_is_pinned("ollama/ollama"));
        assert!(!image_is_pinned("ghcr.io/cloudiy/worker-sdxl:latest"));
        assert!(!image_is_pinned("ghcr.io/cloudiy/worker-ltx:v0.1.0"));
    }

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
