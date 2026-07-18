# @cloudiy/sdk (JavaScript)

Run GPU jobs on the [Cloudiy](https://github.com/w3-surfer/cloudiy) network from
JavaScript — zero dependencies (plain `fetch`), Node 18+/browser/edge, built for
apps and **AI agents**. Ships TypeScript types.

```bash
npm install @cloudiy/sdk
```

```js
import { CloudiyClient, PaymentRequiredError } from "@cloudiy/sdk";

const client = new CloudiyClient("127.0.0.1:8080");

console.log(await client.info());            // GPU model, VRAM, price in USDC

try {
  const r = await client.submit({ kernel: "vector_add", data: "1,2,3;4,5,6" });
  console.log(r.outputText, r.signatureVerified);
} catch (e) {
  if (e instanceof PaymentRequiredError) {   // x402: the node quoted its price
    const r = await client.submit({
      kernel: "vector_add", data: "1,2,3;4,5,6", payment: e.demoPayment(),
    });
    console.log(r.outputText);
  } else throw e;
}
```

## Result verification (on by default)

The provider signs `(job_id, sha256(input), sha256(output))` with its node key
(domain `cloudiy/result/v2` — the signature binds the output to the exact input
submitted). `submit()` **verifies that ed25519 signature by default** and throws
`SignatureError` if it is missing or invalid — an agent never acts on unverified
output. The verify is self-contained (BigInt point math + the runtime's
SubtleCrypto), so the SDK stays zero-dependency. Pass `verify: false` for a
trusted-local/demo node, and `expectPubkey` to pin the provider's identity.

## Reliability

Idempotent reads (`info()`, `health()`, `status()`) retry transient failures
(network error, timeout, HTTP 5xx) with exponential backoff — tune with
`new CloudiyClient(node, { retries: 2 })`. `submit()` is **never** auto-retried
(a paid job must not be resent and double-charged); a connection failure throws
`CloudiyError`.

## For AI agents

`asToolSchema()` returns an OpenAI/Anthropic-style function-tool definition; wire
it to your function-calling LLM and dispatch calls to `CloudiyClient.submit`.

MIT · part of the [Cloudiy SDKs](https://github.com/w3-surfer/cloudiy/tree/main/sdk).
