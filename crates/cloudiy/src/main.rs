//! Cloudiy — decentralized GPU compute on Solana.
//!
//! One binary, two roles:
//! - `cloudiy share`  — provider: put this machine's GPU on the network
//! - `cloudiy run`    — consumer: execute a job on a remote GPU by Node ID

mod client;
mod core;
mod discover;
mod http;
mod p2p;

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::core::{AppState, SharedState, ESCROW_PROGRAM, PROTOCOL_FEE_BPS};

#[derive(Parser)]
#[command(
    name = "cloudiy",
    version,
    about = "Cloudiy — decentralized GPU compute on Solana.\nShare your GPU and earn USDC, or run jobs on someone else's."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Share this machine's GPU with the network (provider mode)
    #[command(alias = "serve")]
    Share {
        /// Address for the legacy HTTP API (used by the web marketplace).
        /// P2P (iroh) is always on and needs no port forwarding.
        #[arg(short, long, default_value = "0.0.0.0:8080")]
        bind: String,
        /// Disable the HTTP API entirely (P2P only)
        #[arg(long, default_value_t = false)]
        no_http: bool,
        /// Auth token consumers must present when submitting jobs.
        /// Omitted: a random per-session access code is generated and printed.
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// GPU model advertised to the network ("auto" = detected adapter)
        #[arg(short, long, default_value = "auto")]
        gpu_model: String,
        /// Advertised VRAM in megabytes
        #[arg(long, default_value_t = 24_576)]
        vram_mb: u64,
        /// Price per job in USDC (quoted via x402)
        #[arg(long, default_value_t = 0.01)]
        price_usdc: f64,
        /// USDC mint accepted for payment (default: devnet USDC)
        #[arg(long, default_value = "4zMMC9srt5Ri5X14GAgXhaHii3GnPAEERYPJgZJDncDU")]
        usdc_mint: String,
        /// x402 network label
        #[arg(long, default_value = "solana-devnet")]
        network: String,
    },
    /// Run a job on a remote GPU (consumer mode)
    #[command(alias = "submit")]
    Run {
        /// Provider Node ID (printed by `cloudiy share`)
        #[arg(short = 'T', long)]
        to: String,
        /// Kernel to execute (vector_add, matrix_mul)
        #[arg(short, long)]
        kernel: String,
        /// Input data for the kernel
        #[arg(short, long)]
        data: String,
        /// Access code / auth token printed by the provider at startup
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// Attach a demo x402 payment payload (flow demonstration only —
        /// real settlement uses the Cloudiy escrow on devnet)
        #[arg(long, default_value_t = false)]
        x402_demo: bool,
    },
    /// Launch a workload (container) on a remote node — Open Compute Protocol.
    /// Everything after `--` is the command to run inside the environment.
    Launch {
        /// Provider Node ID (printed by `cloudiy share`)
        #[arg(short = 'T', long)]
        to: String,
        /// OCI image (e.g. alpine:3.20, pytorch/pytorch:2.4)
        #[arg(short, long)]
        image: String,
        /// CPU cores requested
        #[arg(long, default_value_t = 1.0)]
        cpu: f64,
        /// Memory requested in MiB
        #[arg(long, default_value_t = 512)]
        memory_mb: u64,
        /// Required capabilities (repeatable): --cap cuda:12.8 --cap pytorch
        #[arg(long = "cap")]
        capabilities: Vec<String>,
        /// Hard wall-clock limit in seconds
        #[arg(long, default_value_t = 300)]
        timeout_secs: u64,
        /// Access code / auth token printed by the provider at startup
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// Attach a demo x402 payment payload
        #[arg(long, default_value_t = false)]
        x402_demo: bool,
        /// Command to run (after --)
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Check the status of a previously submitted job
    Status {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
        /// Job id returned by `run`
        #[arg(short, long)]
        job_id: String,
    },
    /// Show information about a provider node
    Info {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
    },
    /// Deploy a declared workload from a WorkloadSpec JSON file — the
    /// full-fidelity form of `launch` (supports `template` kernels too)
    Deploy {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
        /// Path to a WorkloadSpec JSON file (image or template + command +
        /// resources + capabilities — see PROTOCOL.md)
        #[arg(short, long)]
        spec: String,
        /// Access code / auth token printed by the provider at startup
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// Attach a demo x402 payment payload
        #[arg(long, default_value_t = false)]
        x402_demo: bool,
    },
    /// Print this machine's Node ID (stable P2P identity)
    Id,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Share {
            bind,
            no_http,
            token,
            gpu_model,
            vram_mb,
            price_usdc,
            usdc_mint,
            network,
        } => {
            share(
                bind, no_http, token, gpu_model, vram_mb, price_usdc, usdc_mint, network,
            )
            .await?
        }
        Commands::Run {
            to,
            kernel,
            data,
            token,
            x402_demo,
        } => client::run_job(to, kernel, data, token, x402_demo).await?,
        Commands::Launch {
            to,
            image,
            cpu,
            memory_mb,
            capabilities,
            timeout_secs,
            token,
            x402_demo,
            command,
        } => {
            client::launch_workload(
                to,
                image,
                cpu,
                memory_mb,
                capabilities,
                timeout_secs,
                token,
                x402_demo,
                command,
            )
            .await?
        }
        Commands::Status { to, job_id } => client::job_status(to, job_id).await?,
        Commands::Info { to } => client::node_info(to).await?,
        Commands::Deploy {
            to,
            spec,
            token,
            x402_demo,
        } => client::deploy(to, spec, token, x402_demo).await?,
        Commands::Id => {
            let secret = cloudiy_common::load_or_create_node_key()?;
            println!("{}", secret.public());
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn share(
    bind: String,
    no_http: bool,
    token: Option<String>,
    gpu_model: String,
    vram_mb: u64,
    price_usdc: f64,
    usdc_mint: String,
    network: String,
) -> anyhow::Result<()> {
    let pubkey = cloudiy_common::load_pubkey().unwrap_or_else(|e| {
        warn!("No Solana keypair found ({e}). Run `solana-keygen new` to earn USDC.");
        "<no-wallet-configured>".to_string()
    });

    let (token, generated) = match token {
        Some(t) => {
            if t == "cloudiy-dev-token" {
                warn!("'cloudiy-dev-token' is publicly known — use a strong secret.");
            }
            (t, false)
        }
        None => (cloudiy_common::generate_access_code(), true),
    };
    anyhow::ensure!(
        price_usdc > 0.0 && price_usdc < 1_000_000.0,
        "--price-usdc must be positive"
    );

    let gpu = Arc::new(cloudiy_runtime::GpuExecutor::new().await?);
    let gpu_model = if gpu_model == "auto" {
        gpu.info.name.clone()
    } else {
        gpu_model
    };
    info!(
        "GPU detected: {} ({:?} via {:?})",
        gpu.info.name, gpu.info.device_type, gpu.info.backend
    );

    // Stable P2P identity: the EndpointId doubles as the node's
    // address — consumers dial it directly, no IP/port needed.
    let secret_key = cloudiy_common::load_or_create_node_key()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key.clone())
        .alpns(vec![cloudiy_common::proto::ALPN.to_vec()])
        .bind()
        .await?;
    let endpoint_id = endpoint.id().to_string();

    // Open Compute Protocol announcement: resources + capabilities.
    let resources = discover::detect_resources(1, vram_mb);
    let capabilities = discover::detect_capabilities().await;
    info!(
        "Announcing resources: {:?}",
        resources.shared.0
    );
    info!(
        "Announcing capabilities: {}",
        capabilities
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );

    let state: SharedState = Arc::new(AppState {
        gpu: gpu.clone(),
        wgsl: cloudiy_runtime::WgslRuntime::new(gpu),
        jobs: Mutex::new(core::JobStore::default()),
        secret: secret_key,
        busy: Arc::new(tokio::sync::Semaphore::new(core::MAX_CONCURRENT_JOBS)),
        token: token.clone(),
        endpoint_id: endpoint_id.clone(),
        pubkey: pubkey.clone(),
        gpu_model,
        vram_mb,
        price_micro_usdc: (price_usdc * 1_000_000.0).round() as u64,
        usdc_mint,
        network,
        started_at: chrono::Utc::now(),
        resources: Mutex::new(resources),
        capabilities,
    });

    info!("🚀 Cloudiy node is online — sharing this GPU (P2P via iroh)");
    info!("   Node ID:       {}", endpoint_id);
    info!("   Solana pubkey: {}", pubkey);
    info!(
        "   Price: {} USDC/job via x402 · escrow {} ({}bps fee)",
        state.price_micro_usdc as f64 / 1_000_000.0,
        ESCROW_PROGRAM,
        PROTOCOL_FEE_BPS
    );
    if generated {
        info!("   Access code (this session): {}", token);
        info!(
            "   Run a job with: cloudiy run --to {} --token {} ...",
            endpoint_id, token
        );
    } else {
        info!("   Run a job with: cloudiy run --to {} ...", endpoint_id);
    }

    if !no_http {
        let addr: SocketAddr = bind.parse()?;
        let app = http::router(state.clone());
        let listener = TcpListener::bind(addr).await?;
        info!("   Legacy HTTP API (web marketplace): http://{}", addr);
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, app).await {
                warn!("HTTP server exited: {e}");
            }
        });
    }

    p2p::serve(endpoint, state).await
}
