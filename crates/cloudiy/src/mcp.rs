//! `cloudiy mcp` — serve the network as MCP tools over stdio.
//!
//! Any MCP client (Claude Code, agents, IDEs) gets the full consumer loop as
//! tools: discover providers, quote, fund escrow in USDC, run workloads and
//! trustlessly release payment against the on-chain signature check — no API
//! keys, no accounts. The transport is newline-delimited JSON-RPC 2.0 on
//! stdin/stdout (hand-rolled like `solana.rs`, no MCP framework dependency);
//! logs must therefore go to stderr (see `main.rs`).
//!
//! Money safety: devnet RPC unless `--allow-mainnet`; a per-job and a
//! cumulative per-session USDC cap gate `cloudiy_pay_escrow`; `--read-only`
//! removes every tool that signs a transaction.

use anyhow::Context;
use base64::Engine;
use cloudiy_scheduler::{Pipeline, Scheduler};
use cloudiy_sdk::{Client, SubmitError, SubmitOptions};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct McpOpts {
    pub rpc_url: String,
    pub keypair: Option<String>,
    pub max_spend_usdc: f64,
    pub max_per_job_usdc: f64,
    pub directory: Vec<String>,
    pub allow_mainnet: bool,
    pub read_only: bool,
}

struct Session {
    opts: McpOpts,
    /// Micro-USDC locked into escrows so far this session (the spend cap).
    spent_micro_usdc: u64,
}

/// Protocol revisions this server can speak; unknown requests get the latest.
const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Whether an RPC URL is a recognized non-mainnet endpoint. A plain
/// `contains("devnet")` is unsafe — a mainnet host can carry `devnet` in a
/// path or query. We match the *host* against known non-mainnet names and,
/// crucially, reject anything that names `mainnet`; an unrecognized custom RPC
/// is treated as potentially-mainnet and requires the explicit opt-in.
fn rpc_is_non_mainnet(url: &str) -> bool {
    // Strip scheme, then take the authority up to the first `/`, `?` or `#`.
    let after_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    if host.contains("mainnet") {
        return false;
    }
    host.contains("devnet")
        || host.contains("testnet")
        || host == "localhost"
        || host.starts_with("localhost:")
        || host.starts_with("127.0.0.1")
        || host.starts_with("0.0.0.0")
        || host.starts_with("[::1]")
}

pub async fn serve(opts: McpOpts) -> anyhow::Result<()> {
    anyhow::ensure!(
        opts.allow_mainnet || rpc_is_non_mainnet(&opts.rpc_url),
        "refusing to run against RPC {} without --allow-mainnet — its host is not a recognized \
         devnet/testnet/localnet endpoint, so real USDC could move",
        opts.rpc_url
    );
    let mut session = Session {
        opts,
        spent_micro_usdc: 0,
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut stdout = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => dispatch(&mut session, msg).await,
            Err(_) => Some(json!({
                "jsonrpc": "2.0", "id": Value::Null,
                "error": {"code": -32700, "message": "parse error"},
            })),
        };
        if let Some(r) = reply {
            stdout
                .write_all(serde_json::to_string(&r)?.as_bytes())
                .await?;
            stdout.write_all(b"\n").await?;
            stdout.flush().await?;
        }
    }
    Ok(())
}

/// Handle one JSON-RPC message. Returns `None` for notifications (no reply).
async fn dispatch(sess: &mut Session, msg: Value) -> Option<Value> {
    let id = msg.get("id").filter(|v| !v.is_null()).cloned();
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    if method.starts_with("notifications/") {
        return None;
    }
    let result: Result<Value, (i64, String)> = match method {
        "initialize" => {
            let requested = msg["params"]["protocolVersion"].as_str().unwrap_or("");
            let version = if SUPPORTED_VERSIONS.contains(&requested) {
                requested
            } else {
                SUPPORTED_VERSIONS[SUPPORTED_VERSIONS.len() - 1]
            };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "cloudiy", "version": env!("CARGO_PKG_VERSION")},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tool_definitions(sess.opts.read_only)})),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("").to_string();
            let args = msg["params"]["arguments"].clone();
            match call_tool(sess, &name, &args).await {
                Ok(v) => Ok(json!({
                    "content": [{"type": "text",
                                 "text": serde_json::to_string_pretty(&v).unwrap_or_default()}],
                    "isError": false,
                })),
                Err(e) => Ok(json!({
                    "content": [{"type": "text", "text": e}],
                    "isError": true,
                })),
            }
        }
        other => Err((-32601, format!("method not found: {other}"))),
    };
    // Requests without an id are notifications by spec — never answer them.
    let id = id?;
    Some(match result {
        Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
        Err((code, message)) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": {"code": code, "message": message},
        }),
    })
}

