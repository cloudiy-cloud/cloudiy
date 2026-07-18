/**
 * Cloudiy SDK for JavaScript (Node 18+ / browsers / edge runtimes).
 * Zero dependencies — plain fetch. Built for apps and AI agents.
 *
 *   import { CloudiyClient, PaymentRequiredError } from "./cloudiy.mjs";
 *
 *   const client = new CloudiyClient("127.0.0.1:8080", { token: "demo" });
 *   console.log(await client.info());
 *
 *   try {
 *     const r = await client.submit({ kernel: "vector_add", data: "1,2;3,4" });
 *     console.log(r.outputText);
 *   } catch (e) {
 *     if (e instanceof PaymentRequiredError) {
 *       // settle the USDC quote (Cloudiy escrow / x402), then retry:
 *       const r = await client.submit({
 *         kernel: "vector_add", data: "1,2;3,4", payment: e.demoPayment(),
 *       });
 *       console.log(r.outputText);
 *     } else throw e;
 *   }
 */

export class PaymentRequiredError extends Error {
  constructor(requirements) {
    const offer = requirements?.accepts?.[0] ?? {};
    const priceMicro = Number(offer.maxAmountRequired ?? 0);
    super(`payment required: ${priceMicro / 1e6} USDC to ${offer.payTo ?? "?"}`);
    this.name = "PaymentRequiredError";
    this.raw = requirements;
    this.priceMicroUsdc = priceMicro;
    this.priceUsdc = priceMicro / 1e6;
    this.payTo = offer.payTo ?? "";
    this.asset = offer.asset ?? "";
    this.network = offer.network ?? "";
    this.escrowProgram = offer.extra?.escrowProgram ?? "";
  }

  /** Base64 x402 payload for flow demos — real settlement uses the escrow. */
  demoPayment() {
    const payload = {
      x402Version: 1,
      scheme: "exact",
      network: this.network || "solana-devnet",
      payload: { note: "demo payment - settlement via Cloudiy escrow (devnet)" },
    };
    // UTF-8-safe base64 (btoa alone rejects non-latin1 characters)
    const bytes = new TextEncoder().encode(JSON.stringify(payload));
    let bin = "";
    for (const b of bytes) bin += String.fromCharCode(b);
    return btoa(bin);
  }
}

/** A transport/protocol failure talking to the node (unreachable node, HTTP
 * error, node-reported job error) — distinct from PaymentRequiredError (a quote)
 * and SignatureError (untrusted output). */
export class CloudiyError extends Error {
  constructor(message) {
    super(message);
    this.name = "CloudiyError";
  }
}

/** Thrown when a result's provider signature is missing, invalid, or not from
 * the expected node — the output must not be trusted. */
export class SignatureError extends CloudiyError {
  constructor(message) {
    super(message);
    this.name = "SignatureError";
  }
}

// --------------------------------------------------------------------------
// Result-signature verification (mirrors crates/common/src/sig.rs).
//
// The provider signs (job_id, sha256(input), sha256(output)) with its node key
// — the ed25519 key behind its iroh EndpointId (domain cloudiy/result/v2) — so
// a consumer can prove offline which node produced which output for which
// input. This SDK verifies that signature by default
// so an agent never silently trusts unverified output. Verification uses only
// public data, so a self-contained Ed25519 verify (BigInt point math + the
// runtime's SubtleCrypto for hashing) keeps the SDK zero-dependency across
// Node, browsers and edge runtimes.
// --------------------------------------------------------------------------

const _P = 2n ** 255n - 19n;
const _EDL = 2n ** 252n + 27742317777372353535851937790883648493n;
const _mod = (a) => ((a %= _P) < 0n ? a + _P : a);
const _pow = (b, e) => { b = _mod(b); let r = 1n; while (e > 0n) { if (e & 1n) r = _mod(r * b); b = _mod(b * b); e >>= 1n; } return r; };
const _inv = (a) => _pow(a, _P - 2n);
const _D = _mod(-121665n * _inv(121666n));
const _II = _pow(2n, (_P - 1n) / 4n);

function _recoverX(y, sign) {
  if (y >= _P) return null;
  let x2 = _mod((y * y - 1n) * _inv(_D * y * y + 1n));
  if (x2 === 0n) return sign ? null : 0n;
  let x = _pow(x2, (_P + 3n) / 8n);
  if (_mod(x * x - x2) !== 0n) x = _mod(x * _II);
  if (_mod(x * x - x2) !== 0n) return null;
  if ((x & 1n) !== BigInt(sign)) x = _P - x;
  return x;
}
const _GY = _mod(4n * _inv(5n));
const _GX = _recoverX(_GY, 0);
const _G = [_GX, _GY, 1n, _mod(_GX * _GY)];

