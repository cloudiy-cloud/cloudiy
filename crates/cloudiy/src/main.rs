//! Cloudiy — decentralized GPU compute on Solana.
//!
//! One binary, two roles:
//! - `cloudiy share`  — provider: put this machine's GPU on the network
//! - `cloudiy run`    — consumer: execute a job on a remote GPU by Node ID

mod client;
mod core;
mod directory;
mod discover;
mod gateway;
mod http;
mod p2p;
mod payments;
mod session;
mod solana;
mod vm;

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
        /// Directory node to announce this provider on (repeat heartbeats
        /// keep the entry fresh; omit to stay unlisted)
        #[arg(long)]
        directory: Vec<String>,
        /// CPU cores to share with the network (default: all detected;
        /// the rest stays private)
        #[arg(long)]
        share_cpu: Option<f64>,
        /// RAM to share in MB (default: all detected)
        #[arg(long)]
        share_memory_mb: Option<u64>,
        /// Don't share the GPU even if one is present (CPU/RAM provider)
        #[arg(long, default_value_t = false)]
        no_gpu: bool,
        /// Solana RPC endpoint for verifying escrow payments on-chain
        /// (e.g. https://api.devnet.solana.com). Omit to run in dev mode.
        #[arg(long)]
        rpc_url: Option<String>,
        /// Require a verified on-chain escrow for every job — reject the dev
        /// token and demo payments. Needs --rpc-url.
        #[arg(long, default_value_t = false)]
        require_payment: bool,
        /// OCI runtime for stronger isolation of untrusted workloads/VMs:
        /// `runsc` (gVisor) or `kata-runtime` (Kata). Default: runc.
        #[arg(long)]
        runtime: Option<String>,
    },
    /// Run a job on a remote GPU (consumer mode)
    #[command(alias = "submit")]
    Run {
        /// Provider Node ID (printed by `cloudiy share`)
        #[arg(short = 'T', long, conflicts_with = "via")]
        to: Option<String>,
        /// Directory Node ID — discover providers and let the scheduler
        /// pick the placement instead of naming a node
        #[arg(long)]
        via: Vec<String>,
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
        /// Escrow Job account (base58) you funded on-chain — the provider
        /// verifies it before executing (real payment)
        #[arg(long)]
        escrow: Option<String>,
        /// Pin the job id (the UUID printed by `cloudiy pay`) so it matches
        /// the funded escrow
        #[arg(long)]
        job_id: Option<String>,
        /// After a signature-verified result, release the escrow to pay the
        /// provider (needs --escrow). One shot: run → verify → pay.
        #[arg(long, default_value_t = false)]
        release: bool,
        /// Solana keypair for --release (default: ~/.config/solana/id.json)
        #[arg(long)]
        keypair: Option<String>,
        /// Solana RPC endpoint for --release
        #[arg(long, default_value = "https://api.devnet.solana.com")]
        rpc_url: String,
    },
    /// Fund an escrow on-chain for a provider (real USDC payment). Prints the
    /// escrow account + job id to pass to `run --escrow ... --job-id ...`.
    Pay {
        /// Provider Node ID to pay
        #[arg(short = 'T', long)]
        to: String,
        /// Solana keypair to pay from (default: ~/.config/solana/id.json)
        #[arg(long)]
        keypair: Option<String>,
        /// Solana RPC endpoint
        #[arg(long, default_value = "https://api.devnet.solana.com")]
        rpc_url: String,
        /// Amount in USDC (default: the provider's quoted price)
        #[arg(long)]
        amount: Option<f64>,
        /// Escrow timeout in seconds (refundable after this if unspent)
        #[arg(long, default_value_t = 3600)]
        timeout_secs: i64,
    },
    /// Release a funded escrow on-chain — pay the provider (minus the 4% fee)
    /// after you've received a signature-verified result.
    Release {
        /// Escrow Job account (base58), from `cloudiy pay`
        #[arg(long)]
        escrow: String,
        /// Solana keypair that funded the escrow (default: ~/.config/solana/id.json)
        #[arg(long)]
        keypair: Option<String>,
        /// Solana RPC endpoint
        #[arg(long, default_value = "https://api.devnet.solana.com")]
        rpc_url: String,
        /// Escrow program id (default: the built-in devnet program)
        #[arg(long)]
        escrow_program: Option<String>,
    },
    /// Refund a funded escrow back to the consumer — after its deadline (as the
    /// consumer) or as a voluntary provider cancel.
    Refund {
        /// Escrow Job account (base58)
        #[arg(long)]
        escrow: String,
        /// Solana keypair (consumer after deadline, or provider to cancel)
        #[arg(long)]
        keypair: Option<String>,
        /// Solana RPC endpoint
        #[arg(long, default_value = "https://api.devnet.solana.com")]
        rpc_url: String,
        /// Escrow program id (default: the built-in devnet program)
        #[arg(long)]
        escrow_program: Option<String>,
    },
    /// Launch a workload (container) on a remote node — Open Compute Protocol.
    /// Everything after `--` is the command to run inside the environment.
    Launch {
        /// Provider Node ID (printed by `cloudiy share`)
        #[arg(short = 'T', long, conflicts_with = "via")]
        to: Option<String>,
        /// Directory Node ID — schedule instead of naming a node
        #[arg(long)]
        via: Vec<String>,
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
        #[arg(short = 'T', long, conflicts_with = "via")]
        to: Option<String>,
        /// Directory Node ID — schedule instead of naming a node
        #[arg(long)]
        via: Vec<String>,
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
    /// List live providers across one or more directory nodes
    Providers {
        /// Directory Node ID (repeatable — results are merged). Falls back to
        /// $CLOUDIY_DIRECTORY / the compiled default when omitted.
        #[arg(long)]
        via: Vec<String>,
    },
    /// Manage your persistent VM on a provider (CloudiyOS)
    Vm {
        #[command(subcommand)]
        action: VmAction,
    },
    /// Open an interactive shell into your VM on a provider
    Shell {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
        /// Access code / auth token
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// Command to run instead of the default shell (after --)
        #[arg(trailing_var_arg = true)]
        command: Vec<String>,
    },
    /// Forward a local port to a port published by your VM on a provider
    Tunnel {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
        /// Remote port (published by your VM)
        #[arg(short, long)]
        port: u16,
        /// Local port to listen on (default: same as remote)
        #[arg(short, long)]
        local_port: Option<u16>,
        /// Access code / auth token
        #[arg(long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
    },
    /// Run the CloudiyOS gateway — a local HTTP/WebSocket bridge to the P2P
    /// network with a built-in browser terminal (browser → gateway → VM)
    Os {
        /// Address to serve the gateway on
        #[arg(short, long, default_value = "127.0.0.1:4600")]
        bind: String,
        /// Serve the real CloudiyOS UI from this directory (e.g. `web`);
        /// omit to serve only the built-in terminal
        #[arg(long)]
        web_dir: Option<String>,
    },
    /// Run a directory node — the bootstrap discovery registry providers
    /// announce to and consumers discover through
    Directory,
    /// Print this machine's Node ID (stable P2P identity)
    Id,
}

