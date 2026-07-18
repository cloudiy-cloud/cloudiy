// Type declarations for the Cloudiy JS SDK (cloudiy.mjs).

/** x402 quote thrown when the node wants USDC before executing. */
export class PaymentRequiredError extends Error {
  constructor(requirements: unknown);
  readonly raw: unknown;
  readonly priceMicroUsdc: number;
  readonly priceUsdc: number;
  readonly payTo: string;
  readonly asset: string;
  readonly network: string;
  readonly escrowProgram: string;
  /** Base64 x402 payload for flow demos — real settlement uses the escrow. */
  demoPayment(): string;
}

/** Transport/protocol failure (unreachable node, HTTP error, node-reported
 * job error). */
export class CloudiyError extends Error {}

/** A result's provider signature was missing, invalid, or not from the expected
 * node — the output must not be trusted. */
export class SignatureError extends CloudiyError {}

/** True iff `signatureHex` is a valid provider signature over
 * (jobId, sha256(input), sha256(output)) by the node whose hex EndpointId is
 * `signedBy` (domain `cloudiy/result/v2`). */
export function verifyResult(
  signedBy: string,
  jobId: string,
  input: Uint8Array,
  output: Uint8Array,
  signatureHex: string,
): Promise<boolean>;

export interface CloudiyClientOptions {
  /** Auth token / access code the provider printed at startup. */
  token?: string;
  /** Per-request timeout in milliseconds (default 90000). */
  timeoutMs?: number;
  /** Extra attempts idempotent GETs make on transient failures (default 2).
   * submit() is never auto-retried. */
  retries?: number;
}

export interface SubmitOptions {
  kernel: string;
  data: string | Uint8Array;
  params?: Record<string, string>;
  token?: string;
  /** Base64 x402 payment payload. */
  payment?: string;
  /** Verify the provider's result signature (default true). */
  verify?: boolean;
  /** Pin the provider's hex node identity. */
  expectPubkey?: string | null;
}

export interface JobResult {
  jobId: string;
  status: string;
  output: Uint8Array;
  outputText: string;
  providerPubkey: string | null;
  paymentReceipt: unknown | null;
  signature: string | null;
  signedBy: string | null;
  signatureVerified: boolean;
}

export class CloudiyClient {
  constructor(node?: string, options?: CloudiyClientOptions);
  health(): Promise<Record<string, unknown>>;
  /** Node capabilities: GPU model, VRAM, price (USDC), escrow program. */
  info(): Promise<Record<string, unknown>>;
  status(jobId: string): Promise<Record<string, unknown>>;
  /** Run a kernel on the node's GPU. Throws PaymentRequiredError with the x402
   * quote when payment is needed, SignatureError when the result is untrusted. */
  submit(options: SubmitOptions): Promise<JobResult>;
}

export interface ToolSchema {
  name: string;
  description: string;
  input_schema: {
    type: "object";
    properties: Record<string, unknown>;
    required: string[];
  };
}

/** Function-calling tool schema so LLM agents can invoke Cloudiy GPU compute. */
export function asToolSchema(node?: string): ToolSchema;
