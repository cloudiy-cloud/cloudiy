//! Cloudiy — decentralized GPU compute on Solana.
//!
//! One binary, two roles:
//! - `cloudiy share`  — provider: put this machine's GPU on the network
//! - `cloudiy run`    — consumer: execute a job on a remote GPU by Node ID

mod canary;
mod client;
mod core;
mod directory;
mod discover;
mod gateway;
mod http;
mod mcp;
mod p2p;
mod payments;
mod reputation;
mod session;
mod solana;
mod vm;

use clap::{Parser, Subcommand};
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};

use cloudiy_common::{ConfigOverrides, SolanaConfig, ENV_RPC_URL};

use crate::core::{AppState, SharedState, PROTOCOL_FEE_BPS};

/// Resolve the money-layer configuration for one command: CLI flags win, then
/// the environment (`CLOUDIY_CLUSTER` &co.), then the cluster's defaults. With
/// nothing supplied this is devnet, exactly as before.
fn solana_config(
    cluster: Option<String>,
    rpc_url: Option<String>,
    escrow_program: Option<String>,
    usdc_mint: Option<String>,
) -> anyhow::Result<SolanaConfig> {
    SolanaConfig::resolve(&ConfigOverrides {
        cluster,
        rpc_url,
        escrow_program,
        usdc_mint,
    })
}

