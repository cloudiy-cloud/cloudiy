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

# Vector from the Rust source of truth (seed = [7u8; 32]), v2 (input-bound).
PUB = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
SIG = (
    "b6998b170df90c982e1e09655cdd41ab63fa709300aa252e947f920fffaadbfc"
    "7cd022d932c01f7219e0b16f66715c030dbc16eaa6abfc14baa10d14b87c0407"
)
JOB = "job-abc-123"
INP = bytes.fromhex("74686520636f6e73756d657227732065786163742070726f6d7074")  # "the consumer's exact prompt"
OUT = bytes.fromhex("68656c6c6f20636c6f7564697920726573756c74")  # "hello cloudiy result"


def test_valid_signature():
    assert verify_result(PUB, JOB, INP, OUT, SIG) is True


def test_rejects_wrong_job_id():
    assert verify_result(PUB, "job-x", INP, OUT, SIG) is False


def test_rejects_tampered_input():
    assert verify_result(PUB, JOB, INP + b"!", OUT, SIG) is False


def test_rejects_tampered_output():
    assert verify_result(PUB, JOB, INP, OUT + b"!", SIG) is False


def test_rejects_wrong_pubkey():
    other = "00" + PUB[2:]
    assert verify_result(other, JOB, INP, OUT, SIG) is False


def test_rejects_malformed_hex():
    assert verify_result("zz", JOB, INP, OUT, SIG) is False
    assert verify_result(PUB, JOB, INP, OUT, "nothex") is False


if __name__ == "__main__":
    for name, fn in sorted(globals().items()):
        if name.startswith("test_") and callable(fn):
            fn()
            print(f"ok  {name}")
    print("all Python SDK verification tests passed")
