"""Cloudiy SDK for Python — run GPU jobs on the Cloudiy network.

Zero dependencies (stdlib only), built for apps and AI agents:

    from cloudiy_sdk import CloudiyClient, PaymentRequired

    client = CloudiyClient("127.0.0.1:8080")
    print(client.info())

    try:
        result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
    except PaymentRequired as quote:
        print(f"Node charges {quote.price_usdc} USDC -> pay to {quote.pay_to}")
        result = client.submit(kernel="vector_add", data="1,2,3;4,5,6",
                               payment=quote.demo_payment())
    print(result.output_text)

Payments follow the x402 protocol: a submit without payment raises
:class:`PaymentRequired` carrying the node's USDC quote; settle it (Cloudiy
escrow on Solana devnet) and retry with a payment payload.
"""

from __future__ import annotations

import base64
import hashlib
import json
import time
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from typing import Any, Dict, Optional

__all__ = [
    "CloudiyClient",
    "PaymentRequired",
    "JobResult",
    "CloudiyError",
    "SignatureError",
    "as_tool_schema",
]
__version__ = "0.3.0"

_TIMEOUT = 90  # seconds — GPU jobs are bounded to 60 s node-side


# --------------------------------------------------------------------------
# Result-signature verification (mirrors crates/common/src/sig.rs).
#
# The provider signs (job_id, sha256(input), sha256(output)) with its node key
# — the ed25519 key behind its iroh EndpointId — so a consumer can prove OFFLINE
# *which node* produced *which output* for *which input* under *which job*. This
# SDK verifies that signature by default: a tampered/unsigned result, or one
# whose input does not match, raises SignatureError, so an agent never silently
# trusts unverified output. Verification uses only public data, so a
# self-contained (stdlib-only) Ed25519 verify is safe here — no external crypto
# dependency, keeping the SDK zero-dependency.
# --------------------------------------------------------------------------

_RESULT_DOMAIN = b"cloudiy/result/v2"


def _result_signing_payload(job_id: str, input_data: bytes, output: bytes) -> bytes:
    return (
        _RESULT_DOMAIN
        + b"\x00"
        + job_id.encode()
        + b"\x00"
        + hashlib.sha256(input_data).digest()
        + b"\x00"
        + hashlib.sha256(output).digest()
    )


# --- Ed25519 verify, RFC 8032, extended twisted-Edwards coordinates ---------
_ED_P = 2**255 - 19
_ED_L = 2**252 + 27742317777372353535851937790883648493
_ED_D = (-121665 * pow(121666, _ED_P - 2, _ED_P)) % _ED_P
_ED_I = pow(2, (_ED_P - 1) // 4, _ED_P)


def _ed_recover_x(y: int, sign: int):
    if y >= _ED_P:
        return None
    x2 = (y * y - 1) * pow(_ED_D * y * y + 1, _ED_P - 2, _ED_P) % _ED_P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_ED_P + 3) // 8, _ED_P)
    if (x * x - x2) % _ED_P != 0:
        x = x * _ED_I % _ED_P
    if (x * x - x2) % _ED_P != 0:
        return None
    if (x & 1) != sign:
        x = _ED_P - x
    return x


_ED_GY = 4 * pow(5, _ED_P - 2, _ED_P) % _ED_P
_ED_GX = _ed_recover_x(_ED_GY, 0)
_ED_G = (_ED_GX, _ED_GY, 1, _ED_GX * _ED_GY % _ED_P)


def _ed_add(P, Q):
    A = (P[1] - P[0]) * (Q[1] - Q[0]) % _ED_P
    B = (P[1] + P[0]) * (Q[1] + Q[0]) % _ED_P
    C = 2 * P[3] * Q[3] * _ED_D % _ED_P
    D = 2 * P[2] * Q[2] % _ED_P
    E, F, G, H = B - A, D - C, D + C, B + A
    return (E * F % _ED_P, G * H % _ED_P, F * G % _ED_P, E * H % _ED_P)


def _ed_mul(s: int, P):
    Q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            Q = _ed_add(Q, P)
        P = _ed_add(P, P)
        s >>= 1
    return Q


def _ed_equal(P, Q) -> bool:
    return (P[0] * Q[2] - Q[0] * P[2]) % _ED_P == 0 and (
        P[1] * Q[2] - Q[1] * P[2]
    ) % _ED_P == 0


def _ed_decompress(b: bytes):
    if len(b) != 32:
        return None
    y = int.from_bytes(b, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _ed_recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % _ED_P)


def _ed25519_verify(pub: bytes, msg: bytes, sig: bytes) -> bool:
    if len(pub) != 32 or len(sig) != 64:
        return False
    A = _ed_decompress(pub)
    if A is None:
        return False
    R = _ed_decompress(sig[:32])
    if R is None:
        return False
    S = int.from_bytes(sig[32:], "little")
    if S >= _ED_L:
        return False
    h = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % _ED_L
    return _ed_equal(_ed_mul(S, _ED_G), _ed_add(R, _ed_mul(h, A)))


