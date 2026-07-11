"""Result-signature verification tests for the Python SDK.

The known-good vector is generated from the Rust signer
(`crates/common/src/sig.rs` via `cargo run -p cloudiy-common --example
gen_vectors`), so this asserts cross-language agreement: the pure-stdlib
Ed25519 verify here accepts exactly what the provider produces and rejects any
tampering. Run: `python -m pytest sdk/python/tests` (or `python test_verify.py`).
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from cloudiy_sdk import verify_result  # noqa: E402

# Vector from the Rust source of truth (seed = [7u8; 32]).
PUB = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
SIG = (
    "693cb327a07c21352bbb08970436d619a4d0399437ae197189b8918052cd9be6"
    "14ed20f57e00909cf98f9d615166297f0b1dc53fbf048d6b4563026c3fc57f0e"
)
JOB = "job-abc-123"
OUT = bytes.fromhex("68656c6c6f20636c6f7564697920726573756c74")  # "hello cloudiy result"


def test_valid_signature():
    assert verify_result(PUB, JOB, OUT, SIG) is True


def test_rejects_wrong_job_id():
    assert verify_result(PUB, "job-x", OUT, SIG) is False


def test_rejects_tampered_output():
    assert verify_result(PUB, JOB, OUT + b"!", SIG) is False


def test_rejects_wrong_pubkey():
    other = "00" + PUB[2:]
    assert verify_result(other, JOB, OUT, SIG) is False


def test_rejects_malformed_hex():
    assert verify_result("zz", JOB, OUT, SIG) is False
    assert verify_result(PUB, JOB, OUT, "nothex") is False


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok  {name}")
    print("all Python SDK verification tests passed")
