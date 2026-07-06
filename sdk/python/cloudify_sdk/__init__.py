"""Cloudify SDK for Python — run GPU jobs on the Cloudify network.

Zero dependencies (stdlib only), built for apps and AI agents:

    from cloudify_sdk import CloudifyClient, PaymentRequired

    client = CloudifyClient("127.0.0.1:8080")
    print(client.info())

    try:
        result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
    except PaymentRequired as quote:
        print(f"Node charges {quote.price_usdc} USDC -> pay to {quote.pay_to}")
        result = client.submit(kernel="vector_add", data="1,2,3;4,5,6",
                               payment=quote.demo_payment())
    print(result.output_text)

Payments follow the x402 protocol: a submit without payment raises
:class:`PaymentRequired` carrying the node's USDC quote; settle it (Cloudify
escrow on Solana devnet) and retry with a payment payload.
"""

from __future__ import annotations

import base64
import json
import urllib.error
import urllib.request
import uuid
from dataclasses import dataclass, field
from typing import Any, Dict, Optional

__all__ = ["CloudifyClient", "PaymentRequired", "JobResult", "CloudifyError", "as_tool_schema"]
__version__ = "0.1.0"

_TIMEOUT = 90  # seconds — GPU jobs are bounded to 60 s node-side


class CloudifyError(RuntimeError):
    """Provider returned an error."""


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
        Cloudify escrow program on Solana devnet."""
        payload = {
            "x402Version": 1,
            "scheme": "exact",
            "network": self.network or "solana-devnet",
            "payload": {"note": "demo payment — settlement via Cloudify escrow (devnet)"},
        }
        return base64.b64encode(json.dumps(payload).encode()).decode()


@dataclass
class JobResult:
    job_id: str
    output: bytes
    status: str
    provider_pubkey: Optional[str] = None
    payment_receipt: Optional[Dict[str, Any]] = None

    @property
    def output_text(self) -> str:
        return self.output.decode("utf-8", errors="replace")


@dataclass
class CloudifyClient:
    """HTTP client for a Cloudify node (``cloudify share`` exposes the API).

    For the P2P transport (dial-by-NodeID, NAT traversal) use the Rust SDK;
    this client targets the node's HTTP endpoint — ideal for agents,
    notebooks and backends.
    """

    node: str = "127.0.0.1:8080"
    token: Optional[str] = None
    timeout: float = _TIMEOUT
    _base: str = field(init=False, repr=False, default="")

    def __post_init__(self) -> None:
        node = self.node
        self._base = node if "://" in node else f"http://{node}"

    # -- low-level ---------------------------------------------------------

    def _get(self, path: str) -> Dict[str, Any]:
        req = urllib.request.Request(self._base + path)
        with urllib.request.urlopen(req, timeout=self.timeout) as res:
            return json.loads(res.read())

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
    ) -> JobResult:
        """Run a kernel on the node's GPU.

        Raises :class:`PaymentRequired` (with the x402 quote) when the node
        wants USDC and no valid ``payment``/``token`` was given.
        """
        body = json.dumps(
            {
                "job_id": str(uuid.uuid4()),
                "kernel": kernel,
                "input_data": list(data.encode() if isinstance(data, str) else data),
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
                    raise CloudifyError(raw.get("error_message") or "unknown error")
                return JobResult(
                    job_id=raw["job_id"],
                    output=bytes(raw.get("output_data") or []),
                    status=raw.get("status", ""),
                    provider_pubkey=raw.get("provider_pubkey"),
                    payment_receipt=receipt,
                )
        except urllib.error.HTTPError as e:
            detail = e.read()
            if e.code == 402:
                raise PaymentRequired(json.loads(detail)) from None
            raise CloudifyError(f"HTTP {e.code}: {detail[:300]!r}") from None


def as_tool_schema(node: str = "127.0.0.1:8080") -> Dict[str, Any]:
    """OpenAI/Anthropic-style function-tool schema so AI agents can call
    Cloudify GPU compute as a tool. Pair with :meth:`CloudifyClient.submit`.
    """
    return {
        "name": "cloudify_gpu_run",
        "description": (
            "Run a compute kernel on a decentralized GPU (Cloudify network, "
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
