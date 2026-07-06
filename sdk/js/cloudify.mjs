/**
 * Cloudify SDK for JavaScript (Node 18+ / browsers / edge runtimes).
 * Zero dependencies — plain fetch. Built for apps and AI agents.
 *
 *   import { CloudifyClient, PaymentRequiredError } from "./cloudify.mjs";
 *
 *   const client = new CloudifyClient("127.0.0.1:8080", { token: "demo" });
 *   console.log(await client.info());
 *
 *   try {
 *     const r = await client.submit({ kernel: "vector_add", data: "1,2;3,4" });
 *     console.log(r.outputText);
 *   } catch (e) {
 *     if (e instanceof PaymentRequiredError) {
 *       // settle the USDC quote (Cloudify escrow / x402), then retry:
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
      payload: { note: "demo payment — settlement via Cloudify escrow (devnet)" },
    };
    return btoa(JSON.stringify(payload));
  }
}

export class CloudifyClient {
  constructor(node = "127.0.0.1:8080", { token, timeoutMs = 90_000 } = {}) {
    this.base = node.includes("://") ? node : `http://${node}`;
    this.token = token;
    this.timeoutMs = timeoutMs;
  }

  async #get(path) {
    const res = await fetch(this.base + path, {
      signal: AbortSignal.timeout(this.timeoutMs),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status} on ${path}`);
    return res.json();
  }

  health() { return this.#get("/health"); }

  /** Node capabilities: GPU model, VRAM, price (USDC), escrow program. */
  info() { return this.#get("/info"); }

  status(jobId) { return this.#get(`/status/${jobId}`); }

  /**
   * Run a kernel on the node's GPU.
   * Throws PaymentRequiredError with the x402 quote when payment is needed.
   */
  async submit({ kernel, data, params = {}, token, payment } = {}) {
    const input = typeof data === "string" ? new TextEncoder().encode(data) : data;
    const headers = { "Content-Type": "application/json" };
    if (payment) headers["X-PAYMENT"] = payment;

    const res = await fetch(this.base + "/submit", {
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

    if (res.status === 402) throw new PaymentRequiredError(await res.json());
    if (!res.ok) throw new Error(`HTTP ${res.status}: ${await res.text()}`);

    const raw = await res.json();
    if (raw.status === "error") throw new Error(raw.error_message ?? "unknown error");

    let paymentReceipt = null;
    const receiptHeader = res.headers.get("x-payment-response");
    if (receiptHeader) {
      try { paymentReceipt = JSON.parse(atob(receiptHeader)); } catch { /* opaque */ }
    }

    const output = new Uint8Array(raw.output_data ?? []);
    return {
      jobId: raw.job_id,
      status: raw.status,
      output,
      outputText: new TextDecoder().decode(output),
      providerPubkey: raw.provider_pubkey ?? null,
      paymentReceipt,
    };
  }
}

/** Function-calling tool schema so LLM agents can invoke Cloudify GPU compute. */
export function asToolSchema(node = "127.0.0.1:8080") {
  return {
    name: "cloudify_gpu_run",
    description:
      `Run a compute kernel on a decentralized GPU (Cloudify network, node ${node}). ` +
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