/// A tool entry in `tools/list` form.
fn tool(name: &str, description: &str, props: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": props,
            "required": required,
        },
    })
}

fn tool_definitions(read_only: bool) -> Vec<Value> {
    let mut tools = vec![
        tool(
            "cloudiy_list_providers",
            "List live compute providers on the cloudiy network (signed announcements, \
             verified locally): node id, resources, capabilities, price in micro-USDC/h, health.",
            json!({
                "directories": {"type": "array", "items": {"type": "string"},
                    "description": "Directory node ids to query (default: server config / CLOUDIY_DIRECTORY)"},
            }),
            &[],
        ),
        tool(
            "cloudiy_quote",
            "Get a provider's x402 quote: price in USDC per job, USDC mint, escrow program, \
             Solana payout pubkey and capabilities. This is everything needed to pay it.",
            json!({
                "node_id": {"type": "string", "description": "Provider Node ID (from cloudiy_list_providers)"},
            }),
            &["node_id"],
        ),
        tool(
            "cloudiy_run_job",
            "Run a compute kernel (vector_add, matrix_mul, wgsl) on a provider and get the \
             ed25519-signed result. Free dev mode: pass the provider's access token. Paid mode: \
             pass escrow_account + job_id from cloudiy_pay_escrow. The result reports \
             signature_verified plus the signature/signed_by proof for cloudiy_release_verified.",
            json!({
                "node_id": {"type": "string", "description": "Provider Node ID"},
                "kernel": {"type": "string", "description": "Kernel name, e.g. vector_add"},
                "data": {"type": "string", "description": "Kernel input, e.g. \"1,2,3;4,5,6\""},
                "token": {"type": "string", "description": "Provider access code (dev mode)"},
                "escrow_account": {"type": "string", "description": "Funded escrow Job account (paid mode)"},
                "job_id": {"type": "string", "description": "Job UUID the escrow was funded with"},
            }),
            &["node_id", "kernel", "data"],
        ),
        tool(
            "cloudiy_launch",
            "Launch an OCI container workload on the network (declare WHAT, not HOW). \
             Without node_id the client-side scheduler places it over the directory's \
             verified announcements. May take minutes on a cold image pull.",
            json!({
                "image": {"type": "string", "description": "OCI image, e.g. alpine:3.20"},
                "command": {"type": "array", "items": {"type": "string"},
                    "description": "Command to run in the container"},
                "node_id": {"type": "string", "description": "Provider Node ID (omit to schedule)"},
                "directories": {"type": "array", "items": {"type": "string"},
                    "description": "Directories to schedule over when node_id is omitted"},
                "cpu": {"type": "number", "description": "CPU cores requested (default 1)"},
                "memory_mb": {"type": "integer", "description": "Memory in MiB (default 512)"},
                "capabilities": {"type": "array", "items": {"type": "string"},
                    "description": "Required capabilities, e.g. cuda:12.8"},
                "timeout_secs": {"type": "integer", "description": "Wall-clock limit (default 300)"},
                "token": {"type": "string", "description": "Provider access code (dev mode)"},
            }),
            &["image", "command"],
        ),
        tool(
            "cloudiy_deploy",
            "Deploy a serverless worker on the network — a template recipe \
             (pytorch, ollama, comfyui, vllm…) or an OCI image, with environment \
             variables. The agent's own deploy: no dashboard, no API key. Pay keyless \
             by passing escrow_account + job_id from cloudiy_pay_escrow, or a dev token. \
             Without node_id the scheduler places it over the directory.",
            json!({
                "template": {"type": "string", "description": "Recipe name the network expands (pytorch, ollama, comfyui, vllm, whisper…)"},
                "image": {"type": "string", "description": "OCI image instead of a template, e.g. ghcr.io/org/worker:latest"},
                "env": {"type": "object", "description": "Environment variables, e.g. {\"HF_TOKEN\":\"hf_…\"}"},
                "command": {"type": "array", "items": {"type": "string"}, "description": "Override the image command (optional)"},
                "capabilities": {"type": "array", "items": {"type": "string"}, "description": "Required capabilities, e.g. cuda:12.8"},
                "cpu": {"type": "number", "description": "CPU cores requested (default 1)"},
                "memory_mb": {"type": "integer", "description": "Memory in MiB (default 1024)"},
                "ports": {"type": "array", "items": {"type": "integer"}, "description": "Ports the worker exposes"},
                "timeout_secs": {"type": "integer", "description": "Wall-clock limit (default 600)"},
                "node_id": {"type": "string", "description": "Provider Node ID (omit to schedule)"},
                "directories": {"type": "array", "items": {"type": "string"}, "description": "Directories to schedule over when node_id is omitted"},
                "token": {"type": "string", "description": "Provider access code (dev mode)"},
                "escrow_account": {"type": "string", "description": "Funded escrow Job account (paid, keyless)"},
                "job_id": {"type": "string", "description": "Job UUID the escrow was funded with"},
            }),
            &[],
        ),
    ];
    if !read_only {
        tools.push(tool(
            "cloudiy_pay_escrow",
            "Lock USDC on-chain in the cloudiy escrow for a provider (Solana). Returns the \
             escrow_account + job_id to pass to cloudiy_run_job. Subject to the server's \
             per-job and per-session spend caps.",
            json!({
                "node_id": {"type": "string", "description": "Provider Node ID to pay"},
                "amount_usdc": {"type": "number", "description": "USDC to lock (default: provider's quoted price)"},
                "timeout_secs": {"type": "integer", "description": "Escrow deadline in seconds (default 3600; refundable after)"},
            }),
            &["node_id"],
        ));
        tools.push(tool(
            "cloudiy_release_verified",
            "Trustlessly release a funded escrow: the contract re-verifies the provider's \
             ed25519 result signature on-chain (Ed25519 precompile) before paying out. Pass \
             the exact output/signature/signed_by from cloudiy_run_job.",
            json!({
                "escrow_account": {"type": "string", "description": "Escrow Job account (base58)"},
                "output": {"type": "string", "description": "Exact job output (utf8) as returned by cloudiy_run_job"},
                "output_b64": {"type": "string", "description": "Exact job output, base64 (use when output was binary)"},
                "signature_hex": {"type": "string", "description": "Provider's result signature (hex, from cloudiy_run_job)"},
                "node_key_hex": {"type": "string", "description": "Provider node key that signed (signed_by from cloudiy_run_job)"},
            }),
            &["escrow_account", "signature_hex", "node_key_hex"],
        ));
        tools.push(tool(
            "cloudiy_refund",
            "Refund a funded escrow back to the consumer — after its deadline (as consumer) \
             or any time as the provider cancelling.",
            json!({
                "escrow_account": {"type": "string", "description": "Escrow Job account (base58)"},
            }),
            &["escrow_account"],
        ));
    }
    tools
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("missing required argument: {key}"))
}