function _ptAdd(P, Q) {
  const A = _mod((P[1] - P[0]) * (Q[1] - Q[0]));
  const B = _mod((P[1] + P[0]) * (Q[1] + Q[0]));
  const C = _mod(2n * P[3] * Q[3] * _D);
  const Dd = _mod(2n * P[2] * Q[2]);
  const E = B - A, F = Dd - C, G = Dd + C, H = B + A;
  return [_mod(E * F), _mod(G * H), _mod(F * G), _mod(E * H)];
}
function _ptMul(s, P) {
  let Q = [0n, 1n, 1n, 0n];
  while (s > 0n) { if (s & 1n) Q = _ptAdd(Q, P); P = _ptAdd(P, P); s >>= 1n; }
  return Q;
}
const _ptEq = (P, Q) => _mod(P[0] * Q[2] - Q[0] * P[2]) === 0n && _mod(P[1] * Q[2] - Q[1] * P[2]) === 0n;
function _toLE(bytes) { let n = 0n; for (let i = bytes.length - 1; i >= 0; i--) n = (n << 8n) | BigInt(bytes[i]); return n; }
function _ptDecompress(b) {
  if (b.length !== 32) return null;
  let y = _toLE(b); const sign = Number((y >> 255n) & 1n); y &= (1n << 255n) - 1n;
  const x = _recoverX(y, sign);
  return x === null ? null : [x, y, 1n, _mod(x * y)];
}
function _hex(s) {
  if (typeof s !== "string" || s.length % 2) return null;
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) { const b = parseInt(s.substr(i * 2, 2), 16); if (Number.isNaN(b)) return null; out[i] = b; }
  return out;
}
const _concat = (...arrs) => { const n = arrs.reduce((s, a) => s + a.length, 0); const o = new Uint8Array(n); let k = 0; for (const a of arrs) { o.set(a, k); k += a.length; } return o; };
const _sha = async (algo, bytes) => new Uint8Array(await crypto.subtle.digest(algo, bytes));

async function _ed25519Verify(pub, msg, sig) {
  if (pub.length !== 32 || sig.length !== 64) return false;
  const A = _ptDecompress(pub); if (!A) return false;
  const R = _ptDecompress(sig.slice(0, 32)); if (!R) return false;
  const S = _toLE(sig.slice(32)); if (S >= _EDL) return false;
  const h = _toLE(await _sha("SHA-512", _concat(sig.slice(0, 32), pub, msg))) % _EDL;
  return _ptEq(_ptMul(S, _G), _ptAdd(R, _ptMul(h, A)));
}

const _RESULT_DOMAIN = new TextEncoder().encode("cloudiy/result/v2");

/** True iff `signatureHex` is a valid provider signature over
 * (jobId, sha256(input), sha256(output)) by the node whose hex EndpointId is
 * `signedBy`. `input` must be the exact bytes submitted, so a provider that ran
 * a different prompt cannot produce a verifying signature. Same construction as
 * the Rust `cloudiy_common::sig` (v2). */
export async function verifyResult(signedBy, jobId, input, output, signatureHex) {
  const pub = _hex(signedBy), sig = _hex(signatureHex);
  if (!pub || !sig) return false;
  const msg = _concat(
    _RESULT_DOMAIN, new Uint8Array([0]),
    new TextEncoder().encode(jobId), new Uint8Array([0]),
    await _sha("SHA-256", input), new Uint8Array([0]),
    await _sha("SHA-256", output),
  );
  return _ed25519Verify(pub, msg, sig);
}

export class CloudiyClient {
  // `retries`: extra attempts idempotent GETs (info/health/status) make on a
  // transient failure (network error, timeout, HTTP 5xx). submit() is never
  // auto-retried — a paid job must not be resent and double-charged.
  constructor(node = "127.0.0.1:8080", { token, timeoutMs = 90_000, retries = 2 } = {}) {
    this.base = node.includes("://") ? node : `http://${node}`;
    this.token = token;
    this.timeoutMs = timeoutMs;
    this.retries = retries;
  }