def verify_result(
    signed_by: str, job_id: str, input_data: bytes, output: bytes, signature_hex: str
) -> bool:
    """True iff ``signature_hex`` is a valid provider signature over
    ``(job_id, sha256(input), sha256(output))`` by the node whose hex EndpointId
    is ``signed_by``. ``input`` must be the exact bytes submitted, so a provider
    that ran a different prompt cannot produce a verifying signature. Same
    construction as the Rust ``cloudiy_common::sig`` (v2)."""
    try:
        pub = bytes.fromhex(signed_by)
        sig = bytes.fromhex(signature_hex)
    except (ValueError, TypeError):
        return False
    return _ed25519_verify(pub, _result_signing_payload(job_id, input_data, output), sig)


class CloudiyError(RuntimeError):
    """Provider returned an error."""


class SignatureError(CloudiyError):
    """The result's provider signature was missing, invalid, or not from the
    expected node — the output must not be trusted."""


class PaymentRequired(Exception):
    """x402 quote: the node wants USDC before executing.

    Attributes mirror the ``accepts[0]`` entry of the x402 requirements.
    """

    def __init__(self, requirements: Dict[str, Any]):
        self.raw = requirements
        offer = (requirements.get("accepts") or [{}])[0]
        self.price_micro_usdc = int(offer.get("maxAmountRequired") or 0)
        self.price_usdc = self.price_micro_usdc / 1_000_000
        self.pay_to = offer.get("payTo", "")
        self.asset = offer.get("asset", "")
        self.network = offer.get("network", "")
        self.escrow_program = (offer.get("extra") or {}).get("escrowProgram", "")
        super().__init__(
            f"payment required: {self.price_usdc} USDC to {self.pay_to} "
            f"(escrow {self.escrow_program})"
        )

    def demo_payment(self) -> str:
        """Base64 x402 payload for flow demos — real settlement uses the
        Cloudiy escrow program on Solana devnet."""
        payload = {
            "x402Version": 1,
            "scheme": "exact",
            "network": self.network or "solana-devnet",
            "payload": {"note": "demo payment — settlement via Cloudiy escrow (devnet)"},
        }
        return base64.b64encode(json.dumps(payload).encode()).decode()


@dataclass
class JobResult:
    job_id: str
    output: bytes
    status: str
    provider_pubkey: Optional[str] = None
    payment_receipt: Optional[Dict[str, Any]] = None
    #: True when the result carried a valid ed25519 signature from the
    #: provider node (and matched ``expect_pubkey`` when one was pinned).
    signature_verified: bool = False
    #: Hex ed25519 result signature (also feeds on-chain ``release_verified``).
    signature: Optional[str] = None
    #: Hex node key (iroh EndpointId) that produced the signature.
    signed_by: Optional[str] = None

    @property
    def output_text(self) -> str:
        return self.output.decode("utf-8", errors="replace")


