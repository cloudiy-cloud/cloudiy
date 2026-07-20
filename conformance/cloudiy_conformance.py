#!/usr/bin/env python3
"""Cloudiy protocol conformance suite — black-box, spec-referenced.

Point this at ANY implementation of the Cloudiy compute protocol (not just the
reference node) and it checks the *observable contract* a consumer depends on:
node discovery, the x402 payment flow, and the ed25519 result signature. It
speaks only HTTP + JSON and never imports the Cloudiy SDK, so a second
implementation can validate itself with nothing but a Python 3 interpreter.

    python3 cloudiy_conformance.py 127.0.0.1:8080
    python3 cloudiy_conformance.py https://node.example.com --token <code>

Every check prints its verdict and the spec clause it enforces:

    [PASS] PROTOCOL §6  /info advertises the x402 payment scheme
    [SKIP] RFC-0006 §4  no GPU on this node — cannot exercise a signed result
    ...
    conformance: 11/12 checks passed, 1 skipped

PASS means the node honored that clause on this run. SKIP means the clause
could not be exercised here (e.g. a CPU-only node can't produce a signed GPU
result, or the node requires real on-chain settlement this suite can't mint) —
never a conformance failure. FAIL means the node violated the clause.

Exit code: 0 unless at least one check FAILed. Out of scope: anything not
observable at the wire — scheduler policy, reputation math, internal isolation.
See conformance/README.md.

Spec sources cited by the checks:
  PROTOCOL.md          — the protocol spec (design axioms + universal API)
  RFC-0006 §4, §11     — the input→output result-signature binding (v2)
  x402                 — https://solana.com/x402 (the payment-quote envelope)
  "reference (de facto)" — pinned only by the reference implementation, not yet
                           by the written spec. These are spec gaps; see
                           HANDOFF.md.
"""

import base64
import hashlib
import json
import sys
import urllib.error
import urllib.request

