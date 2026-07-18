// HTTP end-to-end for the JS SDK against a live `cloudiy share` node.
//
// Unlike test.mjs (offline vectors), this drives the real wire: info(), the
// x402 402->quote path, and — on a GPU node — a paid submit whose signed result
// is verified end-to-end. On a CPU-only node (e.g. CI) the kernel path has no
// GPU, so the paid submit reports an honest "no GPU" error; the test asserts
// that graceful outcome instead of a signed result. Either way it proves the
// client parses the node's real responses correctly.
//
// Usage: node e2e_http.mjs <node-addr> [access-token]
import assert from "node:assert";
import { CloudiyClient, PaymentRequiredError, SignatureError } from "./cloudiy.mjs";

const NODE = process.argv[2] ?? "127.0.0.1:8080";

// vector_add of "1,2,3;10,20,30" -> "11,22,33" (GPU node only).
const KERNEL = "vector_add", DATA = "1,2,3;10,20,30", EXPECT = "11,22,33";

async function expectQuote(client) {
  try {
    await client.submit({ kernel: KERNEL, data: DATA });
    return null;
  } catch (e) {
    if (e instanceof PaymentRequiredError) return e;
    throw e;
  }
}

async function main() {
  const client = new CloudiyClient(NODE);

  // 1) info() over HTTP.
  const info = await client.info();
  assert(info.endpoint_id, `info() missing endpoint_id: ${JSON.stringify(info)}`);
  assert("price_usdc" in info, "info() missing price_usdc");
  const nodeId = info.endpoint_id;
  console.log(`ok  info() -> node ${nodeId.slice(0, 16)}… price=${info.price_usdc} USDC`);

  // 2) x402: no payment, no token -> quoted, not served.
  const quote = await expectQuote(client);
  assert(quote, "submit without payment should have thrown PaymentRequiredError");
  assert(quote.payTo, "PaymentRequiredError missing payTo");
  console.log(`ok  402 quote -> ${quote.priceUsdc} USDC to ${quote.payTo.slice(0, 16)}…`);

  // 3) Paid submit (demo x402). GPU node -> verified signed result; CPU-only
  //    node -> honest "no GPU" error.
  let result;
  try {
    result = await client.submit({ kernel: KERNEL, data: DATA, payment: quote.demoPayment() });
  } catch (e) {
    if (e instanceof Error && /no GPU/.test(e.message)) {
      console.log(`skip signed submit — CPU-only node (no GPU): ${e.message}`);
      console.log("all JS SDK HTTP e2e checks passed (CPU-only)");
      return 0;
    }
    throw e;
  }
  assert(result.signatureVerified, "result was not signature-verified");
  assert.strictEqual(result.signedBy, nodeId, `signedBy ${result.signedBy} != ${nodeId}`);
  assert.strictEqual(result.outputText.trim(), EXPECT, `output ${result.outputText} != ${EXPECT}`);
  console.log(`ok  paid submit -> "${result.outputText.trim()}", signature verified`);

  // 4) Wrong provider pin is refused even for good output.
  try {
    await client.submit({
      kernel: KERNEL, data: DATA, payment: quote.demoPayment(),
      expectPubkey: "00" + nodeId.slice(2),
    });
    console.log("FAIL wrong expectPubkey should have thrown SignatureError");
    return 1;
  } catch (e) {
    if (e instanceof SignatureError || e instanceof PaymentRequiredError) {
      console.log("ok  wrong expectPubkey refused");
    } else throw e;
  }

  console.log("all JS SDK HTTP e2e checks passed (GPU)");
  return 0;
}

main().then((code) => process.exit(code)).catch((e) => {
  console.error(`FAIL ${e.stack || e.message}`);
  process.exit(1);
});