fn err_str(e: impl std::fmt::Display) -> String {
    format!("{e:#}")
}

/// Reject an escrow that would blow the per-job or per-session USDC caps.
fn check_caps(sess: &Session, amount_usdc: f64) -> Result<(), String> {
    if amount_usdc <= 0.0 {
        return Err("amount must be positive".into());
    }
    if amount_usdc > sess.opts.max_per_job_usdc {
        return Err(format!(
            "amount {amount_usdc} USDC exceeds the per-job cap of {} USDC (server --max-per-job-usdc)",
            sess.opts.max_per_job_usdc
        ));
    }
    let spent = sess.spent_micro_usdc as f64 / 1_000_000.0;
    if spent + amount_usdc > sess.opts.max_spend_usdc {
        return Err(format!(
            "spend cap reached: {spent:.6} USDC already locked this session + {amount_usdc} \
             would exceed the {} USDC session cap (server --max-spend-usdc)",
            sess.opts.max_spend_usdc
        ));
    }
    Ok(())
}

/// Directories for a call: explicit argument > server flag > env/default.
fn directories_for(sess: &Session, args: &Value) -> Vec<String> {
    let explicit: Vec<String> = args["directories"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let seed = if explicit.is_empty() {
        sess.opts.directory.clone()
    } else {
        explicit
    };
    cloudiy_common::resolve_directories(seed)
}

fn load_keypair(sess: &Session) -> Result<crate::solana::Keypair, String> {
    let kp_path = sess
        .opts
        .keypair
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(cloudiy_common::default_keypair_path);
    crate::solana::Keypair::load(&kp_path)
        .with_context(|| format!("loading Solana keypair from {}", kp_path.display()))
        .map_err(err_str)
}

async fn call_tool(sess: &mut Session, name: &str, args: &Value) -> Result<Value, String> {
    if sess.opts.read_only
        && matches!(
            name,
            "cloudiy_pay_escrow" | "cloudiy_release_verified" | "cloudiy_refund"
        )
    {
        return Err("this server runs with --read-only: payment tools are disabled".into());
    }
    match name {
        "cloudiy_list_providers" => {
            let dirs = directories_for(sess, args);
            if dirs.is_empty() {
                return Err(
                    "no directory configured — pass `directories`, start with --directory, \
                     or set CLOUDIY_DIRECTORY"
                        .into(),
                );
            }
            let nodes = crate::client::fetch_providers(&dirs)
                .await
                .map_err(err_str)?;
            Ok(json!({
                "count": nodes.len(),
                "providers": serde_json::to_value(&nodes).map_err(err_str)?,
            }))
        }
        "cloudiy_quote" => {
            let node_id = req_str(args, "node_id")?;
            let client = Client::connect(node_id).await.map_err(err_str)?;
            let info = client.info().await.map_err(err_str);
            client.close().await;
            Ok(serde_json::to_value(info?).map_err(err_str)?)
        }
        "cloudiy_run_job" => {
            let node_id = req_str(args, "node_id")?;
            let kernel = req_str(args, "kernel")?;
            let data = req_str(args, "data")?;
            let mut opts = SubmitOptions::kernel(kernel, data.as_bytes().to_vec());
            if let Some(t) = args["token"].as_str().filter(|s| !s.is_empty()) {
                opts = opts.token(t);
            }
            let job_id = args["job_id"].as_str().filter(|s| !s.is_empty());
            if let Some(id) = job_id {
                opts = opts.job_id(id);
            }
            if let Some(acct) = args["escrow_account"].as_str().filter(|s| !s.is_empty()) {
                // Real payment: same escrow-run binding as `cloudiy run` (A4) —
                // the consumer signs the run-auth message over the pinned job
                // id, so only the escrow's owner can spend it.
                let id =
                    job_id.ok_or("escrow_account requires job_id (from cloudiy_pay_escrow)")?;
                let job_bytes = crate::core::uuid_bytes(id)
                    .ok_or("job_id must be the UUID the escrow was funded with")?;
                let kp = load_keypair(sess)?;
                let sig =
                    kp.sign_message(&crate::payments::run_auth_message(&job_bytes, data.as_bytes()));
                opts = opts.payment(cloudiy_sdk::escrow_payment_payload(acct, &hex::encode(sig)));
            }
            let client = Client::connect(node_id).await.map_err(err_str)?;
            let outcome = client.submit(opts).await;
            client.close().await;
            match outcome {
                Ok(r) => Ok(job_result_json(&r)),
                Err(SubmitError::PaymentRequired(q)) => Err(format!(
                    "payment required: {} USDC to {} (mint {}) — fund it with cloudiy_pay_escrow \
                     and retry with escrow_account + job_id",
                    q.price_usdc(),
                    q.pay_to,
                    q.asset
                )),
                Err(e) => Err(err_str(e)),
            }
        }
        "cloudiy_launch" => {
            use cloudiy_sdk::protocol::{Capability, ResourceKind, ResourceVector};
            let image = req_str(args, "image")?;
            let command: Vec<String> = args["command"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if command.is_empty() {
                return Err("missing required argument: command".into());
            }
            let cpu = args["cpu"].as_f64().unwrap_or(1.0);
            let memory_mb = args["memory_mb"].as_u64().unwrap_or(512);
            let caps: Vec<String> = args["capabilities"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let spec = cloudiy_sdk::WorkloadSpec {
                image: Some(image.to_string()),
                command,
                resources: ResourceVector::new()
                    .with(ResourceKind::Cpu, (cpu * 1000.0).round() as u64)
                    .with(ResourceKind::Memory, memory_mb),
                capabilities: caps.iter().map(Capability::new).collect(),
                max_duration_secs: args["timeout_secs"].as_u64().unwrap_or(300),
                ..Default::default()
            };
            // Explicit node, or client-side scheduling over the directories —
            // same policy as the CLI, without its stdout prints.
            let to = match args["node_id"].as_str().filter(|s| !s.is_empty()) {
                Some(node) => node.to_string(),
                None => {
                    let dirs = directories_for(sess, args);
                    if dirs.is_empty() {
                        return Err(
                            "no node_id and no directory to schedule over — pass one of them"
                                .into(),
                        );
                    }
                    let nodes = crate::client::fetch_providers(&dirs)
                        .await
                        .map_err(err_str)?;
                    if nodes.is_empty() {
                        return Err("no live providers on these directories".into());
                    }
                    Pipeline::default_policy()
                        .place(&spec, &nodes)
                        .ok_or("no provider satisfies this workload's requirements")?
                        .node
                        .to_string()
                }
            };
            let token = args["token"].as_str().map(String::from);
            let client = Client::connect(&to).await.map_err(err_str)?;
            let outcome = client.run_workload(spec, token, None).await;
            client.close().await;
            match outcome {
                Ok(r) => {
                    let mut v = job_result_json(&r);
                    v["node_id"] = json!(to);
                    Ok(v)
                }
                Err(SubmitError::PaymentRequired(q)) => Err(format!(
                    "payment required: {} USDC to {} — fund it with cloudiy_pay_escrow",
                    q.price_usdc(),
                    q.pay_to
                )),
                Err(e) => Err(err_str(e)),
            }
        }
        "cloudiy_deploy" => {
            use cloudiy_sdk::protocol::{Capability, ResourceKind, ResourceVector};
            let template = args["template"].as_str().filter(|s| !s.is_empty());
            let image = args["image"].as_str().filter(|s| !s.is_empty());
            if template.is_none() && image.is_none() {
                return Err("provide a `template` recipe name or an `image`".into());
            }
            let str_vec = |k: &str| -> Vec<String> {
                args[k]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            // Environment variables (BTreeMap on the spec).
            let mut env = std::collections::BTreeMap::new();
            if let Some(obj) = args["env"].as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        env.insert(k.clone(), s.to_string());
                    }
                }
            }
            let cpu = args["cpu"].as_f64().unwrap_or(1.0);
            let memory_mb = args["memory_mb"].as_u64().unwrap_or(1024);
            let ports: Vec<u16> = args["ports"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_u64().map(|n| n as u16))
                        .collect()
                })
                .unwrap_or_default();
            let spec = cloudiy_sdk::WorkloadSpec {
                image: image.map(String::from),
                template: template.map(String::from),
                command: str_vec("command"),
                env,
                ports,
                resources: ResourceVector::new()
                    .with(ResourceKind::Cpu, (cpu * 1000.0).round() as u64)
                    .with(ResourceKind::Memory, memory_mb),
                capabilities: str_vec("capabilities")
                    .iter()
                    .map(Capability::new)
                    .collect(),
                max_duration_secs: args["timeout_secs"].as_u64().unwrap_or(600),
                ..Default::default()
            };
            let to = match args["node_id"].as_str().filter(|s| !s.is_empty()) {
                Some(node) => node.to_string(),
                None => {
                    let dirs = directories_for(sess, args);
                    if dirs.is_empty() {
                        return Err(
                            "no node_id and no directory to schedule over — pass one".into()
                        );
                    }
                    let nodes = crate::client::fetch_providers(&dirs)
                        .await
                        .map_err(err_str)?;
                    if nodes.is_empty() {
                        return Err("no live providers on these directories".into());
                    }
                    Pipeline::default_policy()
                        .place(&spec, &nodes)
                        .ok_or("no provider satisfies this deploy's requirements")?
                        .node
                        .to_string()
                }
            };
            // Keyless paid deploy: escrow_account + job_id, signed like A4.
            let payment =
                if let Some(acct) = args["escrow_account"].as_str().filter(|s| !s.is_empty()) {
                    let id = args["job_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("escrow_account requires job_id (from cloudiy_pay_escrow)")?;
                    let job_bytes = crate::core::uuid_bytes(id)
                        .ok_or("job_id must be the UUID the escrow was funded with")?;
                    let kp = load_keypair(sess)?;
                    // Workloads carry no input_data; sign over an empty input to
                    // match what the provider verifies (§4).
                    let sig =
                        kp.sign_message(&crate::payments::run_auth_message(&job_bytes, &[]));
                    Some(cloudiy_sdk::escrow_payment_payload(acct, &hex::encode(sig)))
                } else {
                    None
                };
            let token = args["token"].as_str().map(String::from);
            let client = Client::connect(&to).await.map_err(err_str)?;
            let outcome = client.run_workload(spec, token, payment).await;
            client.close().await;
            match outcome {
                Ok(r) => {
                    let mut v = job_result_json(&r);
                    v["node_id"] = json!(to);
                    v["deployed"] = json!(template.or(image));
                    Ok(v)
                }
                Err(SubmitError::PaymentRequired(q)) => Err(format!(
                    "payment required: {} USDC to {} — fund it with cloudiy_pay_escrow and \
                     retry with escrow_account + job_id",
                    q.price_usdc(),
                    q.pay_to
                )),
                Err(e) => Err(err_str(e)),
            }
        }
        "cloudiy_pay_escrow" => {
            let node_id = req_str(args, "node_id")?;
            // Resolve the amount before signing so the caps see the real number.
            let amount = match args["amount_usdc"].as_f64() {
                Some(a) => a,
                None => {
                    let client = Client::connect(node_id).await.map_err(err_str)?;
                    let info = client.info().await.map_err(err_str);
                    client.close().await;
                    info?.price_usdc
                }
            };
            check_caps(sess, amount)?;
            let timeout = args["timeout_secs"].as_i64().unwrap_or(3600);
            let s = crate::client::fund_escrow(
                node_id,
                sess.opts.keypair.clone(),
                &sess.opts.rpc_url,
                Some(amount),
                timeout,
            )
            .await
            .map_err(err_str)?;
            sess.spent_micro_usdc += s.amount_micro;
            Ok(json!({
                "escrow_account": s.escrow_account,
                "job_id": s.job_uuid,
                "tx": s.tx,
                "amount_usdc": s.amount_micro as f64 / 1_000_000.0,
                "provider_pubkey": s.provider_pubkey,
                "usdc_mint": s.usdc_mint,
                "session_spent_usdc": sess.spent_micro_usdc as f64 / 1_000_000.0,
                "session_cap_usdc": sess.opts.max_spend_usdc,
                "next": "call cloudiy_run_job with this escrow_account and job_id",
            }))
        }
        "cloudiy_release_verified" => {
            let escrow = req_str(args, "escrow_account")?;
            let signature_hex = req_str(args, "signature_hex")?;
            let node_key_hex = req_str(args, "node_key_hex")?;
            let output: Vec<u8> = match args["output_b64"].as_str().filter(|s| !s.is_empty()) {
                Some(b64) => base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .map_err(|e| format!("bad output_b64: {e}"))?,
                None => req_str(args, "output")?.as_bytes().to_vec(),
            };
            let kp = load_keypair(sess)?;
            let program =
                crate::solana::parse_pubkey(crate::core::ESCROW_PROGRAM).map_err(err_str)?;
            let job_account = crate::solana::parse_pubkey(escrow).map_err(err_str)?;
            let node_key = crate::solana::parse_hex32(node_key_hex).map_err(err_str)?;
            let signature = crate::solana::parse_hex64(signature_hex).map_err(err_str)?;
            let r = crate::solana::release_verified(
                &sess.opts.rpc_url,
                &kp,
                &program,
                &job_account,
                &node_key,
                &output,
                &signature,
            )
            .await
            .map_err(err_str)?;
            Ok(json!({
                "tx": r.signature,
                "provider": crate::solana::pubkey_str(&r.provider),
                "payout_micro_usdc": r.payout,
                "fee_micro_usdc": r.fee,
                "note": "signature re-verified on-chain by the escrow contract before payout",
            }))
        }
        "cloudiy_refund" => {
            let escrow = req_str(args, "escrow_account")?;
            let kp = load_keypair(sess)?;
            let program =
                crate::solana::parse_pubkey(crate::core::ESCROW_PROGRAM).map_err(err_str)?;
            let job_account = crate::solana::parse_pubkey(escrow).map_err(err_str)?;
            let r = crate::solana::refund(&sess.opts.rpc_url, &kp, &program, &job_account)
                .await
                .map_err(err_str)?;
            Ok(json!({
                "tx": r.signature,
                "amount_micro_usdc": r.amount,
                "refunded_to": crate::solana::pubkey_str(&r.consumer),
            }))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

/// JSON view of a signed job result, output as utf8 when possible else base64.
fn job_result_json(r: &cloudiy_sdk::JobResult) -> Value {
    let (output, encoding) = match std::str::from_utf8(&r.output) {
        Ok(s) => (json!(s), "utf8"),
        Err(_) => (
            json!(base64::engine::general_purpose::STANDARD.encode(&r.output)),
            "base64",
        ),
    };
    json!({
        "job_id": r.job_id,
        "output": output,
        "output_encoding": encoding,
        "signature_verified": r.signature_verified,
        "signature": r.signature,
        "signed_by": r.signed_by,
        "provider_pubkey": r.provider_pubkey,
        "payment_receipt": r.payment_receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> McpOpts {
        McpOpts {
            rpc_url: "https://api.devnet.solana.com".into(),
            keypair: None,
            max_spend_usdc: 1.0,
            max_per_job_usdc: 0.25,
            directory: vec![],
            allow_mainnet: false,
            read_only: false,
        }
    }

    fn session() -> Session {
        Session {
            opts: opts(),
            spent_micro_usdc: 0,
        }
    }

    #[test]
    fn rpc_guard_classifies_by_host_not_substring() {
        // Recognized non-mainnet hosts pass.
        assert!(rpc_is_non_mainnet("https://api.devnet.solana.com"));
        assert!(rpc_is_non_mainnet("https://api.testnet.solana.com"));
        assert!(rpc_is_non_mainnet("http://127.0.0.1:8899"));
        assert!(rpc_is_non_mainnet("http://localhost:8899"));
        // Mainnet is refused even if the path/query mentions devnet — the old
        // `contains("devnet")` check would have wrongly allowed this.
        assert!(!rpc_is_non_mainnet(
            "https://api.mainnet-beta.solana.com/?tag=devnet"
        ));
        // An unrecognized custom RPC is treated as potentially-mainnet.
        assert!(!rpc_is_non_mainnet("https://my-rpc.example.com"));
    }

    #[tokio::test]
    async fn initialize_echoes_known_version_and_defaults_unknown() {
        let mut s = session();
        let r = dispatch(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2024-11-05"}}),
        )
        .await
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(r["result"]["serverInfo"]["name"], "cloudiy");

        let r = dispatch(
            &mut s,
            json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                   "params":{"protocolVersion":"9999-01-01"}}),
        )
        .await
        .unwrap();
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn tools_list_full_and_read_only() {
        let names = |tools: &Value| -> Vec<String> {
            tools
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect()
        };
        let mut s = session();
        let r = dispatch(
            &mut s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await
        .unwrap();
        let all = names(&r["result"]["tools"]);
        assert_eq!(all.len(), 8);
        assert!(all.contains(&"cloudiy_pay_escrow".to_string()));
        assert!(all.contains(&"cloudiy_deploy".to_string()));

        s.opts.read_only = true;
        let r = dispatch(
            &mut s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await
        .unwrap();
        let ro = names(&r["result"]["tools"]);
        assert_eq!(ro.len(), 5);
        assert!(!ro.contains(&"cloudiy_pay_escrow".to_string()));
        assert!(ro.contains(&"cloudiy_deploy".to_string()));
    }

    #[tokio::test]
    async fn unknown_method_is_32601_and_notifications_are_silent() {
        let mut s = session();
        let r = dispatch(&mut s, json!({"jsonrpc":"2.0","id":7,"method":"nope"}))
            .await
            .unwrap();
        assert_eq!(r["error"]["code"], -32601);

        let none = dispatch(
            &mut s,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        )
        .await;
        assert!(none.is_none());
    }

    #[test]
    fn spend_caps_enforced() {
        let mut s = session();
        // per-job cap
        assert!(check_caps(&s, 0.5).is_err());
        assert!(check_caps(&s, 0.25).is_ok());
        // session cap
        s.spent_micro_usdc = 900_000; // 0.9 USDC spent of the 1.0 cap
        assert!(check_caps(&s, 0.2).is_err());
        assert!(check_caps(&s, 0.05).is_ok());
        // nonsense amounts
        assert!(check_caps(&s, 0.0).is_err());
        assert!(check_caps(&s, -1.0).is_err());
    }

    #[tokio::test]
    async fn read_only_refuses_payment_calls() {
        let mut s = session();
        s.opts.read_only = true;
        let err = call_tool(&mut s, "cloudiy_pay_escrow", &json!({"node_id":"x"}))
            .await
            .unwrap_err();
        assert!(err.contains("--read-only"));
    }
}
