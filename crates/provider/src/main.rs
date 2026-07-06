use axum::{
    extract::{Json, Path, State},
    routing::{get, post},
    Router,
};
use clap::{Parser, Subcommand};
use gpuasas_common::{JobRequest as CommonJobRequest, JobResponse as CommonJobResponse, StatusResponse as CommonStatusResponse};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tracing::{info, warn};
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "gpuasas-provider", about = "GPU Provider Node - GPUaaS on Solana")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Inicia o servidor do provedor de GPU
    Serve {
        #[arg(short, long, default_value = "0.0.0.0:8080")]
        bind: String,
        #[arg(short, long, default_value = "seu-token-secreto-123")]
        token: String,
    },
}

type JobStore = Arc<Mutex<HashMap<String, CommonJobResponse>>>;

async fn submit_job(
    State(state): State<JobStore>,
    Json(req): Json<CommonJobRequest>,
) -> Json<CommonJobResponse> {
    if req.auth_token != "seu-token-secreto-123" {
        return Json(CommonJobResponse {
            job_id: req.job_id,
            output_data: vec![],
            status: "error".to_string(),
            error_message: Some("Token inválido".to_string()),
            provider_pubkey: None,
        });
    }

    info!("Recebido job {} - kernel: {}", req.job_id, req.kernel);

    let output = execute_on_gpu(&req.kernel, &req.input_data, &req.params);

    let response = CommonJobResponse {
        job_id: req.job_id.clone(),
        output_data: output.into_bytes(),
        status: "completed".to_string(),
        error_message: None,
        provider_pubkey: Some("GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP".to_string()),
    };

    {
        let mut store = state.lock().unwrap();
        store.insert(req.job_id, response.clone());
    }

    Json(response)
}

async fn get_status(
    State(state): State<JobStore>,
    Path(job_id): Path<String>,
) -> Json<CommonStatusResponse> {
    let store = state.lock().unwrap();
    if let Some(job) = store.get(&job_id) {
        Json(CommonStatusResponse {
            job_id,
            status: job.status.clone(),
            progress: if job.status == "completed" { 100.0 } else { 50.0 },
            provider_pubkey: job.provider_pubkey.clone(),
        })
    } else {
        Json(CommonStatusResponse {
            job_id,
            status: "not_found".to_string(),
            progress: 0.0,
            provider_pubkey: None,
        })
    }
}

fn execute_on_gpu(kernel: &str, input: &[u8], _params: &HashMap<String, String>) -> String {
    let input_str = String::from_utf8_lossy(input);
    match kernel {
        "vector_add" => format!("GPU_RESULT: vector_add executado com input={}", input_str),
        "matrix_mul" => format!("GPU_RESULT: matrix_mul executado (simulado) com input={}", input_str),
        _ => format!("GPU_RESULT: kernel '{}' executado na GPU remota", kernel),
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Serve { bind, token: _ } => {
            let addr: SocketAddr = bind.parse()?;
            let job_store: JobStore = Arc::new(Mutex::new(HashMap::new()));

            let app = Router::new()
                .route("/submit", post(submit_job))
                .route("/status/:job_id", get(get_status))
                .with_state(job_store);

            let pubkey = gpuasas_common::load_pubkey()
                .unwrap_or_else(|_| "GnaUN3hxTZaq6FqzVzLjXzJWi6svocFqgYbBJSdusFJP".to_string());

            info!("🚀 GPU Provider Node rodando em http://{}", addr);
            info!("   Solana Pubkey: {}", pubkey);
            info!("   Use o token: seu-token-secreto-123");

            let listener = TcpListener::bind(addr).await?;
            axum::serve(listener, app).await?;
        }
    }

    Ok(())
}