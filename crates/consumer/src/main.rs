use clap::{Parser, Subcommand};
use gpuasas_common::JobRequest;
use reqwest::Client;
use tracing::info;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "gpuasas-consumer", about = "GPU Consumer Client")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Envia um job para um provedor
    Submit {
        #[arg(short, long)]
        server: String,
        #[arg(short, long)]
        kernel: String,
        #[arg(short, long)]
        data: String,
        #[arg(short, long, default_value = "seu-token-secreto-123")]
        token: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Submit { server, kernel, data, token } => {
            let url = format!("http://{}/submit", server);
            let job_id = Uuid::new_v4().to_string();

            let request = JobRequest {
                job_id: job_id.clone(),
                kernel,
                input_data: data.into_bytes(),
                params: Default::default(),
                auth_token: token,
                consumer_pubkey: None,
            };

            info!("Conectando ao provedor {} ...", server);

            let client = Client::new();
            let res = client.post(&url).json(&request).send().await?;

            if res.status().is_success() {
                let response: gpuasas_common::JobResponse = res.json().await?;
                println!("✅ Job {} completado!", response.job_id);
                println!("Status: {}", response.status);
                if let Some(output) = String::from_utf8(response.output_data).ok() {
                    println!("Resultado:\n{}", output);
                }
            } else {
                println!("Erro: {}", res.status());
            }
        }
    }

    Ok(())
}