#[derive(Subcommand)]
enum VmAction {
    /// Provision (or show) your VM on a provider
    Up {
        /// Provider Node ID
        #[arg(short = 'T', long)]
        to: String,
        /// VM image (default: debian:12-slim)
        #[arg(short, long)]
        image: Option<String>,
        /// CPU cores to reserve
        #[arg(long, default_value_t = 1.0)]
        cpu: f64,
        /// Memory to reserve in MB
        #[arg(long, default_value_t = 1024)]
        memory_mb: u64,
        /// Ports the VM publishes (repeatable), reachable via `cloudiy tunnel`
        #[arg(long = "port")]
        ports: Vec<u16>,
        /// Access code / auth token
        #[arg(short, long, env = "CLOUDIY_TOKEN")]
        token: Option<String>,
        /// Attach a demo x402 payment payload
        #[arg(long, default_value_t = false)]
        x402_demo: bool,
    },
    /// Show your VM's status on a provider
    Status {
        #[arg(short = 'T', long)]
        to: String,
    },
    /// Destroy your VM on a provider (add --wipe to delete its disk)
    Down {
        #[arg(short = 'T', long)]
        to: String,
        /// Also delete the persistent volume
        #[arg(long, default_value_t = false)]
        wipe: bool,
    },
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
            directory,
            share_cpu,
            share_memory_mb,
            no_gpu,
            rpc_url,
            require_payment,
            runtime,
        } => {
            share(ShareOpts {
                bind,
                no_http,
                token,
                gpu_model,
                vram_mb,
                price_usdc,
                usdc_mint,
                network,
                directory,
                share_cpu,
                share_memory_mb,
                no_gpu,
                rpc_url,
                require_payment,
                runtime,
            })
            .await?
        }
        Commands::Run {
            to,
            via,
            kernel,
            data,
            token,
            x402_demo,
            escrow,
            job_id,
            release,
            keypair,
            rpc_url,
        } => {
            client::run_job(
                to, via, kernel, data, token, x402_demo, escrow, job_id, release, keypair, rpc_url,
            )
            .await?
        }
        Commands::Pay {
            to,
            keypair,
            rpc_url,
            amount,
            timeout_secs,
        } => client::pay(to, keypair, rpc_url, amount, timeout_secs).await?,
        Commands::Release {
            escrow,
            keypair,
            rpc_url,
            escrow_program,
        } => client::release(escrow, keypair, rpc_url, escrow_program).await?,
        Commands::Refund {
            escrow,
            keypair,
            rpc_url,
            escrow_program,
        } => client::refund(escrow, keypair, rpc_url, escrow_program).await?,
        Commands::Launch {
            to,
            via,
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
                via,
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
            via,
            spec,
            token,
            x402_demo,
        } => client::deploy(to, via, spec, token, x402_demo).await?,
        Commands::Providers { via } => client::providers(via).await?,
        Commands::Vm { action } => match action {
            VmAction::Up {
                to,
                image,
                cpu,
                memory_mb,
                ports,
                token,
                x402_demo,
            } => client::vm_up(to, image, cpu, memory_mb, ports, token, x402_demo).await?,
            VmAction::Status { to } => client::vm_status(to).await?,
            VmAction::Down { to, wipe } => client::vm_down(to, wipe).await?,
        },
        Commands::Shell { to, token, command } => client::shell(to, token, command).await?,
        Commands::Tunnel {
            to,
            port,
            local_port,
            token,
        } => client::tunnel(to, port, local_port, token).await?,
        Commands::Os { bind, web_dir } => {
            let addr: SocketAddr = bind.parse()?;
            gateway::serve(addr, web_dir.map(std::path::PathBuf::from)).await?;
        }
        Commands::Directory => {
            let secret = cloudiy_common::load_or_create_directory_key()?;
            let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
                .secret_key(secret)
                .alpns(vec![cloudiy_common::proto::ALPN.to_vec()])
                .bind()
                .await?;
            let dir_id = endpoint.id();
            info!("📒 Cloudiy directory node is online");
            info!("   Directory ID: {dir_id}");
            info!("   Providers announce with:  cloudiy share --directory {dir_id}");
            info!("   Consumers discover with:  cloudiy providers --via {dir_id}");
            info!("   Or schedule directly:     cloudiy run --via {dir_id} ...");
            info!("   Zero-config for a fleet:  export CLOUDIY_DIRECTORY={dir_id}");
            info!("   Bake in as the default:   CLOUDIY_DEFAULT_DIRECTORY={dir_id} cargo build --release");
            directory::serve(endpoint).await?;
        }
        Commands::Id => {
            let secret = cloudiy_common::load_or_create_node_key()?;
            println!("{}", secret.public());
        }
    }

    Ok(())
}

