// Result-signature verification tests for the JS SDK.
//
// The known-good vector is generated from the Rust signer
// (crates/common/src/sig.rs via `cargo run -p cloudiy-common --example
// gen_vectors`), so this asserts cross-language agreement: the BigInt Ed25519
// verify here accepts exactly what the provider produces and rejects tampering.
// Run: `node sdk/js/test.mjs`.
import assert from "node:assert";
import { verifyResult } from "./cloudiy.mjs";

// Vector from the Rust source of truth (seed = [7u8; 32]), v2 (input-bound).
const PUB = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const SIG =
  "b6998b170df90c982e1e09655cdd41ab63fa709300aa252e947f920fffaadbfc" +
  "7cd022d932c01f7219e0b16f66715c030dbc16eaa6abfc14baa10d14b87c0407";
const JOB = "job-abc-123";
const INP = Uint8Array.from(Buffer.from("74686520636f6e73756d657227732065786163742070726f6d7074", "hex")); // "the consumer's exact prompt"
const OUT = Uint8Array.from(Buffer.from("68656c6c6f20636c6f7564697920726573756c74", "hex")); // "hello cloudiy result"

const tests = {
  async valid_signature() {
    assert.strictEqual(await verifyResult(PUB, JOB, INP, OUT, SIG), true);
  },
  async rejects_wrong_job_id() {
    assert.strictEqual(await verifyResult(PUB, "job-x", INP, OUT, SIG), false);
  },
  async rejects_tampered_input() {
    const t = Uint8Array.from([...INP, 0x21]);
    assert.strictEqual(await verifyResult(PUB, JOB, t, OUT, SIG), false);
  },
  async rejects_tampered_output() {
    const t = Uint8Array.from([...OUT, 0x21]);
    assert.strictEqual(await verifyResult(PUB, JOB, INP, t, SIG), false);
  },
  async rejects_wrong_pubkey() {
    assert.strictEqual(await verifyResult("00" + PUB.slice(2), JOB, INP, OUT, SIG), false);
  },
  async rejects_malformed_hex() {
    assert.strictEqual(await verifyResult("zz", JOB, INP, OUT, SIG), false);
    assert.strictEqual(await verifyResult(PUB, JOB, INP, OUT, "nothex"), false);
  },
};

let failed = 0;
for (const [name, fn] of Object.entries(tests)) {
  try {
    await fn();
    console.log(`ok  ${name}`);
  } catch (e) {
    failed++;
    console.error(`FAIL ${name}: ${e.message}`);
  }
}
if (failed) process.exit(1);
console.log("all JS SDK verification tests passed");
