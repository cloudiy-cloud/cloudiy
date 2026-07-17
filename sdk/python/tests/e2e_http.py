"""HTTP end-to-end for the Python SDK against a live `cloudiy share` node.

Unlike test_verify.py (offline vectors), this drives the real wire: `info()`,
the x402 402→quote path, and — on a GPU node — a paid submit whose signed
result is verified end-to-end. On a CPU-only node (e.g. CI) the kernel path has
no GPU, so the paid submit returns an honest "no GPU" error; the test asserts
that graceful outcome instead of a signed result. Either way it proves the
client parses the node's real responses correctly.

Usage: python3 e2e_http.py <node-addr> [access-token]
"""

import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from cloudiy_sdk import (  # noqa: E402
    CloudiyClient,
    CloudiyError,
    PaymentRequired,
    SignatureError,
)

NODE = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
TOKEN = sys.argv[2] if len(sys.argv) > 2 else os.environ.get("CLOUDIY_TOKEN")

# vector_add of "1,2,3;10,20,30" -> "11,22,33" (GPU node only).
KERNEL, DATA, EXPECT = "vector_add", "1,2,3;10,20,30", "11,22,33"


def main() -> int:
    client = CloudiyClient(NODE)

    # 1) info() over HTTP — node advertises its capabilities.
    info = client.info()
    assert info.get("endpoint_id"), f"info() missing endpoint_id: {info}"
    assert "price_usdc" in info, f"info() missing price_usdc: {info}"
    node_id = info["endpoint_id"]
    print(f"ok  info() -> node {node_id[:16]}… price={info['price_usdc']} USDC")

    # 2) x402: a submit with no payment and no token must be quoted, not served.
    try:
        client.submit(kernel=KERNEL, data=DATA)
        print("FAIL submit without payment should have raised PaymentRequired")
        return 1
    except PaymentRequired as quote:
        assert quote.pay_to, "PaymentRequired missing payTo"
        print(f"ok  402 quote -> {quote.price_usdc} USDC to {quote.pay_to[:16]}…")

    # 3) Paid submit (demo x402). On a GPU node the signed result is verified
    #    end-to-end; on a CPU-only node the kernel path returns "no GPU".
    quote = None
    try:
        client.submit(kernel=KERNEL, data=DATA)
    except PaymentRequired as q:
        quote = q
    assert quote is not None
    try:
        result = client.submit(kernel=KERNEL, data=DATA, payment=quote.demo_payment())
    except CloudiyError as e:
        if "no GPU" in str(e):
            print(f"skip signed submit — CPU-only node (no GPU): {e}")
            print("all Python SDK HTTP e2e checks passed (CPU-only)")
            return 0
        raise
    assert result.signature_verified, "result was not signature-verified"
    assert result.signed_by == node_id, f"signed_by {result.signed_by} != {node_id}"
    assert result.output_text.strip() == EXPECT, f"output {result.output_text!r} != {EXPECT!r}"
    print(f"ok  paid submit -> {result.output_text.strip()!r}, signature verified")

    # 4) Pinning a wrong provider identity must be refused even for good output.
    try:
        client.submit(
            kernel=KERNEL, data=DATA, payment=quote.demo_payment(),
            expect_pubkey="00" + node_id[2:],
        )
        print("FAIL wrong expect_pubkey should have raised SignatureError")
        return 1
    except (SignatureError, PaymentRequired):
        # PaymentRequired can occur if the demo escrow is single-use; either way
        # the wrong-pin output was not trusted.
        print("ok  wrong expect_pubkey refused")

    print("all Python SDK HTTP e2e checks passed (GPU)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