struct ShareOpts {
    bind: String,
    no_http: bool,
    token: Option<String>,
    gpu_model: String,
    vram_mb: u64,
    price_usdc: f64,
    usdc_mint: String,
    network: String,
    directory: Vec<String>,
    share_cpu: Option<f64>,
    share_memory_mb: Option<u64>,
    no_gpu: bool,
    rpc_url: Option<String>,
    require_payment: bool,
    runtime: Option<String>,
}

async fn share(opts: ShareOpts) -> anyhow::Result<()> {
    let ShareOpts {
        bind,
        no_http,
        token,
        gpu_model,
        vram_mb,
        price_usdc,
        usdc_mint,
        network,
        directory,
        share_cpu,
        share_memory_mb,
        no_gpu,
        rpc_url,
        require_payment,
        runtime,
    } = opts;

    anyhow::ensure!(
        !require_payment || rpc_url.is_some(),
        "--require-payment needs --rpc-url to verify escrows on-chain"
    );

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

    // GPU is optional — a machine without one (or with --no-gpu) joins the
    // network as a CPU/RAM provider serving container workloads.
    let gpu = if no_gpu {
        info!("GPU sharing disabled (--no-gpu) — providing CPU/RAM only");
        None
    } else {
        match cloudiy_runtime::GpuExecutor::new().await {
            Ok(g) => {
                info!(
                    "GPU detected: {} ({:?} via {:?})",
                    g.info.name, g.info.device_type, g.info.backend
                );
                Some(Arc::new(g))
            }
            Err(e) => {
                warn!("No usable GPU ({e:#}) — providing CPU/RAM only");
                None
            }
        }
    };
    let gpu_model = match (&gpu, gpu_model.as_str()) {
        (Some(g), "auto") => g.info.name.clone(),
        (None, "auto") => "cpu-only".to_string(),
        _ => gpu_model,
    };

    // Stable P2P identity: the EndpointId doubles as the node's
    // address — consumers dial it directly, no IP/port needed.
    let secret_key = cloudiy_common::load_or_create_node_key()?;
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret_key.clone())
        .alpns(vec![cloudiy_common::proto::ALPN.to_vec()])
        .bind()
        .await?;
    let endpoint_id = endpoint.id().to_string();

    // Open Compute Protocol announcement: resources + capabilities. The
    // provider chooses the shared slice; the rest of the machine is private.
    let share_cpu_millis = share_cpu.map(|c| (c * 1000.0).round().max(0.0) as u64);
    let resources = discover::detect_resources(
        u64::from(gpu.is_some()),
        if gpu.is_some() { vram_mb } else { 0 },
        share_cpu_millis,
        share_memory_mb,
    );
    // Isolation runtime: warn if the requested one isn't available to Docker.
    if let Some(rt) = &runtime {
        let available = discover::detect_docker_runtimes().await;
        if available.iter().any(|r| r == rt) {
            info!(
                "Isolation: containers run under `{rt}` ({})",
                discover::isolation_level(Some(rt))
            );
        } else {
            warn!(
                "Requested runtime `{rt}` not available to Docker (has: {}) — falling back to runc",
                available.join(", ")
            );
        }
    }
    let capabilities = discover::detect_capabilities(gpu.is_some(), runtime.as_deref()).await;
    info!("Announcing resources: {:?}", resources.shared.0);
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
        wgsl: gpu.map(cloudiy_runtime::WgslRuntime::new),
        jobs: Mutex::new(core::JobStore::default()),
        secret: secret_key,
        busy: Arc::new(tokio::sync::Semaphore::new(core::MAX_CONCURRENT_JOBS)),
        sessions: Arc::new(tokio::sync::Semaphore::new(core::MAX_CONCURRENT_SESSIONS)),
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
        vm: vm::VmManager::new(runtime.clone()),
        rpc_url: rpc_url.clone(),
        require_payment,
        container_runtime: runtime.clone(),
        served_escrows: Mutex::new(std::collections::HashSet::new()),
    });

    // Adopt any VMs left running by a previous provider process (rebuild the
    // in-memory map + re-reserve their resources), so restarts don't orphan
    // containers or collide on `vm up`.
    let adopted = state.vm.reconcile(&state.resources).await;
    if adopted > 0 {
        info!("Reconciled {adopted} VM(s) from a previous run");
    }

    if state.gpu.is_some() {
        info!("🚀 Cloudiy node is online — sharing GPU + CPU/RAM (P2P via iroh)");
    } else {
        info!("🚀 Cloudiy node is online — sharing CPU/RAM (P2P via iroh)");
    }
    info!("   Node ID:       {}", endpoint_id);
    info!("   Solana pubkey: {}", pubkey);
    info!(
        "   Price: {} USDC/job via x402 · escrow {} ({}bps fee)",
        state.price_micro_usdc as f64 / 1_000_000.0,
        ESCROW_PROGRAM,
        PROTOCOL_FEE_BPS
    );
    match (&rpc_url, require_payment) {
        (Some(url), true) => info!("   Payment: ENFORCED — on-chain escrow required (RPC {url})"),
        (Some(url), false) => {
            info!("   Payment: on-chain escrow verified when attached (RPC {url})")
        }
        (None, _) => warn!("   Payment: dev mode — no on-chain verification (set --rpc-url)"),
    }
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

    // Discovery: announce this provider on every configured directory (for
    // redundancy), then keep the entries fresh with heartbeats well inside
    // the announcement TTL. Fall back to CLOUDIY_DIRECTORY / the compiled
    // default so a provider joins the public network with no flags.
    let directory = cloudiy_common::resolve_directories(directory);
    if directory.is_empty() {
        info!("   No directory configured — reachable by node id only (set CLOUDIY_DIRECTORY to auto-announce)");
    }
    for dir in &directory {
        let dir_id: iroh::EndpointId = dir
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid --directory node id: {dir}"))?;
        let ep = endpoint.clone();
        let st = state.clone();
        let dir = dir.clone();
        info!("   Announcing on directory {dir} (heartbeat 60s)");
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(60));
            loop {
                ticker.tick().await;
                match announce_once(&ep, dir_id, &st).await {
                    Ok(()) => tracing::debug!("announce ok"),
                    Err(e) => warn!("announce to {dir} failed: {e:#}"),
                }
            }
        });
    }

    p2p::serve(endpoint, state).await
}