  async #get(path) {
    const attempts = Math.max(1, this.retries + 1);
    let last;
    for (let i = 0; i < attempts; i++) {
      try {
        const res = await fetch(this.base + path, {
          signal: AbortSignal.timeout(this.timeoutMs),
        });
        // 5xx is transient (retry); 4xx is the caller's fault (don't).
        if (!res.ok) {
          if (res.status < 500 || i === attempts - 1) {
            throw new CloudiyError(`GET ${path} -> HTTP ${res.status}`);
          }
        } else {
          return res.json();
        }
      } catch (e) {
        if (e instanceof CloudiyError) throw e;
        // Network/abort error (fetch throws TypeError / AbortError).
        if (i === attempts - 1) {
          throw new CloudiyError(`cannot reach node at ${this.base} (${path}): ${e.message}`);
        }
        last = e;
      }
      await new Promise((r) => setTimeout(r, 200 * 2 ** i)); // 200ms, 400ms, …
    }
    throw last;
  }

  health() { return this.#get("/health"); }

  /** Node capabilities: GPU model, VRAM, price (USDC), escrow program. */
  info() { return this.#get("/info"); }

  status(jobId) { return this.#get(`/status/${jobId}`); }

  /**
   * Run a kernel on the node's GPU.
   * Throws PaymentRequiredError with the x402 quote when payment is needed.
   */
  async submit({ kernel, data, params = {}, token, payment, verify = true, expectPubkey = null } = {}) {
    const input = typeof data === "string" ? new TextEncoder().encode(data) : data;
    const headers = { "Content-Type": "application/json" };
    if (payment) headers["X-PAYMENT"] = payment;

    // A submit is not auto-retried (a paid job must not be resent), so a
    // connection failure is surfaced as a CloudiyError for the caller to handle.
    let res;
    try {
      res = await fetch(this.base + "/submit", {
        method: "POST",
        headers,
        signal: AbortSignal.timeout(this.timeoutMs),
        body: JSON.stringify({
          job_id: crypto.randomUUID(),
          kernel,
          input_data: Array.from(input),
          params,
          auth_token: token ?? this.token ?? "",
          consumer_pubkey: null,
          payment: payment ?? null,
        }),
      });
    } catch (e) {
      throw new CloudiyError(`cannot reach node at ${this.base} (/submit): ${e.message}`);
    }

    if (res.status === 402) throw new PaymentRequiredError(await res.json());
    if (!res.ok) throw new CloudiyError(`HTTP ${res.status}: ${await res.text()}`);

    const raw = await res.json();
    if (raw.status === "error") throw new CloudiyError(raw.error_message ?? "unknown error");

    let paymentReceipt = null;
    const receiptHeader = res.headers.get("x-payment-response");
    if (receiptHeader) {
      try { paymentReceipt = JSON.parse(atob(receiptHeader)); } catch { /* opaque */ }
    }

    const output = new Uint8Array(raw.output_data ?? []);
    const signature = raw.signature ?? null;
    const signedBy = raw.signed_by ?? null;

    // Verify the provider's signature over (job_id, sha256(input),
    // sha256(output)) by default — binds the output to THIS input.
    // With expectPubkey set, the signer must also BE that node (identity pin);
    // without it, a valid signature proves integrity — that `signedBy` signed
    // this exact output — but not that it is the node you intended.
    let signatureVerified = false;
    if (signature && signedBy && (expectPubkey === null || signedBy === expectPubkey)) {
      signatureVerified = await verifyResult(signedBy, raw.job_id, input, output, signature);
    }
    if (verify && !signatureVerified) {
      if (!signature || !signedBy) {
        throw new SignatureError("result was not signed by the provider — refusing to trust output (pass verify:false to accept unsigned)");
      }
      if (expectPubkey !== null && signedBy !== expectPubkey) {
        throw new SignatureError(`result signed by ${signedBy} but expected ${expectPubkey} — refusing to trust output`);
      }
      throw new SignatureError("invalid provider signature — result may be tampered; refusing to trust output");
    }

    return {
      jobId: raw.job_id,
      status: raw.status,
      output,
      outputText: new TextDecoder().decode(output),
      providerPubkey: raw.provider_pubkey ?? null,
      paymentReceipt,
      signature,
      signedBy,
      signatureVerified,
    };
  }
}

/** Function-calling tool schema so LLM agents can invoke Cloudiy GPU compute. */
export function asToolSchema(node = "127.0.0.1:8080") {
  return {
    name: "cloudiy_gpu_run",
    description:
      `Run a compute kernel on a decentralized GPU (Cloudiy network, node ${node}). ` +
      "Payment in USDC on Solana via x402. Kernels: vector_add ('a1,a2,...;b1,b2,...'), " +
      "matrix_mul ('m,k,n;A row-major;B row-major').",
    input_schema: {
      type: "object",
      properties: {
        kernel: { type: "string", enum: ["vector_add", "matrix_mul"] },
        data: { type: "string", description: "Kernel input in the documented format" },
      },
      required: ["kernel", "data"],
    },
  };
}
