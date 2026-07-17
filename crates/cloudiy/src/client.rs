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
    match info.lease_remaining_secs {
        Some(secs) => println!(
            "Lease:   {} micro-USDC @ {}/h · {} left",
            info.lease_micro_usdc,
            info.price_micro_usdc_per_hour,
            fmt_duration(secs)
        ),
        None => println!("Lease:   unmetered (dev mode)"),
    }
}

/// Human-friendly `1h 23m 4s` from a second count (clamped at 0).
fn fmt_duration(secs: i64) -> String {
    let s = secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h {m}m {sec}s")
    } else if m > 0 {
        format!("{m}m {sec}s")
    } else {
        format!("{sec}s")
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

/// Probe a remote provider with the model's canary bank (RFC-0006 §5.1), under
/// this machine's client identity. Returns the pass/fail result.
pub(crate) async fn canary_probe_remote(
    to: &str,
    model: &str,
    token: Option<&str>,
) -> anyhow::Result<crate::canary::ProbeResult> {
    let endpoint = client_endpoint().await?;
    let r = crate::canary::probe_remote(&endpoint, to, model, token).await;
    endpoint.close().await;
    r
}

/// Fetch a directory's authoritative, canary-derived reputation map (RFC-0006
/// §6), verifying the directory's signature against the node id we dialed —
/// so a relay can't forge or swap scores. Best-effort: a directory that doesn't
/// serve it (or serves an invalid/stale signature) yields an empty map.
async fn fetch_one_reputation(via: &str) -> anyhow::Result<Vec<(String, f64)>> {
    let id: iroh::EndpointId = via.parse().context("invalid directory Node ID")?;
    let endpoint = client_endpoint().await?;
    let conn = endpoint.connect(id, proto::ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &Request::Reputation).await?;
    let resp: Response = proto::read_msg(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
    match resp {
        Response::Reputation(sr) => {
            let now = chrono::Utc::now().timestamp();
            match cloudiy_common::verify_reputation(&sr, via, now) {
                Ok(scores) => Ok(scores),
                Err(e) => {
                    eprintln!("⚠️  dropping reputation from {via}: {e}");
                    Ok(Vec::new())
                }
            }
        }
        _ => Ok(Vec::new()),
    }
}

/// Query one or more directories, verify every signature, and merge —
/// deduping by provider identity. Unreachable directories are skipped with a
/// warning, so redundant directories give resilience against a dead one.
///
/// Each provider's self-reported `reputation` is overridden with the directory's
/// **authoritative canary-derived** score (RFC-0006 §6) when the directory has
/// one — so scheduling ranks on *earned* trust, not a spoofable self-report.
/// The directory is trusted only for this ranking hint; payment safety still
/// comes from the escrow + result signatures, never from reputation.
pub(crate) async fn fetch_providers(
    vias: &[String],
    job_value_micro_usdc: u64,
) -> anyhow::Result<Vec<ProviderAnnouncement>> {
    anyhow::ensure!(!vias.is_empty(), "no directory specified");
    let now = chrono::Utc::now().timestamp();
    let mut seen = std::collections::HashSet::new();
    let mut merged = Vec::new();
    // Authoritative reputation across directories (keep the highest score seen).
    let mut authoritative: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();
    for via in vias {
        for (id, score) in fetch_one_reputation(via).await.unwrap_or_default() {
            authoritative
                .entry(id)
                .and_modify(|s| *s = s.max(score))
                .or_insert(score);
        }
        match fetch_one_directory(via).await {
            Ok(list) => {
                for sa in list {
                    match cloudiy_common::verify_announcement(&sa, now) {
                        Ok(mut ann) => {
                            // A provider WITH an authoritative (canary-derived)
                            // score below the routing floor has been caught
                            // cheating repeatedly — drop it (RFC-0006 §6). An
                            // unprobed provider has no authoritative score and
                            // passes (bootstrap); its self-report is only a hint.
                            if let Some(&score) = authoritative.get(ann.identity.as_str()) {
                                if score < crate::reputation::REPUTATION_ROUTING_FLOOR {
                                    eprintln!(
                                        "⚠️  skipping {} — reputation {:.2} below routing floor",
                                        ann.identity.as_str(),
                                        score
                                    );
                                    continue;
                                }
                                // Value-cap (RFC-0006 §6): a provider may only take
                                // jobs within its earned tier. Skip probed providers
                                // whose tier caps below this job's value at stake.
                                if !crate::reputation::may_take_value(score, job_value_micro_usdc) {
                                    eprintln!(
                                        "⚠️  skipping {} — tier for reputation {:.2} caps below {} µUSDC at stake",
                                        ann.identity.as_str(),
                                        score,
                                        job_value_micro_usdc
                                    );
                                    continue;
                                }
                                ann.reputation = score;
                            }
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
    if let Some(node) = to {
        return Ok(node);
    }
    // No explicit node: schedule over a directory. Resolve one from --via, the
    // CLOUDIY_DIRECTORY env, or the compiled default — so a zero-config user
    // still discovers providers.
    let dirs = cloudiy_common::resolve_directories(via);
    anyhow::ensure!(
        !dirs.is_empty(),
        "no directory configured — pass --to <node-id>, --via <directory-id>, or set CLOUDIY_DIRECTORY"
    );
    let nodes = fetch_providers(&dirs, spec.job_value_micro_usdc).await?;
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

/// Top-`n` distinct providers for a spec, ranked by the scheduling policy.
async fn resolve_targets(
    via: Vec<String>,
    spec: &cloudiy_sdk::WorkloadSpec,
    n: usize,
) -> anyhow::Result<Vec<String>> {
    let dirs = cloudiy_common::resolve_directories(via);
    anyhow::ensure!(
        !dirs.is_empty(),
        "--replicas needs a directory — pass --via <directory-id> or set CLOUDIY_DIRECTORY"
    );
    let nodes = fetch_providers(&dirs, spec.job_value_micro_usdc).await?;
    let ranked = Pipeline::default_policy().rank(spec, &nodes);
    Ok(ranked
        .into_iter()
        .take(n)
        .map(|p| p.node.to_string())
        .collect())
}

fn short_id(id: &str) -> &str {
    &id[..id.len().min(12)]
}

/// Redundant execution with quorum agreement (#4): run the same deterministic
/// kernel on `replicas` independent providers, verify each signature, and
/// accept the output only if a strict majority produced the *same* bytes.
/// Divergent providers are flagged — the hook for reputation/slashing.
async fn run_quorum(
    via: Vec<String>,
    spec: &cloudiy_sdk::WorkloadSpec,
    kernel: String,
    data: String,
    token: Option<String>,
    x402_demo: bool,
    replicas: usize,
) -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};

    let targets = resolve_targets(via, spec, replicas).await?;
    anyhow::ensure!(
        targets.len() >= replicas,
        "need {replicas} distinct providers but only {} are available",
        targets.len()
    );
    println!("🧮 Quorum run of `{kernel}` on {} providers", targets.len());

    let input = data.into_bytes();
    // (node, output, signature_verified)
    let mut results: Vec<(String, Vec<u8>, bool)> = Vec::new();
    for node in &targets {
        let mut opts = SubmitOptions::kernel(kernel.clone(), input.clone());
        if let Some(t) = &token {
            opts = opts.token(t.clone());
        }
        if x402_demo {
            opts = opts.demo_payment();
        }
        match Client::connect(node).await {
            Ok(client) => {
                let r = client.submit(opts).await;
                client.close().await;
                match r {
                    Ok(res) => {
                        println!(
                            "  • {} → {} bytes {}",
                            short_id(node),
                            res.output.len(),
                            if res.signature_verified {
                                "🔏 signed"
                            } else {
                                "⚠ unsigned (excluded from quorum)"
                            }
                        );
                        results.push((node.clone(), res.output, res.signature_verified));
                    }
                    Err(e) => println!("  • {} → failed: {e}", short_id(node)),
                }
            }
            Err(e) => println!("  • {} → unreachable: {e}", short_id(node)),
        }
    }
    anyhow::ensure!(!results.is_empty(), "no provider returned a result");

    // Tally output hashes — only signature-verified results vote.
    let mut tally: std::collections::HashMap<[u8; 32], Vec<String>> =
        std::collections::HashMap::new();
    for (node, out, sig_ok) in &results {
        if *sig_ok {
            let h: [u8; 32] = Sha256::digest(out).into();
            tally.entry(h).or_default().push(node.clone());
        }
    }
    let (winner_hash, agree) = tally
        .iter()
        .max_by_key(|(_, v)| v.len())
        .map(|(h, v)| (*h, v.clone()))
        .context("no signature-verified results to form a quorum")?;

    let voters = tally.values().map(Vec::len).sum::<usize>();
    let threshold = replicas / 2 + 1;
    println!();
    // Flag every provider whose output diverged from the winning consensus.
    for (h, nodes) in &tally {
        if *h != winner_hash {
            for n in nodes {
                println!(
                    "⚠  divergent result from {} — possible faulty/malicious provider",
                    short_id(n)
                );
            }
        }
    }
    anyhow::ensure!(
        agree.len() >= threshold,
        "no quorum: best agreement {}/{} verified < required {}",
        agree.len(),
        voters,
        threshold
    );

    let output = results
        .iter()
        .find(|(n, _, _)| n == &agree[0])
        .map(|(_, o, _)| o.clone())
        .unwrap_or_default();
    println!(
        "✅ Quorum reached: {}/{} signed providers agree",
        agree.len(),
        voters
    );
    if let Ok(s) = String::from_utf8(output) {
        println!("Result:\n{s}");
    }
    Ok(())
}

/// List live providers across one or more directory nodes.
pub async fn providers(via: Vec<String>) -> anyhow::Result<()> {
    let dirs = cloudiy_common::resolve_directories(via);
    anyhow::ensure!(
        !dirs.is_empty(),
        "no directory configured — pass --via <directory-id> or set CLOUDIY_DIRECTORY"
    );
    // Listing only — no job value at stake, so the value-cap is inert.
    let nodes = fetch_providers(&dirs, 0).await?;
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
    replicas: usize,
) -> anyhow::Result<()> {
    // For scheduling purposes a kernel job is a template workload requiring
    // the matching `kernel:*` capability.
    let mut sched_spec = cloudiy_sdk::WorkloadSpec {
        template: Some(kernel.clone()),
        capabilities: vec![cloudiy_sdk::protocol::Capability::new(format!(
            "kernel:{kernel}"
        ))],
        ..Default::default()
    };
    // Value at stake for the placement value-cap (RFC-0006 §6): the funded
    // escrow amount when this is a paid run. Free/dev-token runs leave it 0, so
    // the cap is inert and any provider (incl. unproven ones) may serve them.
    if let Some(acct) = &escrow {
        match crate::solana::escrow_amount(&rpc_url, acct).await {
            Ok(amount) => sched_spec.job_value_micro_usdc = amount,
            Err(e) => eprintln!("⚠️  could not read escrow value for placement: {e:#}"),
        }
    }

    // Quorum path: run the same deterministic kernel on N independent providers
    // and require agreement on the signed result (#4).
    if replicas > 1 {
        anyhow::ensure!(
            escrow.is_none(),
            "--replicas isn't supported with --escrow yet (per-replica payment is a follow-up); \
             use a dev token or --x402-demo"
        );
        return run_quorum(via, &sched_spec, kernel, data, token, x402_demo, replicas).await;
    }

    let to = resolve_target(to, via, &sched_spec).await?;
    let client = Client::connect(&to).await?;

    // Keep the input bytes to bind them into the run-auth signature (§4).
    let input_bytes = data.into_bytes();
    let mut opts = SubmitOptions::kernel(kernel, input_bytes.clone());
    if let Some(t) = token {
        opts = opts.token(t);
    }
    if let Some(id) = &job_id {
        // Pin the job id so it matches the escrow funded by `cloudiy pay`.
        opts = opts.job_id(id.clone());
    }
    if let Some(acct) = &escrow {
        // Real payment: point the provider at the funded escrow account, plus a
        // signature over the escrow-run message so only the escrow's own owner
        // can spend it (A4). Requires the pinned job id to bind the signature.
        let id = job_id
            .as_deref()
            .context("--escrow requires --job-id (the id funded by `cloudiy pay`)")?;
        let job_bytes = crate::core::uuid_bytes(id)
            .context("--job-id must be the UUID used to fund the escrow")?;
        let kp_path = keypair
            .clone()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(cloudiy_common::default_keypair_path);
        let kp = crate::solana::Keypair::load(&kp_path)
            .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))?;
        // Bound the authorization to a short window (MEDIUM-2).
        let expiry = chrono::Utc::now().timestamp() + 900;
        let sig = kp.sign_message(&crate::payments::run_auth_message(
            &job_bytes,
            &input_bytes,
            expiry,
        ));
        opts = opts.payment(cloudiy_sdk::escrow_payment_payload(
            acct,
            &hex::encode(sig),
            expiry,
        ));
    } else if x402_demo {
        opts = opts.demo_payment();
    }

    let mut verified_ok = false;
    let mut proof: Option<(Vec<u8>, String, String)> = None; // (output, sig_hex, node_key_hex)
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
            if let (Some(sig), Some(sb)) = (&result.signature, &result.signed_by) {
                proof = Some((result.output.clone(), sig.clone(), sb.clone()));
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

    // --release: trustlessly pay the provider now that we hold a
    // signature-verified result — the escrow's `release_verified` re-checks
    // the signature on-chain. Refuse if it didn't verify locally.
    if auto_release {
        let acct = escrow.context("--release needs --escrow (the funded escrow account)")?;
        anyhow::ensure!(
            verified_ok,
            "refusing to release: the result signature did not verify"
        );
        let (output, sig_hex, node_key_hex) =
            proof.context("provider returned no signature to release against")?;
        println!();
        release_verified_cmd(
            acct,
            keypair,
            rpc_url,
            None,
            input_bytes.clone(),
            output,
            sig_hex,
            node_key_hex,
        )
        .await?;
    }
    Ok(())
}

/// Trustless release used by `run --release`: pays only against an on-chain
/// re-check of the provider's result signature.
#[allow(clippy::too_many_arguments)]
async fn release_verified_cmd(
    escrow: String,
    keypair: Option<String>,
    rpc_url: String,
    escrow_program: Option<String>,
    input: Vec<u8>,
    output: Vec<u8>,
    signature_hex: String,
    node_key_hex: String,
) -> anyhow::Result<()> {
    let kp_path = keypair
        .map(std::path::PathBuf::from)
        .unwrap_or_else(cloudiy_common::default_keypair_path);
    let kp = crate::solana::Keypair::load(&kp_path)
        .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))?;
    let program = escrow_program.unwrap_or_else(|| crate::core::ESCROW_PROGRAM.to_string());
    let program = crate::solana::parse_pubkey(&program)?;
    let job_account = crate::solana::parse_pubkey(&escrow)?;
    let node_key = crate::solana::parse_hex32(&node_key_hex)?;
    let signature = crate::solana::parse_hex64(&signature_hex)?;

    println!("💵 Releasing escrow {escrow} (verifying result signature on-chain) …");
    let r = crate::solana::release_verified(
        &rpc_url,
        &kp,
        &program,
        &job_account,
        &node_key,
        &input,
        &output,
        &signature,
    )
    .await?;
    println!("✅ Payment released on-chain (signature verified by the contract)");
    println!(
        "   Provider {} received {} micro-USDC ({} USDC)",
        crate::solana::pubkey_str(&r.provider),
        r.payout,
        r.payout as f64 / 1_000_000.0
    );
    println!("   Protocol fee: {} micro-USDC", r.fee);
    println!("   Tx: {}", r.signature);
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

/// A funded escrow, ready to be spent by `run --escrow … --job-id …`.
pub(crate) struct FundedEscrowSummary {
    pub escrow_account: String,
    pub job_uuid: String,
    pub tx: String,
    pub amount_micro: u64,
    pub provider_pubkey: String,
    pub usdc_mint: String,
}

/// Fund an escrow `Job` on-chain for a provider: fetch its quote (payout
/// pubkey, mint, price, program), then lock USDC before the job runs. Silent
/// core shared by the CLI `pay` and the MCP server — no printing here.
pub(crate) async fn fund_escrow(
    to: &str,
    keypair: Option<String>,
    rpc_url: &str,
    amount_usdc: Option<f64>,
    timeout_secs: i64,
) -> anyhow::Result<FundedEscrowSummary> {
    // Ask the provider what to pay: its Solana payout pubkey, the USDC mint,
    // its price and the escrow program.
    let client = Client::connect(to).await?;
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
    // The provider's iroh node key (hex EndpointId) — stored in the escrow so
    // `release_verified` can check the result signature on-chain.
    let node_key = crate::solana::parse_hex32(&info.endpoint_id)?;
    let job_id = *uuid::Uuid::new_v4().as_bytes();

    let funded = crate::solana::create_job(
        rpc_url,
        &kp,
        &escrow_program,
        &provider,
        &mint,
        amount_micro,
        timeout_secs.max(60),
        job_id,
        &node_key,
    )
    .await?;

    Ok(FundedEscrowSummary {
        escrow_account: crate::solana::pubkey_str(&funded.job_account),
        job_uuid: uuid::Uuid::from_bytes(funded.job_id).to_string(),
        tx: funded.signature,
        amount_micro,
        provider_pubkey: provider_pk,
        usdc_mint: info.usdc_mint,
    })
}

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
    let s = fund_escrow(&to, keypair, &rpc_url, amount_usdc, timeout_secs).await?;
    println!(
        "💸 Funded escrow: {} USDC → provider {} (mint {})",
        s.amount_micro as f64 / 1_000_000.0,
        s.provider_pubkey,
        s.usdc_mint
    );
    println!("✅ Escrow funded and confirmed on-chain");
    println!("   Job account: {}", s.escrow_account);
    println!("   Job id:      {}", s.job_uuid);
    println!("   Tx:          {}", s.tx);
    println!(
        "\nRun the paid job:\n  cloudiy run --to {} --escrow {} --job-id {} --kernel <k> --data <d>",
        to, s.escrow_account, s.job_uuid
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
