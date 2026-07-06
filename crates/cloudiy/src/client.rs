//! Consumer-side CLI commands — thin printing layer over [`cloudiy_sdk`].
//! Apps and AI agents should depend on the `cloudiy-sdk` crate directly.

use cloudiy_sdk::{Client, Quote, SubmitError, SubmitOptions};

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

pub async fn run_job(
    to: String,
    kernel: String,
    data: String,
    token: Option<String>,
    x402_demo: bool,
) -> anyhow::Result<()> {
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