/// `share` treats an absent RPC as "dev mode — no on-chain verification", so it
/// must NOT inherit the cluster's default endpoint: only an explicit flag or an
/// explicitly-set env var turns verification on.
fn explicit_rpc_url(flag: Option<String>) -> Option<String> {
    flag.filter(|v| !v.trim().is_empty()).or_else(|| {
        std::env::var(ENV_RPC_URL)
            .ok()
            .filter(|v| !v.trim().is_empty())
    })
}

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
        /// Hourly lease price in USDC for a dedicated VM on this node
        /// (the rate advertised to consumers renting a machine). Omit to
        /// reuse `--price-usdc`.
        #[arg(long)]
        price_usdc_per_hour: Option<f64>,
        /// Solana cluster: devnet (default) or mainnet. Sets the RPC, USDC mint,
        /// escrow program and x402 label together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
        /// USDC mint accepted for payment (default: the cluster's mint).
        /// Env: CLOUDIY_USDC_MINT.
        #[arg(long)]
        usdc_mint: Option<String>,
        /// Escrow program id (default: the cluster's program).
        /// Env: CLOUDIY_ESCROW_PROGRAM.
        #[arg(long)]
        escrow_program: Option<String>,
        /// x402 network label (default: derived from the cluster)
        #[arg(long)]
        network: Option<String>,
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
        /// `runsc` (gVisor) or `kata-runtime` (Kata). With neither this nor
        /// --allow-runc-untrusted, consumer images are refused (isolation
        /// audit 2026-07: plain runc shares the host kernel).
        #[arg(long)]
        runtime: Option<String>,
        /// Explicitly accept running consumer images under plain runc
        /// (shared host kernel). Prefer installing gVisor and using
        /// --runtime runsc.
        #[arg(long, default_value_t = false)]
        allow_runc_untrusted: bool,
        /// GPUs exposed to workloads: a device list like "0" or "0,1"
        /// (--gpus device=…). Omit = all GPUs (fine on single-GPU nodes;
        /// multi-GPU providers should restrict).
        #[arg(long)]
        gpu_device: Option<String>,
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
        #[arg(long)]
        rpc_url: Option<String>,
        /// Solana cluster: devnet (default) or mainnet. Sets RPC, USDC mint and
        /// escrow program together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
        /// Escrow program id (default: the cluster's program).
        /// Env: CLOUDIY_ESCROW_PROGRAM.
        #[arg(long)]
        escrow_program: Option<String>,
        /// Run on N independent providers and require a quorum agreement on the
        /// signed result (deterministic kernels only). Guards against a single
        /// provider returning signed-but-wrong output. Needs --via.
        #[arg(long, default_value_t = 1)]
        replicas: usize,
        /// Fund one escrow per replica automatically, after the scheduler picks
        /// the providers (RFC-0008). Real USDC. The provider set isn't known
        /// until placement, and each escrow pins one provider, so a replicated
        /// run cannot be pre-funded — use this instead of --escrow/--job-id.
        #[arg(long, default_value_t = false, conflicts_with_all = ["escrow", "job_id"])]
        pay: bool,
        /// USDC per replica for --pay (default: each provider's quoted price)
        #[arg(long)]
        amount: Option<f64>,
        /// Escrow timeout in seconds for --pay — must cover every replica's run
        /// plus settlement (refundable after this if unspent)
        #[arg(long, default_value_t = 3600)]
        timeout_secs: i64,
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
        #[arg(long)]
        rpc_url: Option<String>,
        /// Solana cluster: devnet (default) or mainnet. Sets RPC, USDC mint and
        /// escrow program together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
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
        #[arg(long)]
        rpc_url: Option<String>,
        /// Solana cluster: devnet (default) or mainnet. Sets RPC, USDC mint and
        /// escrow program together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
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
        #[arg(long)]
        rpc_url: Option<String>,
        /// Solana cluster: devnet (default) or mainnet. Sets RPC, USDC mint and
        /// escrow program together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
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
    /// Run canary checks (RFC-0006 §5.1): send known-answer prompts and report
    /// pass/fail. Without --to, self-checks the local worker; with --to, probes
    /// a remote provider (what a directory's prober does to earn/lose trust).
    Canary {
        /// Endpoint key to probe
        #[arg(short, long, default_value = "llama-ep")]
        model: String,
        /// Remote provider Node ID to probe (omit to self-check locally)
        #[arg(short = 'T', long)]
        to: Option<String>,
        /// Dev/admission token for the remote provider, if it gates runs
        #[arg(long)]
        token: Option<String>,
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
    /// Serve the network as MCP tools over stdio — any AI agent (Claude,
    /// MCP clients) can discover providers, pay escrow in USDC, run
    /// workloads and trustlessly release payment. No API keys.
    Mcp {
        /// Solana RPC endpoint (devnet by default; see --allow-mainnet)
        #[arg(long)]
        rpc_url: Option<String>,
        /// Solana cluster: devnet (default) or mainnet. Sets RPC, USDC mint and
        /// escrow program together. Env: CLOUDIY_CLUSTER.
        #[arg(long)]
        cluster: Option<String>,
        /// Solana keypair for payments (default: ~/.config/solana/id.json)
        #[arg(long, env = "CLOUDIY_KEYPAIR")]
        keypair: Option<String>,
        /// Max total USDC this MCP session may lock into escrows
        #[arg(long, default_value_t = 1.0)]
        max_spend_usdc: f64,
        /// Max USDC a single escrow may lock
        #[arg(long, default_value_t = 0.25)]
        max_per_job_usdc: f64,
        /// Directory node(s) for provider discovery (falls back to
        /// $CLOUDIY_DIRECTORY / the compiled default)
        #[arg(long)]
        directory: Vec<String>,
        /// Allow a non-devnet RPC endpoint (real money — off by default)
        #[arg(long, default_value_t = false)]
        allow_mainnet: bool,
        /// Expose only discovery/run tools — no transaction-signing tools
        #[arg(long, default_value_t = false)]
        read_only: bool,
    },
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
    let cli = Cli::parse();

    // Default to info-level output so `cloudiy share` shows its Node ID / access
    // code out of the box; RUST_LOG still overrides (e.g. RUST_LOG=debug).
    let env_filter = || {
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
    };
    // `mcp` speaks JSON-RPC on stdout — logs must go to stderr there so the
    // protocol stream stays clean. Every other command keeps stdout logs.
    if matches!(cli.command, Commands::Mcp { .. }) {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(env_filter())
            .init();
    }

    match cli.command {
        Commands::Share {
            bind,
            no_http,
            token,
            gpu_model,
            vram_mb,
            price_usdc,
            price_usdc_per_hour,
            cluster,
            usdc_mint,
            escrow_program,
            network,
            directory,
            share_cpu,
            share_memory_mb,
            no_gpu,
            rpc_url,
            require_payment,
            runtime,
            allow_runc_untrusted,
            gpu_device,
        } => {
            let cfg = solana_config(cluster, None, escrow_program, usdc_mint)?;
            share(ShareOpts {
                bind,
                no_http,
                token,
                gpu_model,
                vram_mb,
                price_usdc,
                price_usdc_per_hour,
                usdc_mint: cfg.usdc_mint,
                // An explicit --network still wins; otherwise it follows the cluster.
                network: network.unwrap_or(cfg.x402_network),
                escrow_program: cfg.escrow_program,
                directory,
                share_cpu,
                share_memory_mb,
                no_gpu,
                rpc_url: explicit_rpc_url(rpc_url),
                require_payment,
                runtime,
                allow_runc_untrusted,
                gpu_device,
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
            cluster,
            escrow_program,
            replicas,
            pay,
            amount,
            timeout_secs,
        } => {
            let cfg = solana_config(cluster, rpc_url, escrow_program, None)?;
            client::run_job(client::RunArgs {
                to,
                via,
                kernel,
                data,
                token,
                x402_demo,
                escrow,
                job_id,
                auto_release: release,
                keypair,
                rpc_url: cfg.rpc_url,
                escrow_program: cfg.escrow_program,
                replicas,
                pay,
                amount,
                timeout_secs,
            })
            .await?
        }
        Commands::Pay {
            to,
            keypair,
            rpc_url,
            cluster,
            amount,
            timeout_secs,
        } => {
            let cfg = solana_config(cluster, rpc_url, None, None)?;
            client::pay(to, keypair, cfg.rpc_url, amount, timeout_secs).await?
        }
        Commands::Release {
            escrow,
            keypair,
            rpc_url,
            cluster,
            escrow_program,
        } => {
            let cfg = solana_config(cluster, rpc_url, escrow_program, None)?;
            client::release(escrow, keypair, cfg.rpc_url, Some(cfg.escrow_program)).await?
        }
        Commands::Refund {
            escrow,
            keypair,
            rpc_url,
            cluster,
            escrow_program,
        } => {
            let cfg = solana_config(cluster, rpc_url, escrow_program, None)?;
            client::refund(escrow, keypair, cfg.rpc_url, Some(cfg.escrow_program)).await?
        }
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
        Commands::Canary { model, to, token } => {
            let r = match &to {
                Some(node) => {
                    println!("Probing remote provider {node} for `{model}`…\n");
                    client::canary_probe_remote(node, &model, token.as_deref()).await?
                }
                None => {
                    println!("Running canary checks for `{model}` on the local worker…\n");
                    canary::probe_local(&model).await
                }
            };
            for (prompt, ok, answer) in &r.items {
                let mark = if *ok { "PASS" } else { "FAIL" };
                println!("  [{mark}] {prompt}\n         → {answer}");
            }
            if r.total() == 0 {
                println!("No canaries for `{model}` (not served / no bank entries).");
            } else {
                println!(
                    "\nScore: {}/{} passed ({:.0}%)",
                    r.passed(),
                    r.total(),
                    r.score() * 100.0
                );
                // Fold this probe into a fresh reputation to show the ramp it
                // implies (RFC-0006 §6). One probe won't make you trusted —
                // trust is earned over a sustained clean record.
                let mut reg = reputation::Registry::default();
                let rep = reg.record_probe("self", &r);
                let pol = rep.policy();
                println!(
                    "Reputation (from this probe alone): score {:.2} · tier `{}`",
                    rep.score,
                    rep.tier().label()
                );
                println!(
                    "  ramp → jobs up to ${:.2} · {:.0}% audited · {}h holdback",
                    pol.max_job_micro_usdc as f64 / 1e6,
                    pol.canary_rate * 100.0,
                    pol.holdback_secs / 3600
                );
                println!("  (a single probe stays `new` — trust is earned over a sustained clean record)");
            }
        }
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
                .secret_key(secret.clone())
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
            directory::serve(endpoint, secret).await?;
        }
        Commands::Id => {
            let secret = cloudiy_common::load_or_create_node_key()?;
            println!("{}", secret.public());
        }
        Commands::Mcp {
            rpc_url,
            cluster,
            keypair,
            max_spend_usdc,
            max_per_job_usdc,
            directory,
            allow_mainnet,
            read_only,
        } => {
            let cfg = solana_config(cluster, rpc_url, None, None)?;
            mcp::serve(mcp::McpOpts {
                rpc_url: cfg.rpc_url,
                cluster: cfg.cluster,
                escrow_program: cfg.escrow_program,
                keypair,
                max_spend_usdc,
                max_per_job_usdc,
                directory,
                allow_mainnet,
                read_only,
            })
            .await?
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
    price_usdc_per_hour: Option<f64>,
    usdc_mint: String,
    network: String,
    escrow_program: String,
    directory: Vec<String>,
    share_cpu: Option<f64>,
    share_memory_mb: Option<u64>,
    no_gpu: bool,
    rpc_url: Option<String>,
    require_payment: bool,
    runtime: Option<String>,
    allow_runc_untrusted: bool,
    gpu_device: Option<String>,
}

async fn share(opts: ShareOpts) -> anyhow::Result<()> {
    let ShareOpts {
        bind,
        no_http,
        token,
        gpu_model,
        vram_mb,
        price_usdc,
        price_usdc_per_hour,
        usdc_mint,
        network,
        escrow_program,
        directory,
        share_cpu,
        share_memory_mb,
        no_gpu,
        rpc_url,
        require_payment,
        runtime,
        allow_runc_untrusted,
        gpu_device,
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
        jobs: Mutex::new(core::JobStore::with_persistence(
            cloudiy_common::config_dir().join("jobs.jsonl"),
        )),
        secret: secret_key,
        busy: Arc::new(tokio::sync::Semaphore::new(core::MAX_CONCURRENT_JOBS)),
        sessions: Arc::new(tokio::sync::Semaphore::new(core::MAX_CONCURRENT_SESSIONS)),
        inbound: Arc::new(tokio::sync::Semaphore::new(
            core::MAX_CONCURRENT_INBOUND_STREAMS,
        )),
        token: token.clone(),
        endpoint_id: endpoint_id.clone(),
        pubkey: pubkey.clone(),
        gpu_model,
        vram_mb,
        price_micro_usdc: (price_usdc * 1_000_000.0).round() as u64,
        price_micro_usdc_per_hour: (price_usdc_per_hour.unwrap_or(price_usdc) * 1_000_000.0).round()
            as u64,
        usdc_mint,
        network,
        started_at: chrono::Utc::now(),
        resources: Mutex::new(resources),
        capabilities,
        vm: vm::VmManager::new(runtime.clone()),
        rpc_url: rpc_url.clone(),
        require_payment,
        escrow_program,
        container_runtime: runtime.clone(),
        allow_runc_untrusted,
        gpu_device,
        served_escrows: Mutex::new(core::ServedEscrows::default()),
    });

    // Adopt any VMs left running by a previous provider process (rebuild the
    // in-memory map + re-reserve their resources), so restarts don't orphan
    // containers or collide on `vm up`.
    let adopted = state.vm.reconcile(&state.resources).await;
    if adopted > 0 {
        info!("Reconciled {adopted} VM(s) from a previous run");
    }

    // Lease reaper: stop VMs whose prepaid compute budget is spent, so a tenant
    // can't hold hardware past what they paid for (#2). Unmetered/dev VMs are
    // never reaped.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                ticker.tick().await;
                let stopped = state.vm.reap_expired(&state.resources).await;
                for owner in stopped {
                    info!("VM lease expired — stopped VM for {owner}");
                }
            }
        });
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
        state.escrow_program,
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
        // The legacy HTTP API is optional — P2P is the real interface. A busy
        // port (8080 is commonly taken) must NOT take the whole provider down,
        // so a bind failure degrades to P2P-only instead of aborting the node.
        match TcpListener::bind(addr).await {
            Ok(listener) => {
                info!("   Legacy HTTP API (web marketplace): http://{}", addr);
                tokio::spawn(async move {
                    if let Err(e) = axum::serve(listener, app).await {
                        warn!("HTTP server exited: {e}");
                    }
                });
            }
            Err(e) => {
                warn!("   HTTP API off — could not bind {addr}: {e}");
                warn!("   Provider stays online over P2P. Change it with --bind <addr:port>, or hide this with --no-http.");
            }
        }
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

    // Phase 3: a headless provider (no gateway) still auto-hosts when the
    // operator set policy.mode = "auto". The cycle updates the hosted set;
    // the 60s announce heartbeat re-publishes it. No-op in manual mode.
    {
        let ep = endpoint.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                let _ = gateway::autohost_cycle(&ep).await;
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
    // Model endpoints this node serves (pulled on demand) and which are warm,
    // so consumers can route to a ready provider. Compute before the struct
    // literal so no MutexGuard is held across the `.await`.
    let served_models = gateway::servable_models().await;
    let warm_models = gateway::warm_models();
    let resources = state.resources.lock().clone();
    let announcement = cloudiy_protocol::ProviderAnnouncement {
        identity: cloudiy_protocol::Identity::new(state.endpoint_id.clone()),
        resources,
        capabilities: state.capabilities.clone(),
        region: None,
        price_micro_usdc_per_hour: state.price_micro_usdc_per_hour,
        reputation: 0.0,
        utilization,
        health: cloudiy_protocol::Health::Healthy,
        served_models,
        warm_models,
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