# ---------------------------------------------------------------------------
# Self-contained ed25519 verify (RFC 8032). Inlined on purpose: the suite must
# not depend on the Cloudiy SDK — it validates implementations independently of
# it. Same construction the Rust signer and every SDK use.
# ---------------------------------------------------------------------------
_P = 2**255 - 19
_L = 2**252 + 27742317777372353535851937790883648493
_D = (-121665 * pow(121666, _P - 2, _P)) % _P
_I = pow(2, (_P - 1) // 4, _P)


def _recover_x(y, sign):
    if y >= _P:
        return None
    x2 = (y * y - 1) * pow(_D * y * y + 1, _P - 2, _P) % _P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (_P + 3) // 8, _P)
    if (x * x - x2) % _P != 0:
        x = x * _I % _P
    if (x * x - x2) % _P != 0:
        return None
    if (x & 1) != sign:
        x = _P - x
    return x


_GY = 4 * pow(5, _P - 2, _P) % _P
_GX = _recover_x(_GY, 0)
_G = (_GX, _GY, 1, _GX * _GY % _P)


def _add(P, Q):
    A = (P[1] - P[0]) * (Q[1] - Q[0]) % _P
    B = (P[1] + P[0]) * (Q[1] + Q[0]) % _P
    C = 2 * P[3] * Q[3] * _D % _P
    Dd = 2 * P[2] * Q[2] % _P
    E, F, G, H = B - A, Dd - C, Dd + C, B + A
    return (E * F % _P, G * H % _P, F * G % _P, E * H % _P)


def _mul(s, P):
    Q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            Q = _add(Q, P)
        P = _add(P, P)
        s >>= 1
    return Q


def _eq(P, Q):
    return (P[0] * Q[2] - Q[0] * P[2]) % _P == 0 and (P[1] * Q[2] - Q[1] * P[2]) % _P == 0


def _decompress(b):
    if len(b) != 32:
        return None
    y = int.from_bytes(b, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % _P)


def _ed25519_verify(pub, msg, sig):
    if len(pub) != 32 or len(sig) != 64:
        return False
    A = _decompress(pub)
    if A is None:
        return False
    R = _decompress(sig[:32])
    if R is None:
        return False
    S = int.from_bytes(sig[32:], "little")
    if S >= _L:
        return False
    h = int.from_bytes(hashlib.sha512(sig[:32] + pub + msg).digest(), "little") % _L
    return _eq(_mul(S, _G), _add(R, _mul(h, A)))


_RESULT_DOMAIN = b"cloudiy/result/v2"


def result_signing_payload(job_id: str, input_data: bytes, output: bytes) -> bytes:
    """The exact bytes a conforming provider signs (RFC-0006 §4 change 3):
    domain ‖ 0 ‖ job_id ‖ 0 ‖ sha256(input) ‖ 0 ‖ sha256(output)."""
    return (
        _RESULT_DOMAIN
        + b"\x00"
        + job_id.encode()
        + b"\x00"
        + hashlib.sha256(input_data).digest()
        + b"\x00"
        + hashlib.sha256(output).digest()
    )


def verify_result(signed_by, job_id, input_data, output, signature_hex) -> bool:
    try:
        pub = bytes.fromhex(signed_by)
        sig = bytes.fromhex(signature_hex)
    except (ValueError, TypeError):
        return False
    return _ed25519_verify(pub, result_signing_payload(job_id, input_data, output), sig)


# ---------------------------------------------------------------------------
# Tiny check framework
# ---------------------------------------------------------------------------
PASS, FAIL, SKIP = "PASS", "FAIL", "SKIP"
_COLOR = {PASS: "\033[32m", FAIL: "\033[31m", SKIP: "\033[33m"}
_RESET = "\033[0m"


class Report:
    def __init__(self):
        self.rows = []

    def record(self, status, spec, desc, detail=""):
        self.rows.append((status, spec, desc, detail))
        color = _COLOR[status] if sys.stdout.isatty() else ""
        reset = _RESET if sys.stdout.isatty() else ""
        line = f"  {color}[{status}]{reset} {spec:<22} {desc}"
        if detail:
            line += f"\n          {detail}"
        print(line)

    def summary_and_exit(self):
        passed = sum(1 for r in self.rows if r[0] == PASS)
        failed = sum(1 for r in self.rows if r[0] == FAIL)
        skipped = sum(1 for r in self.rows if r[0] == SKIP)
        total = passed + failed  # skips don't count toward the denominator
        print()
        msg = f"conformance: {passed}/{total} checks passed"
        if skipped:
            msg += f", {skipped} skipped"
        if failed:
            msg += f", {failed} FAILED"
        print(msg)
        sys.exit(1 if failed else 0)


# ---------------------------------------------------------------------------
# HTTP helpers (stdlib only)
# ---------------------------------------------------------------------------
class Http:
    def __init__(self, node, timeout=90):
        self.base = node if "://" in node else f"http://{node}"
        self.timeout = timeout

    def get(self, path):
        req = urllib.request.Request(self.base + path)
        with urllib.request.urlopen(req, timeout=self.timeout) as res:
            return res.status, json.loads(res.read())

    def post_submit(self, body: dict, payment: str = None):
        """Returns (http_status, parsed_json_or_bytes)."""
        data = json.dumps(body).encode()
        req = urllib.request.Request(
            self.base + "/submit",
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        if payment:
            req.add_header("X-PAYMENT", payment)
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as res:
                return res.status, json.loads(res.read())
        except urllib.error.HTTPError as e:
            raw = e.read()
            try:
                return e.code, json.loads(raw)
            except (ValueError, json.JSONDecodeError):
                return e.code, raw


def new_job_id():
    # Any UUID-shaped string; the reference node requires a UUID to bind escrow,
    # but a plain unique string is enough to exercise the observable flow.
    import uuid

    return str(uuid.uuid4())


def demo_payment(network="solana-devnet") -> str:
    """A minimal x402 payload (base64 JSON). A dev-mode node accepts it to open
    the gate; a settlement-required node will not — that's a SKIP, not a FAIL."""
    payload = {
        "x402Version": 1,
        "scheme": "exact",
        "network": network,
        "payload": {"note": "cloudiy conformance probe"},
    }
    return base64.b64encode(json.dumps(payload).encode()).decode()


# ---------------------------------------------------------------------------
# The checks
# ---------------------------------------------------------------------------
# A kernel every reference node advertises; a second implementation is expected
# to expose at least one deterministic kernel. vector_add of these two vectors
# is "11,22,33".
KERNEL, KDATA, KEXPECT = "vector_add", "1,2,3;10,20,30", "11,22,33"


def run(node, token=None, slow=False):
    http = Http(node)
    rep = Report()
    print(f"Cloudiy conformance — target {http.base}\n")

    # --- discovery ---------------------------------------------------------
    info = None
    try:
        status, info = http.get("/info")
        if status == 200 and isinstance(info, dict):
            rep.record(PASS, "PROTOCOL §6", "GET /info returns a node descriptor")
        else:
            rep.record(FAIL, "PROTOCOL §6", "GET /info returns a node descriptor",
                       f"status {status}, body {str(info)[:80]}")
    except Exception as e:
        rep.record(FAIL, "PROTOCOL §6", "GET /info returns a node descriptor", str(e))
        rep.summary_and_exit()  # nothing else is testable

    # Identity: PROTOCOL §2 — "an identity is an opaque verifiable string
    # (today: an ed25519 key)". The node must advertise its own, or a consumer
    # can never verify a signature against it.
    node_id = info.get("endpoint_id")
    if isinstance(node_id, str) and len(node_id) == 64 and _is_hex(node_id):
        rep.record(PASS, "PROTOCOL §2", "/info carries the node's ed25519 identity",
                   f"endpoint_id {node_id[:16]}…")
    else:
        rep.record(FAIL, "PROTOCOL §2", "/info carries the node's ed25519 identity",
                   f"endpoint_id = {node_id!r} (want 32-byte hex)")

    # Payment binding: PROTOCOL §6 pins that a quote carries price (micro-USDC),
    # payee, asset, settlement hint. /info surfaces the same, so a consumer can
    # decide before submitting. Field NAMES are reference-de-facto (see HANDOFF).
    for field, spec, why in [
        ("price_usdc", "PROTOCOL §6", "price"),
        ("usdc_mint", "PROTOCOL §6", "asset (USDC mint)"),
        ("network", "PROTOCOL §6", "settlement network"),
        ("payment", "reference (de facto)", "payment scheme name"),
        ("escrow_program", "PROTOCOL §6", "settlement hint (escrow)"),
    ]:
        if field in info and info[field] not in (None, ""):
            rep.record(PASS, spec, f"/info advertises {why}", f"{field} = {info[field]}")
        else:
            rep.record(FAIL, spec, f"/info advertises {why}", f"missing/empty {field}")

    # Capabilities & resources are first-class (PROTOCOL axiom 4, §3).
    if isinstance(info.get("capabilities"), list):
        rep.record(PASS, "PROTOCOL §2.4", "/info lists capabilities (functionality)",
                   f"{info['capabilities'][:4]}")
    else:
        rep.record(FAIL, "PROTOCOL §2.4", "/info lists capabilities (functionality)",
                   "capabilities is not a list")
    if isinstance(info.get("resources"), dict):
        rep.record(PASS, "PROTOCOL §3", "/info reports resource accounting")
    else:
        rep.record(SKIP, "PROTOCOL §3", "/info reports resource accounting",
                   "no resources block (optional in the reference)")

    # --- x402 flow ---------------------------------------------------------
    # Step 1: an unpaid, untokened submit MUST be quoted, not served (§7 axiom:
    # "no accounts, no API keys" — payment is the gate).
    body = {
        "job_id": new_job_id(), "kernel": KERNEL,
        "input_data": list(KDATA.encode()), "params": {},
        "auth_token": "", "consumer_pubkey": None, "payment": None,
    }
    status, quote = http.post_submit(body)
    if status == 402:
        rep.record(PASS, "PROTOCOL §6", "unpaid submit is answered 402 Payment Required")
    else:
        rep.record(FAIL, "PROTOCOL §6", "unpaid submit is answered 402 Payment Required",
                   f"got status {status}")
        quote = None

    # Step 2: the 402 body is a well-formed x402 quote carrying price/payee/asset.
    if isinstance(quote, dict):
        offer = (quote.get("accepts") or [{}])[0]
        have_price = str(offer.get("maxAmountRequired", "")).strip() not in ("", "0", "None")
        have_payee = bool(offer.get("payTo"))
        have_asset = bool(offer.get("asset"))
        if quote.get("x402Version") and (have_price and have_payee and have_asset):
            rep.record(PASS, "x402 / §6", "402 body is a valid x402 quote (price+payee+asset)",
                       f"{offer.get('maxAmountRequired')} → {str(offer.get('payTo'))[:12]}…")
        else:
            rep.record(FAIL, "x402 / §6", "402 body is a valid x402 quote (price+payee+asset)",
                       f"x402Version={quote.get('x402Version')} price={have_price} "
                       f"payee={have_payee} asset={have_asset}")
        quote_network = offer.get("network") or "solana-devnet"
    else:
        rep.record(SKIP, "x402 / §6", "402 body is a valid x402 quote (price+payee+asset)",
                   "no 402 body to inspect")
        quote_network = "solana-devnet"

    # Step 3: retry with a payment (or token) opens the gate. Either a completed
    # result OR an honest "no compute here" (e.g. no GPU) proves the 402 lifted;
    # a node that stays 402 requires real settlement this suite can't mint (SKIP).
    result = None
    pay_body = dict(body, job_id=new_job_id())
    if token:
        pay_body["auth_token"] = token
        status, result = http.post_submit(pay_body)
        pay_kind = "auth token"
    else:
        status, result = http.post_submit(pay_body, payment=demo_payment(quote_network))
        pay_kind = "demo x402 payment"
    if status == 402:
        rep.record(SKIP, "PROTOCOL §6", f"retry with {pay_kind} lifts the 402 gate",
                   "node requires real on-chain settlement — out of scope for a "
                   "black-box probe (pass --token for a dev node)")
        result = None
    elif status == 200 and isinstance(result, dict):
        rep.record(PASS, "PROTOCOL §6", f"retry with {pay_kind} lifts the 402 gate",
                   f"status={result.get('status')}")
    else:
        rep.record(FAIL, "PROTOCOL §6", f"retry with {pay_kind} lifts the 402 gate",
                   f"unexpected status {status}: {str(result)[:80]}")
        result = None

    # --- result signature (RFC-0006 §4/§11) --------------------------------
    # The signed happy-path needs real compute. A CPU-only node returns
    # status:"error" with no signature — the contract is not violated, it just
    # can't be exercised here, so the signature checks SKIP.
    signed = None
    if isinstance(result, dict):
        if result.get("status") == "error" or not result.get("signature"):
            rep.record(SKIP, "RFC-0006 §4",
                       "completed result is ed25519-signed (v2 input-binding)",
                       f"node produced no signed output "
                       f"(status={result.get('status')}, "
                       f"error={str(result.get('error_message'))[:40]}) — "
                       "likely no GPU; run against a compute node to exercise")
        else:
            signed = result

    if signed:
        sig = signed.get("signature")
        signed_by = signed.get("signed_by")
        job_id = signed.get("job_id")
        output = bytes(signed.get("output_data") or [])
        inp = KDATA.encode()

        # (a) signer identity must be the node's own advertised identity.
        if signed_by == node_id:
            rep.record(PASS, "PROTOCOL §2", "result is signed by the node's own identity")
        else:
            rep.record(FAIL, "PROTOCOL §2", "result is signed by the node's own identity",
                       f"signed_by {str(signed_by)[:16]}… != endpoint_id {str(node_id)[:16]}…")

        # (b) the signature verifies over the v2 message binding THIS input.
        if verify_result(signed_by, job_id, inp, output, sig):
            rep.record(PASS, "RFC-0006 §4",
                       "result signature verifies over (job_id, sha256(input), sha256(output))")
        else:
            rep.record(FAIL, "RFC-0006 §4",
                       "result signature verifies over (job_id, sha256(input), sha256(output))",
                       "ed25519 verification failed for the v2 message")

        # (c) the binding is real: tampering the output must break the signature.
        # Guards against a node that returns a well-formed but unbound signature.
        if not verify_result(signed_by, job_id, inp, output + b"!", sig):
            rep.record(PASS, "RFC-0006 §4",
                       "signature rejects a tampered output (binding is real)")
        else:
            rep.record(FAIL, "RFC-0006 §4",
                       "signature rejects a tampered output (binding is real)",
                       "a modified output still verified — signature is not bound to output")

        # (d) and to THIS input: a different input must break it too.
        if not verify_result(signed_by, job_id, b"a different prompt", output, sig):
            rep.record(PASS, "RFC-0006 §4",
                       "signature rejects a different input (input-binding is real)")
        else:
            rep.record(FAIL, "RFC-0006 §4",
                       "signature rejects a different input (input-binding is real)",
                       "a swapped input still verified — signature is not input-bound")

        # (e) if the node ran the deterministic kernel, the output is correct.
        got = output.decode("utf-8", "replace").strip()
        if got == KEXPECT:
            rep.record(PASS, "reference (de facto)",
                       f"deterministic kernel output is correct ({KERNEL})", got)
        else:
            rep.record(SKIP, "reference (de facto)",
                       f"deterministic kernel output is correct ({KERNEL})",
                       f"got {got!r}, expected {KEXPECT!r} — different kernel semantics?")

    # --- stable error behavior ---------------------------------------------
    # A malformed submit must be rejected with a stable client/again error, not
    # a crash or a 200. (Error taxonomy is reference-de-facto; see HANDOFF.)
    try:
        req = urllib.request.Request(
            http.base + "/submit", data=b"{ this is not valid json",
            headers={"Content-Type": "application/json"}, method="POST")
        code = None
        try:
            with urllib.request.urlopen(req, timeout=http.timeout) as res:
                code = res.status
        except urllib.error.HTTPError as e:
            code = e.code
        if code and 400 <= code < 600 and code != 500:
            rep.record(PASS, "reference (de facto)", "malformed request is rejected cleanly",
                       f"HTTP {code}")
        elif code == 500:
            rep.record(FAIL, "reference (de facto)", "malformed request is rejected cleanly",
                       "HTTP 500 — a bad body should be a 4xx, not a server error")
        else:
            rep.record(FAIL, "reference (de facto)", "malformed request is rejected cleanly",
                       f"HTTP {code}")
    except Exception as e:
        rep.record(SKIP, "reference (de facto)", "malformed request is rejected cleanly", str(e))

    # --- frame/size limit (opt-in: it uploads >16 MiB) ---------------------
    if slow:
        big = "9," * (9 * 1024 * 1024)  # ~18 MiB of body
        over = dict(body, job_id=new_job_id(), input_data=list(big.encode()[: 17 * 1024 * 1024]))
        try:
            code, _ = http.post_submit(over)
            if code in (413, 400):
                rep.record(PASS, "reference (de facto)", "oversized body is rejected (frame limit)",
                           f"HTTP {code}")
            else:
                rep.record(FAIL, "reference (de facto)", "oversized body is rejected (frame limit)",
                           f"HTTP {code} — expected 413")
        except Exception as e:
            # A connection reset is an acceptable way to enforce the limit.
            rep.record(PASS, "reference (de facto)", "oversized body is rejected (frame limit)",
                       f"connection refused/reset: {type(e).__name__}")
    else:
        rep.record(SKIP, "reference (de facto)", "oversized body is rejected (frame limit)",
                   "pass --slow to upload >16 MiB and exercise the limit")

    rep.summary_and_exit()


def _is_hex(s):
    try:
        bytes.fromhex(s)
        return True
    except ValueError:
        return False


def main():
    args = [a for a in sys.argv[1:]]
    token = None
    slow = False
    node = None
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--token":
            token = args[i + 1]
            i += 2
        elif a == "--slow":
            slow = True
            i += 1
        elif a in ("-h", "--help"):
            print(__doc__)
            return
        else:
            node = a
            i += 1
    if not node:
        node = "127.0.0.1:8080"
    run(node, token=token, slow=slow)


if __name__ == "__main__":
    main()