/// Build, sign and push one announcement to the directory.
async fn announce_once(
    endpoint: &iroh::Endpoint,
    dir: iroh::EndpointId,
    state: &SharedState,
) -> anyhow::Result<()> {
    use cloudiy_common::proto::{self, Request, Response};

    let utilization =
        1.0 - state.busy.available_permits() as f64 / core::MAX_CONCURRENT_JOBS as f64;
    let announcement = cloudiy_protocol::ProviderAnnouncement {
        identity: cloudiy_protocol::Identity::new(state.endpoint_id.clone()),
        resources: state.resources.lock().unwrap().clone(),
        capabilities: state.capabilities.clone(),
        region: None,
        price_micro_usdc_per_hour: state.price_micro_usdc,
        reputation: 0.0,
        utilization,
        health: cloudiy_protocol::Health::Healthy,
    };
    let signed = cloudiy_common::sign_announcement(
        &state.secret,
        &announcement,
        chrono::Utc::now().timestamp(),
    )?;

    let conn = endpoint.connect(dir, proto::ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &Request::Announce(signed)).await?;
    let resp: Response = proto::read_msg(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    match resp {
        Response::Ack => Ok(()),
        Response::Error { message } => anyhow::bail!("directory rejected announce: {message}"),
        other => anyhow::bail!("unexpected directory response: {other:?}"),
    }
}