@dataclass
class CloudiyClient:
    """HTTP client for a Cloudiy node (``cloudiy share`` exposes the API).

    For the P2P transport (dial-by-NodeID, NAT traversal) use the Rust SDK;
    this client targets the node's HTTP endpoint — ideal for agents,
    notebooks and backends.
    """

    node: str = "127.0.0.1:8080"
    token: Optional[str] = None
    timeout: float = _TIMEOUT
    #: How many extra attempts idempotent GETs (info/health/status) make on a
    #: transient failure (connection error, timeout, HTTP 5xx). ``submit`` is
    #: never auto-retried — a paid job must not be resent and double-charged.
    retries: int = 2
    _base: str = field(init=False, repr=False, default="")

    def __post_init__(self) -> None:
        node = self.node
        self._base = node if "://" in node else f"http://{node}"

    # -- low-level ---------------------------------------------------------

    def _get(self, path: str) -> Dict[str, Any]:
        """GET ``path`` and decode JSON, retrying transient failures with
        exponential backoff. Idempotent, so retries are safe."""
        attempts = max(1, self.retries + 1)
        last: Exception = CloudiyError(f"GET {path} failed")
        for i in range(attempts):
            try:
                req = urllib.request.Request(self._base + path)
                with urllib.request.urlopen(req, timeout=self.timeout) as res:
                    return json.loads(res.read())
            except urllib.error.HTTPError as e:
                # 5xx is transient (retry); 4xx is the caller's fault (don't).
                if e.code < 500 or i == attempts - 1:
                    raise CloudiyError(f"GET {path} -> HTTP {e.code}") from None
                last = e
            except (urllib.error.URLError, TimeoutError, OSError) as e:
                if i == attempts - 1:
                    raise CloudiyError(
                        f"cannot reach node at {self._base} ({path}): {e}"
                    ) from None
                last = e
            time.sleep(0.2 * (2 ** i))  # 0.2s, 0.4s, 0.8s, …
        raise last

    # -- API ----------------------------------------------------------------

    def health(self) -> Dict[str, Any]:
        return self._get("/health")

    def info(self) -> Dict[str, Any]:
        """Node capabilities: GPU model, VRAM, price (USDC), escrow program."""
        return self._get("/info")

    def status(self, job_id: str) -> Dict[str, Any]:
        return self._get(f"/status/{job_id}")

    def submit(
        self,
        kernel: str,
        data: str | bytes,
        params: Optional[Dict[str, str]] = None,
        token: Optional[str] = None,
        payment: Optional[str] = None,
        verify: bool = True,
        expect_pubkey: Optional[str] = None,
    ) -> JobResult:
        """Run a kernel on the node's GPU.

        Raises :class:`PaymentRequired` (with the x402 quote) when the node
        wants USDC and no valid ``payment``/``token`` was given.

        The provider's ed25519 result signature is verified by default: a
        missing or invalid signature raises :class:`SignatureError` so an
        agent never trusts unverified output. Pass ``verify=False`` to accept
        unsigned results (demo / trusted-local nodes). ``expect_pubkey`` pins
        the provider's hex node identity — without it, verification proves the
        output was signed by whoever holds ``signed_by`` (integrity), but not
        that it is the specific node you intended (pin it to get that).
        """
        # Keep the exact input bytes to verify the result signature binds them.
        input_bytes = data.encode() if isinstance(data, str) else data
        body = json.dumps(
            {
                "job_id": str(uuid.uuid4()),
                "kernel": kernel,
                "input_data": list(input_bytes),
                "params": params or {},
                "auth_token": token or self.token or "",
                "consumer_pubkey": None,
                "payment": payment,
            }
        ).encode()

        req = urllib.request.Request(
            self._base + "/submit",
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        if payment:
            req.add_header("X-PAYMENT", payment)

        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as res:
                raw = json.loads(res.read())
                receipt = None
                header = res.headers.get("x-payment-response")
                if header:
                    try:
                        receipt = json.loads(base64.b64decode(header))
                    except (ValueError, json.JSONDecodeError):
                        receipt = None
                if raw.get("status") == "error":
                    raise CloudiyError(raw.get("error_message") or "unknown error")
                job_id = raw["job_id"]
                output = bytes(raw.get("output_data") or [])
                signature = raw.get("signature")
                signed_by = raw.get("signed_by")
                # Verify the provider signature over
                # (job_id, sha256(input), sha256(output)) — binds the output to
                # THIS input. When a pin is given, the signer must also BE that node.
                verified = bool(
                    signature
                    and signed_by
                    and (expect_pubkey is None or signed_by == expect_pubkey)
                    and verify_result(signed_by, job_id, input_bytes, output, signature)
                )
                if verify and not verified:
                    if not signature or not signed_by:
                        raise SignatureError(
                            "result was not signed by the provider — refusing to "
                            "trust output (pass verify=False to accept unsigned)"
                        )
                    if expect_pubkey is not None and signed_by != expect_pubkey:
                        raise SignatureError(
                            f"result signed by {signed_by} but expected "
                            f"{expect_pubkey} — refusing to trust output"
                        )
                    raise SignatureError(
                        "invalid provider signature — result may be tampered; "
                        "refusing to trust output"
                    )
                return JobResult(
                    job_id=job_id,
                    output=output,
                    status=raw.get("status", ""),
                    provider_pubkey=raw.get("provider_pubkey"),
                    payment_receipt=receipt,
                    signature_verified=verified,
                    signature=signature,
                    signed_by=signed_by,
                )
        except urllib.error.HTTPError as e:
            detail = e.read()
            if e.code == 402:
                raise PaymentRequired(json.loads(detail)) from None
            raise CloudiyError(f"HTTP {e.code}: {detail[:300]!r}") from None
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            # A submit is not auto-retried (a paid job must not be resent), so
            # surface the connection failure clearly for the caller to handle.
            raise CloudiyError(
                f"cannot reach node at {self._base} (/submit): {e}"
            ) from None


def as_tool_schema(node: str = "127.0.0.1:8080") -> Dict[str, Any]:
    """OpenAI/Anthropic-style function-tool schema so AI agents can call
    Cloudiy GPU compute as a tool. Pair with :meth:`CloudiyClient.submit`.
    """
    return {
        "name": "cloudiy_gpu_run",
        "description": (
            "Run a compute kernel on a decentralized GPU (Cloudiy network, "
            f"node {node}). Payment in USDC on Solana via x402. Kernels: "
            "vector_add ('a1,a2,...;b1,b2,...'), "
            "matrix_mul ('m,k,n;A row-major;B row-major')."
        ),
        "input_schema": {
            "type": "object",
            "properties": {
                "kernel": {
                    "type": "string",
                    "enum": ["vector_add", "matrix_mul"],
                    "description": "Which GPU kernel to execute",
                },
                "data": {
                    "type": "string",
                    "description": "Kernel input in the documented format",
                },
            },
            "required": ["kernel", "data"],
        },
    }
