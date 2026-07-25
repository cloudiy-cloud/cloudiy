# Embeddings & Reranking endpoints — design + backend skeleton

Status: design (catalog + pricing landed; worker container is a `workers/`
deliverable). This is the "design + skeleton + report" for the embeddings/rerank
worker the mission scoped — the compute (a model server) lives in `workers/`,
outside the on-chain agent's territory, so the wiring plan and HTTP contract are
here and the container request is in `HANDOFF.md`.

## Why this is the highest-value endpoint to add

1. **Runs on CPU.** The real network will be ordinary machines, not H100s. BGE-M3
   (~1GB) and all-MiniLM-L6-v2 (~90MB) embed on a laptop CPU. This is compute a
   normal provider can actually contribute.
2. **Called constantly.** Every RAG agent embeds documents and queries thousands
   of times a day. It is the highest-frequency, lowest-latency primitive an agent
   needs — exactly the volume a compute network wants.
3. **Deterministic → the perfect quorum load (RFC-0008).** A fixed model +
   fixed input yields **byte-identical** embeddings across providers (fp32, fixed
   pooling, no sampling). So `run --replicas N` can settle it trustlessly: 3
   providers produce the same vector, the quorum agrees, `release_verified`
   pays the majority. Generative models are only *statistically* checkable;
   embeddings are *exactly* checkable. This is the cleanest fit for the whole
   verifiable-settlement thesis.

## Catalog (already landed)

`crates/cloudiy/src/gateway.rs::model_catalog`:

| Key | Model | License | Category | Worker image |
|---|---|---|---|---|
| `bge-m3` | BGE-M3 (BAAI) | MIT | `embed` | `ghcr.io/cloudiy/worker-embed:latest` |
| `minilm` | all-MiniLM-L6-v2 | Apache 2.0 | `embed` | `ghcr.io/cloudiy/worker-embed:latest` |
| `bge-rerank` | BGE-reranker-v2-m3 (BAAI) | Apache 2.0 | `rerank` | `ghcr.io/cloudiy/worker-rerank:latest` |

Pricing (`crates/protocol/src/pricing.rs`, CPU class, posted micro-USDC):
`minilm` 480 · `bge-m3` 800 · `bge-rerank` 1600 — cheap, high-volume.

Until the worker images are published, `serve_endpoint` returns `model_pending`
honestly (no fake output). No node claims to serve them until a real worker
answers.

## Worker HTTP contract (the `workers/` deliverable)

Two small FastAPI servers (or one, two routes), same shape as the existing
whisper/ollama workers — bound to loopback, provisioned by the gateway.

**Embeddings** — `worker-embed` (BGE-M3 + all-MiniLM-L6-v2 via
sentence-transformers):

```
POST /embed
{ "model": "bge-m3" | "minilm", "input": ["text one", "text two"] }
→ 200
{ "model": "bge-m3", "dim": 1024,
  "embeddings": [[0.01, ...], [0.02, ...]] }     # fp32, L2-normalized
```

**Reranking** — `worker-rerank` (BGE-reranker-v2-m3):

```
POST /rerank
{ "query": "…", "documents": ["doc a", "doc b", "doc c"], "top_k": 3 }
→ 200
{ "scores": [{"index": 2, "score": 0.91}, {"index": 0, "score": 0.44}, …] }
```

**Determinism requirements (non-negotiable — this is what makes quorum work):**
- fp32 inference, no fp16/bf16 (rounding diverges across GPUs/CPUs).
- Fixed pooling (BGE-M3: CLS/dense pooling; MiniLM: mean pooling) and fixed
  normalization (L2). Document the exact recipe in the worker README.
- Single-thread or fixed thread count if any reduction order affects the last
  ulp — verify byte-identical output across two hosts before declaring quorum
  support. If bit-exactness can't be guaranteed, expose a quantized-to-int8
  "canonical" vector for the signature and the float vector for use (a follow-up
  if raw fp32 proves host-sensitive).

## Backend wiring plan (gateway side — my territory, trivial once the image ships)

Add, mirroring `run_whisper_worker` (`gateway.rs`):

```rust
const EMBED_WORKER: &str = "cloudiy-wk-embed";
const EMBED_PORT:   &str = "9981";
const EMBED_URL:    &str = "http://127.0.0.1:9981";

async fn run_embed_worker(model: &str, input: &[String]) -> anyhow::Result<serde_json::Value> {
    // ensure the container (same hardening as other workers: pinned digest,
    // signature verify, cap-drop, loopback publish), then POST /embed.
}
```

Then in `serve_endpoint`, before the `model_pending` fallback:

```rust
if let Some((_, _, _, "embed")) = catalog_entry(key) {
    return match run_embed_worker(key, &inputs).await { … };
}
if key == "bge-rerank" { return run_rerank_worker(query, docs).await; }
```

Inputs: embeddings take an array (not a single `prompt`), so `serve_endpoint`
(or `RunEndpoint`) needs an `input: Vec<String>` path alongside the existing
`prompt`. That's the one non-trivial gateway change — a small request-shape
addition — deferred until the worker exists so we don't add an unused code path.

## Sequencing

1. `workers/`: build `worker-embed` + `worker-rerank` to the contract above
   (HANDOFF item). 2. Publish to GHCR, pin by digest. 3. On-chain agent: add the
   two runners + the array-input path + wire into `serve_endpoint`, replacing
   `model_pending` for these keys. 4. Add a canary (deterministic: embed a fixed
   string, assert the known vector) and an RFC-0008 quorum e2e (3 embed replicas
   agree byte-for-byte).
