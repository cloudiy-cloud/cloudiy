//! Consumer-side CLI commands — thin printing layer over [`cloudiy_sdk`].
//! Apps and AI agents should depend on the `cloudiy-sdk` crate directly.

use anyhow::Context;
use cloudiy_protocol::ProviderAnnouncement;
use cloudiy_scheduler::{Pipeline, Scheduler};
use cloudiy_sdk::{Client, Quote, SubmitError, SubmitOptions};

/// Fetch fresh announcements from a directory node and verify every
/// signature locally — the directory is an untrusted relay.
async fn fetch_providers(via: &str) -> anyhow::Result<Vec<ProviderAnnouncement>> {
    use cloudiy_common::proto::{self, Request, Response};

    let id: iroh::EndpointId = via
        .parse()
        .context("invalid directory Node ID")?;
    let endpoint = iroh::Endpoint::bind(iroh::endpoint::presets::N0).await?;
    let conn = endpoint.connect(id, proto::ALPN).await?;
    let (mut send, mut recv) = conn.open_bi().await?;
    proto::write_msg(&mut send, &Request::Providers).await?;
    let resp: Response = proto::read_msg(&mut recv).await?;
    conn.close(0u32.into(), b"done");
    endpoint.close().await;

    let announcements = match resp {
        Response::Providers(list) => list,
        Response::Error { message } => anyhow::bail!("directory error: {message}"),
        other => anyhow::bail!("unexpected directory response: {other:?}"),
    };

    let now = chrono::Utc::now().timestamp();
    let mut verified = Vec::new();
    for sa in announcements {
        match cloudiy_common::verify_announcement(&sa, now) {
            Ok(ann) => verified.push(ann),
            Err(e) => eprintln!("⚠️  dropping announcement from {}: {e}", sa.signed_by),
        }
    }
    Ok(verified)
}

/// Resolve the target node: explicit `--to`, or client-side scheduling over
/// the directory's verified announcements ("I need computation" — the
/// network answers with the best placement).
async fn resolve_target(
    to: Option<String>,
    via: Option<String>,
    spec: &cloudiy_sdk::WorkloadSpec,
) -> anyhow::Result<String> {
    match (to, via) {
        (Some(node), _) => Ok(node),
        (None, Some(via)) => {
            let nodes = fetch_providers(&via).await?;
            anyhow::ensure!(!nodes.is_empty(), "no live providers on this directory");
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
        (None, None) => anyhow::bail!("pass --to <node-id> or --via <directory-id>"),
    }
}

/// List live providers registered on a directory node.
pub async fn providers(via: String) -> anyhow::Result<()> {
    let nodes = fetch_providers(&via).await?;
    if nodes.is_empty() {
        println!("No live providers on this directory.");
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
        println!(
            "  available:    {:?}",
            ann.resources.available().0
        );
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
    via: Option<String>,
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
        capabilities: capabilities.iter().map(|c| Capability::new(c)).collect(),
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
    via: Option<String>,
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

pub async fn run_job(
    to: Option<String>,
    via: Option<String>,
    kernel: String,
    data: String,
    token: Option<String>,
    x402_demo: bool,
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
    if x402_demo {
        opts = opts.demo_payment();
    }

    match client.submit(opts).await {
        Ok(result) => {
            if result.signature_verified {
                println!("🔏 Signature verified — result signed by the node you dialed");
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
        Err(SubmitError::PaymentRequired(quote)) => print_payment_required(&quote),
        Err(e) => {
            client.close().await;
            return Err(e.into());
        }
    }
    client.close().await;
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
