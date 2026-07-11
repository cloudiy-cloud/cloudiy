// Result-signature verification tests for the JS SDK.
//
// The known-good vector is generated from the Rust signer
// (crates/common/src/sig.rs via `cargo run -p cloudiy-common --example
// gen_vectors`), so this asserts cross-language agreement: the BigInt Ed25519
// verify here accepts exactly what the provider produces and rejects tampering.
// Run: `node sdk/js/test.mjs`.
import assert from "node:assert";
import { verifyResult } from "./cloudiy.mjs";

// Vector from the Rust source of truth (seed = [7u8; 32]).
const PUB = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const SIG =
  "693cb327a07c21352bbb08970436d619a4d0399437ae197189b8918052cd9be6" +
  "14ed20f57e00909cf98f9d615166297f0b1dc53fbf048d6b4563026c3fc57f0e";
const JOB = "job-abc-123";
const OUT = Uint8Array.from(Buffer.from("68656c6c6f20636c6f7564697920726573756c74", "hex")); // "hello cloudiy result"

const tests = {
  async valid_signature() {
    assert.strictEqual(await verifyResult(PUB, JOB, OUT, SIG), true);
  },
  async rejects_wrong_job_id() {
    assert.strictEqual(await verifyResult(PUB, "job-x", OUT, SIG), false);
  },
  async rejects_tampered_output() {
    const t = Uint8Array.from([...OUT, 0x21]);
    assert.strictEqual(await verifyResult(PUB, JOB, t, SIG), false);
  },
  async rejects_wrong_pubkey() {
    assert.strictEqual(await verifyResult("00" + PUB.slice(2), JOB, OUT, SIG), false);
  },
  async rejects_malformed_hex() {
    assert.strictEqual(await verifyResult("zz", JOB, OUT, SIG), false);
    assert.strictEqual(await verifyResult(PUB, JOB, OUT, "nothex"), false);
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
