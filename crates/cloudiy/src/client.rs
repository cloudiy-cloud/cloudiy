//! Consumer-side CLI commands — thin printing layer over [`cloudiy_sdk`].
//! Apps and AI agents should depend on the `cloudiy-sdk` crate directly.

use anyhow::Context;
use cloudiy_common::proto::{self, Request, Response, SessionFrame};
use cloudiy_common::{JobRequest, VmInfo};
use cloudiy_protocol::ProviderAnnouncement;
use cloudiy_scheduler::{Pipeline, Scheduler};
use cloudiy_sdk::{Client, Quote, SubmitError, SubmitOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bind a consumer endpoint under this machine's stable **client** identity
/// (distinct from the provider `node.key`), so a consumer's VMs, sessions and
/// tunnels are all owned by the same key across invocations.
async fn client_endpoint() -> anyhow::Result<iroh::Endpoint> {
    let secret = cloudiy_common::load_or_create_client_key()?;
    Ok(iroh::Endpoint::builder(iroh::endpoint::presets::N0)
        .secret_key(secret)
        .bind()
        .await?)
}

async fn connect(to: &str) -> anyhow::Result<(iroh::Endpoint, iroh::endpoint::Connection)> {
    let id: iroh::EndpointId = to.parse().context("invalid provider Node ID")?;
    let endpoint = client_endpoint().await?;
    let conn = endpoint.connect(id, proto::ALPN).await?;
    Ok((endpoint, conn))
}

/// Minimal request envelope carrying auth/payment for VM-plane operations.
fn vm_request(token: Option<String>, payment: Option<String>) -> JobRequest {
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

fn print_vm(info: &VmInfo) {
    println!("VM:      {}", info.vm_id);
    println!("State:   {}", info.state);
    if info.state == "missing" {
        return;
    }
    println!("Image:   {}", info.image);
    println!(
        "Reserved: {} vCPU · {} MiB RAM{}",
        info.cpu_millis as f64 / 1000.0,
        info.memory_mib,
        if info.gpu { " · GPU" } else { "" }
    );
    println!("Disk:    {} (persistent)", info.volume);
    if !info.ports.is_empty() {
        println!(
            "Ports:   {} (reach via `cloudiy tunnel`)",
            info.ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

/// Fetch fresh announcements from a directory node and verify every
/// signature locally — the directory is an untrusted relay.
async fn fetch_one_directory(via: &str) -> anyhow::Result<Vec<cloudiy_common::SignedAnnouncement>> {
    use cloudiy_common::proto::{self, Request, Response};

    let id: iroh::EndpointId = via.parse().context("invalid directory Node ID")?;
    let endpoint = client_endpoint().await?;
    let conn = endpoint.connect(id, proto::ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &Request::Providers).await?;
    let resp: Response = proto::read_msg(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    match resp {
        Response::Providers(list) => Ok(list),
        Response::Error { message } => anyhow::bail!("directory error: {message}"),
        other => anyhow::bail!("unexpected directory response: {other:?}"),
    }
}

/// Query one or more directories, verify every signature, and merge —
/// deduping by provider identity. Unreachable directories are skipped with a
/// warning, so redundant directories give resilience against a dead one.
async fn fetch_providers(vias: &[String]) -> anyhow::Result<Vec<ProviderAnnouncement>> {
    anyhow::ensure!(!vias.is_empty(), "no directory specified");
    let now = chrono::Utc::now().timestamp();
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    for via in vias {
        match fetch_one_directory(via).await {
            Ok(list) => {
                for sa in list {
                    match cloudiy_common::verify_announcement(&sa, now) {
                        Ok(ann) => {
                            if seen.insert(ann.identity.to_string()) {
                                merged.push(ann);
                            }
                        }
                        Err(e) => eprintln!("⚠️  dropping announcement from {}: {e}", sa.signed_by),
                    }
                }
            }
            Err(e) => eprintln!("⚠️  directory {via} unreachable: {e:#}"),
        }
    }
    Ok(merged)
}

/// Resolve the target node: explicit `--to`, or client-side scheduling over
/// the directories' verified announcements ("I need computation" — the
/// network answers with the best placement).
async fn resolve_target(
    to: Option<String>,
    via: Vec<String>,
    spec: &cloudiy_sdk::WorkloadSpec,
) -> anyhow::Result<String> {
    match (to, via.is_empty()) {
        (Some(node), _) => Ok(node),
        (None, false) => {
            let nodes = fetch_providers(&via).await?;
            anyhow::ensure!(!nodes.is_empty(), "no live providers on these directories");
            let placement = Pipeline::default_policy()
                .place(spec, &nodes)
                .context("no provider satisfies this workload's requirements")?;
            println!(
                "📡 Scheduled onto {} (score {:.2}, {} micro-USDC/h) — {} candidate(s)",
                placement.node,
                placement.score,
                placement.price_micro_usdc_per_hour,
                nodes.len()
            );
            Ok(placement.node.to_string())
        }
        (None, true) => anyhow::bail!("pass --to <node-id> or --via <directory-id>"),
    }
}

/// List live providers across one or more directory nodes.
pub async fn providers(via: Vec<String>) -> anyhow::Result<()> {
    let nodes = fetch_providers(&via).await?;
    if nodes.is_empty() {
        println!("No live providers on these directories.");
        return Ok(());
    }
    println!("{} live provider(s):\n", nodes.len());
    for ann in &nodes {
        println!("• {}", ann.identity);
        println!(
            "  capabilities: {}",
            ann.capabilities
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("  available:    {:?}", ann.resources.available().0);
        println!(
            "  price: {} micro-USDC/h · utilization {:.0}% · health {:?}",
            ann.price_micro_usdc_per_hour,
            ann.utilization * 100.0,
            ann.health
        );
        println!();
    }
    Ok(())
}

fn print_payment_required(quote: &Quote) {
    println!("💰 Payment Required (x402)");
    println!(
        "   Price:   {} micro-USDC ({} USDC)",
        quote.price_micro_usdc,
        quote.price_usdc()
    );
    println!("   Pay to:  {}", quote.pay_to);
    println!("   Asset:   {} (USDC)", quote.asset);
    println!("   Network: {}", quote.network);
    println!("   Escrow:  {}", quote.escrow_program);
    println!();
    println!("   Retry with --x402-demo to attach a demo payment payload,");
    println!("   or fund the escrow with `create_job` and resubmit.");
}

/// Open Compute Protocol: launch a container workload on a remote node.
#[allow(clippy::too_many_arguments)]
pub async fn launch_workload(
    to: Option<String>,
    via: Vec<String>,
    image: String,
    cpu: f64,
    memory_mb: u64,
    capabilities: Vec<String>,
    timeout_secs: u64,
    token: Option<String>,
    x402_demo: bool,
    command: Vec<String>,
) -> anyhow::Result<()> {
    use cloudiy_sdk::protocol::{Capability, ResourceKind, ResourceVector};

    let spec = cloudiy_sdk::WorkloadSpec {
        image: Some(image),
        command,
        resources: ResourceVector::new()
            .with(ResourceKind::Cpu, (cpu * 1000.0).round() as u64)
            .with(ResourceKind::Memory, memory_mb),
        capabilities: capabilities.iter().map(Capability::new).collect(),
        max_duration_secs: timeout_secs,
        ..Default::default()
    };

    let to = resolve_target(to, via, &spec).await?;
    let payment = x402_demo.then(cloudiy_sdk::demo_payment_payload);
    let client = Client::connect(&to).await?;

    match client.run_workload(spec, token, payment).await {
        Ok(result) => {
            if result.signature_verified {
                println!("🔏 Signature verified — result signed by the node you dialed");
            }
            println!("✅ Workload {} completed!", result.job_id);
            if let Some(receipt) = &result.payment_receipt {
                println!("Payment receipt (x402): {}", receipt);
            }
            println!("Logs:\n{}", String::from_utf8_lossy(&result.output));
        }
        Err(SubmitError::PaymentRequired(quote)) => print_payment_required(&quote),
        Err(e) => {
            client.close().await;
            return Err(e.into());
        }
    }
    client.close().await;
    Ok(())
}

/// Deploy a workload declared in a JSON spec file — the canonical
/// "declare WHAT, never HOW" form. Works for both runtime classes:
/// OCI (`image`) and WGSL kernels (`template` + `command[0]` input).
pub async fn deploy(
    to: Option<String>,
    via: Vec<String>,
    spec_path: String,
    token: Option<String>,
    x402_demo: bool,
) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(&spec_path)
        .map_err(|e| anyhow::anyhow!("cannot read spec file {spec_path}: {e}"))?;
    let spec: cloudiy_sdk::WorkloadSpec =
        serde_json::from_str(&raw).map_err(|e| anyhow::anyhow!("invalid WorkloadSpec: {e}"))?;
    anyhow::ensure!(
        spec.image.is_some() || spec.template.is_some(),
        "spec needs `image` (OCI) or `template` (kernel)"
    );

    let to = resolve_target(to, via, &spec).await?;
    let payment = x402_demo.then(cloudiy_sdk::demo_payment_payload);
    let client = Client::connect(&to).await?;

    match client.run_workload(spec, token, payment).await {
        Ok(result) => {
            if result.signature_verified {
                println!("🔏 Signature verified — result signed by the node you dialed");
            }
            println!("✅ Workload {} completed!", result.job_id);
            if let Some(receipt) = &result.payment_receipt {
                println!("Payment receipt (x402): {}", receipt);
            }
            println!("Logs:\n{}", String::from_utf8_lossy(&result.output));
        }
        Err(SubmitError::PaymentRequired(quote)) => print_payment_required(&quote),
        Err(e) => {
            client.close().await;
            return Err(e.into());
        }
    }
    client.close().await;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_job(
    to: Option<String>,
    via: Vec<String>,
    kernel: String,
    data: String,
    token: Option<String>,
    x402_demo: bool,
    escrow: Option<String>,
    job_id: Option<String>,
    auto_release: bool,
    keypair: Option<String>,
    rpc_url: String,
) -> anyhow::Result<()> {
    // For scheduling purposes a kernel job is a template workload requiring
    // the matching `kernel:*` capability.
    let sched_spec = cloudiy_sdk::WorkloadSpec {
        template: Some(kernel.clone()),
        capabilities: vec![cloudiy_sdk::protocol::Capability::new(format!(
            "kernel:{kernel}"
        ))],
        ..Default::default()
    };
    let to = resolve_target(to, via, &sched_spec).await?;
    let client = Client::connect(&to).await?;

    let mut opts = SubmitOptions::kernel(kernel, data.into_bytes());
    if let Some(t) = token {
        opts = opts.token(t);
    }
    if let Some(id) = job_id {
        // Pin the job id so it matches the escrow funded by `cloudiy pay`.
        opts = opts.job_id(id);
    }
    if let Some(acct) = &escrow {
        // Real payment: point the provider at the funded escrow account.
        opts = opts.payment(cloudiy_sdk::escrow_payment_payload(acct));
    } else if x402_demo {
        opts = opts.demo_payment();
    }

    let mut verified_ok = false;
    match client.submit(opts).await {
        Ok(result) => {
            if result.signature_verified {
                println!("🔏 Signature verified — result signed by the node you dialed");
                verified_ok = true;
            }
            println!("✅ Job {} completed!", result.job_id);
            if let Some(provider) = &result.provider_pubkey {
                println!("Provider: {}", provider);
            }
            if let Some(receipt) = &result.payment_receipt {
                println!("Payment receipt (x402): {}", receipt);
            }
            if let Ok(output) = String::from_utf8(result.output) {
                println!("Result:\n{}", output);
            }
        }
        Err(SubmitError::PaymentRequired(quote)) => {
            print_payment_required(&quote);
            client.close().await;
            return Ok(());
        }
        Err(e) => {
            client.close().await;
            return Err(e.into());
        }
    }
    client.close().await;

    // --release: pay the provider now that we hold a signature-verified
    // result. Refuse if the signature didn't verify — never pay for an
    // unattested result.
    if auto_release {
        let acct = escrow.context("--release needs --escrow (the funded escrow account)")?;
        anyhow::ensure!(
            verified_ok,
            "refusing to release: the result signature did not verify"
        );
        println!();
        release(acct, keypair, rpc_url, None).await?;
    }
    Ok(())
}

pub async fn job_status(to: String, job_id: String) -> anyhow::Result<()> {
    let client = Client::connect(&to).await?;
    let status = client.status(job_id).await?;
    println!("Job:      {}", status.job_id);
    println!("Status:   {}", status.status);
    println!("Progress: {:.0}%", status.progress);
    if let Some(provider) = status.provider_pubkey {
        println!("Provider: {}", provider);
    }
    client.close().await;
    Ok(())
}

pub async fn node_info(to: String) -> anyhow::Result<()> {
    let client = Client::connect(&to).await?;
    let info = client.info().await?;
    println!("{}", serde_json::to_string_pretty(&info)?);
    client.close().await;
    Ok(())
}

// ------------------------------------------------------- CloudiyOS VM plane

/// One-shot request/response over a fresh bi-stream under the client identity.
async fn rpc(to: &str, req: Request) -> anyhow::Result<Response> {
    let (endpoint, conn) = connect(to).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &req).await?;
    let resp = proto::read_msg::<Response>(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    Ok(resp)
}

#[allow(clippy::too_many_arguments)]
pub async fn vm_up(
    to: String,
    image: Option<String>,
    cpu: f64,
    memory_mb: u64,
    ports: Vec<u16>,
    token: Option<String>,
    x402_demo: bool,
) -> anyhow::Result<()> {
    use cloudiy_protocol::{ResourceKind, ResourceVector};
    let spec = cloudiy_sdk::WorkloadSpec {
        image,
        resources: ResourceVector::new()
            .with(ResourceKind::Cpu, (cpu * 1000.0).round() as u64)
            .with(ResourceKind::Memory, memory_mb),
        ports,
        ..Default::default()
    };
    let payment = x402_demo.then(cloudiy_sdk::demo_payment_payload);
    let req = Request::StartVm {
        request: vm_request(token, payment),
        spec,
    };
    match rpc(&to, req).await? {
        Response::Vm(info) => {
            println!("🖥️  VM ready");
            print_vm(&info);
            println!("\nOpen a shell:  cloudiy shell --to {to}");
        }
        Response::PaymentRequired { requirements } => {
            println!("💰 Payment Required (x402) to provision a VM:");
            println!("{}", serde_json::to_string_pretty(&requirements)?);
            println!("\nRetry with --x402-demo to attach a demo payment payload.");
        }
        Response::Error { message } => anyhow::bail!("provider error: {message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub async fn vm_status(to: String) -> anyhow::Result<()> {
    match rpc(
        &to,
        Request::VmStatus {
            request: vm_request(None, None),
        },
    )
    .await?
    {
        Response::Vm(info) => print_vm(&info),
        Response::Error { message } => anyhow::bail!("provider error: {message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

pub async fn vm_down(to: String, wipe: bool) -> anyhow::Result<()> {
    match rpc(
        &to,
        Request::StopVm {
            request: vm_request(None, None),
            wipe,
        },
    )
    .await?
    {
        Response::Ack => println!(
            "🗑️  VM destroyed{}",
            if wipe {
                " (disk wiped)"
            } else {
                " (disk kept)"
            }
        ),
        Response::Error { message } => anyhow::bail!("provider error: {message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
    Ok(())
}

/// Best-effort local terminal size via `stty size` → (cols, rows).
fn term_size() -> (u16, u16) {
    if let Ok(out) = std::process::Command::new("stty").arg("size").output() {
        let s = String::from_utf8_lossy(&out.stdout);
        let nums: Vec<u16> = s
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        if nums.len() == 2 {
            return (nums[1], nums[0]); // stty prints "rows cols"
        }
    }
    (80, 24)
}

/// Toggle the local terminal into raw mode (no local echo / line editing) so
/// only the remote PTY drives the screen — this is what makes vim/htop work.
fn set_raw(enabled: bool) {
    let args: &[&str] = if enabled {
        &["raw", "-echo"]
    } else {
        &["sane"]
    };
    let _ = std::process::Command::new("stty").args(args).status();
}

/// Interactive shell into the caller's VM over a real pseudo-terminal —
/// full-screen programs (vim, htop, less) work. Raw mode locally; the remote
/// shell handles echo and line editing.
pub async fn shell(to: String, token: Option<String>, command: Vec<String>) -> anyhow::Result<()> {
    let (endpoint, conn) = connect(&to).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    let (cols, rows) = term_size();
    proto::write_msg(
        &mut send,
        &Request::OpenSession {
            request: vm_request(token, None),
            command,
            cols,
            rows,
        },
    )
    .await?;

    match proto::read_msg::<Response>(&mut recv).await? {
        Response::SessionOpened { vm_id } => {
            eprintln!("🔗 connected to {vm_id} — Ctrl-D / `exit` to leave\r");
        }
        Response::Error { message } => {
            conn.close(0u32.into(), b"done");
            endpoint.close().await;
            anyhow::bail!("{message}");
        }
        other => anyhow::bail!("unexpected response: {other:?}"),
    }

    set_raw(true);

    // Local stdin (raw bytes, incl. Ctrl-D=0x04) → session Data frames.
    let stdin_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => {
                    let _ = proto::write_session_frame(&mut send, &SessionFrame::Eof).await;
                    break;
                }
                Ok(n) => {
                    if proto::write_session_frame(&mut send, &SessionFrame::Data(buf[..n].to_vec()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Session frames → local stdout, until the shell exits.
    let mut stdout = tokio::io::stdout();
    let mut exit_code = None;
    loop {
        match proto::read_session_frame(&mut recv).await {
            Ok(Some(SessionFrame::Data(d))) => {
                let _ = stdout.write_all(&d).await;
                let _ = stdout.flush().await;
            }
            Ok(Some(SessionFrame::Exit(code))) => {
                exit_code = code;
                break;
            }
            Ok(Some(SessionFrame::Error(msg))) => {
                eprintln!("\r\nsession error: {msg}\r");
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    stdin_task.abort();
    set_raw(false);
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    eprintln!(
        "[session ended{}]",
        match exit_code {
            Some(c) => format!(", exit {c}"),
            None => String::new(),
        }
    );
    Ok(())
}

/// Forward a local TCP port to a port published by the caller's VM. Each
/// local connection opens its own bi-stream; after `Ack` the stream is a raw
/// bidirectional byte copy.
pub async fn tunnel(
    to: String,
    port: u16,
    local_port: Option<u16>,
    token: Option<String>,
) -> anyhow::Result<()> {
    let (_endpoint, conn) = connect(&to).await?;
    let local = local_port.unwrap_or(port);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", local)).await?;
    println!("🔌 127.0.0.1:{local}  →  VM :{port}  (Ctrl-C to stop)");

    loop {
        let (sock, _) = listener.accept().await?;
        let conn = conn.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = tunnel_one(conn, sock, port, token).await {
                eprintln!("tunnel connection error: {e:#}");
            }
        });
    }
}

async fn tunnel_one(
    conn: iroh::endpoint::Connection,
    sock: tokio::net::TcpStream,
    port: u16,
    token: Option<String>,
) -> anyhow::Result<()> {
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(
        &mut send,
        &Request::Tunnel {
            request: vm_request(token, None),
            port,
        },
    )
    .await?;
    match proto::read_msg::<Response>(&mut recv).await? {
        Response::Ack => {}
        Response::Error { message } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected response: {other:?}"),
    }
    let (mut rd, mut wr) = sock.into_split();
    let up = tokio::io::copy(&mut rd, &mut send);
    let down = tokio::io::copy(&mut recv, &mut wr);
    let _ = tokio::try_join!(up, down);
    Ok(())
}

// ---------------------------------------------------- escrow funding (pay)

/// Fund an escrow `Job` on-chain for a provider, then print the account +
/// job id to pass to `cloudiy run --escrow ... --job-id ...`. This is the
/// consumer counterpart to the provider's on-chain verification: it locks
/// USDC before the job runs.
pub async fn pay(
    to: String,
    keypair: Option<String>,
    rpc_url: String,
    amount_usdc: Option<f64>,
    timeout_secs: i64,
) -> anyhow::Result<()> {
    // Ask the provider what to pay: its Solana payout pubkey, the USDC mint,
    // its price and the escrow program.
    let client = Client::connect(&to).await?;
    let info = client.info().await?;
    client.close().await;

    let provider_pk = info
        .solana_pubkey
        .context("provider has no Solana wallet configured — cannot be paid")?;
    let amount = amount_usdc.unwrap_or(info.price_usdc);
    anyhow::ensure!(amount > 0.0, "amount must be positive");
    let amount_micro = (amount * 1_000_000.0).round() as u64;

    let kp_path = keypair
        .map(std::path::PathBuf::from)
        .unwrap_or_else(cloudiy_common::default_keypair_path);
    let kp = crate::solana::Keypair::load(&kp_path)
        .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))?;

    let escrow_program = crate::solana::parse_pubkey(&info.escrow_program)?;
    let provider = crate::solana::parse_pubkey(&provider_pk)?;
    let mint = crate::solana::parse_pubkey(&info.usdc_mint)?;
    let job_id = *uuid::Uuid::new_v4().as_bytes();

    println!(
        "💸 Funding escrow: {} USDC → provider {} (mint {})",
        amount, provider_pk, info.usdc_mint
    );
    let funded = crate::solana::create_job(
        &rpc_url,
        &kp,
        &escrow_program,
        &provider,
        &mint,
        amount_micro,
        timeout_secs.max(60),
        job_id,
    )
    .await?;

    let job_uuid = uuid::Uuid::from_bytes(funded.job_id).to_string();
    println!("✅ Escrow funded and confirmed on-chain");
    println!(
        "   Job account: {}",
        crate::solana::pubkey_str(&funded.job_account)
    );
    println!("   Job id:      {job_uuid}");
    println!("   Tx:          {}", funded.signature);
    println!(
        "\nRun the paid job:\n  cloudiy run --to {} --escrow {} --job-id {} --kernel <k> --data <d>",
        to,
        crate::solana::pubkey_str(&funded.job_account),
        job_uuid
    );
    Ok(())
}

/// Release a funded escrow: pay the provider (minus the 4% protocol fee) and
/// mark the job done on-chain. Signed by the consumer that funded it. Run this
/// after `run` returned a signature-verified result you're satisfied with.
pub async fn release(
    escrow: String,
    keypair: Option<String>,
    rpc_url: String,
    escrow_program: Option<String>,
) -> anyhow::Result<()> {
    let kp_path = keypair
        .map(std::path::PathBuf::from)
        .unwrap_or_else(cloudiy_common::default_keypair_path);
    let kp = crate::solana::Keypair::load(&kp_path)
        .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))?;

    let program = escrow_program.unwrap_or_else(|| crate::core::ESCROW_PROGRAM.to_string());
    let program = crate::solana::parse_pubkey(&program)?;
    let job_account = crate::solana::parse_pubkey(&escrow)?;

    println!("💵 Releasing escrow {escrow} …");
    let r = crate::solana::release(&rpc_url, &kp, &program, &job_account).await?;

    println!("✅ Payment released on-chain");
    println!(
        "   Provider {} received {} micro-USDC ({} USDC)",
        crate::solana::pubkey_str(&r.provider),
        r.payout,
        r.payout as f64 / 1_000_000.0
    );
    println!(
        "   Protocol fee: {} micro-USDC ({}bps)",
        r.fee,
        crate::solana::PROTOCOL_FEE_BPS
    );
    println!("   Tx: {}", r.signature);
    Ok(())
}

/// Refund a funded escrow back to the consumer. As the consumer this works
/// after the escrow's deadline; as the provider it's a voluntary cancel any
/// time (the on-chain program enforces the rule).
pub async fn refund(
    escrow: String,
    keypair: Option<String>,
    rpc_url: String,
    escrow_program: Option<String>,
) -> anyhow::Result<()> {
    let kp_path = keypair
        .map(std::path::PathBuf::from)
        .unwrap_or_else(cloudiy_common::default_keypair_path);
    let kp = crate::solana::Keypair::load(&kp_path)
        .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))?;
    let program = escrow_program.unwrap_or_else(|| crate::core::ESCROW_PROGRAM.to_string());
    let program = crate::solana::parse_pubkey(&program)?;
    let job_account = crate::solana::parse_pubkey(&escrow)?;

    println!("↩️  Refunding escrow {escrow} …");
    let r = crate::solana::refund(&rpc_url, &kp, &program, &job_account).await?;
    println!("✅ Refund confirmed on-chain");
    println!(
        "   {} micro-USDC ({} USDC) returned to {}",
        r.amount,
        r.amount as f64 / 1_000_000.0,
        crate::solana::pubkey_str(&r.consumer)
    );
    println!("   Tx: {}", r.signature);
    Ok(())
}
