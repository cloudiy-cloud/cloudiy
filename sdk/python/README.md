# cloudiy-sdk (Python)

Run GPU jobs on the [Cloudiy](https://github.com/cloudiy-cloud/cloudiy) network from Python — zero dependencies, built for apps and **AI agents**.

## Verify or reject — the whole point

Buying compute from a stranger is only safe if you can prove *which node*
produced *which output* for *which input*. Every result is ed25519-signed, and
this SDK checks it **before returning** — so an agent can't act on forged
compute:

```python
result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")  # SignatureError if unsigned/tampered
assert result.signature_verified                                  # True — proof, not trust
print(result.signed_by)                                           # which node actually computed it
```

That's on by default. [Details below.](#result-verification-on-by-default)

```bash
pip install ./sdk/python          # (from the repo; PyPI release planned)
```

```python
from cloudiy_sdk import CloudiyClient, PaymentRequired

client = CloudiyClient("127.0.0.1:8080", token="<access-code>")

print(client.info())              # GPU model, VRAM, price in USDC, escrow program

try:
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
except PaymentRequired as quote:  # x402: the node quoted its USDC price
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6",
                           payment=quote.demo_payment())

print(result.output_text)         # "5,7,9"
print(result.signature_verified)  # True — ed25519 proof of which node computed it
print(result.payment_receipt)     # x402 settlement receipt
```

### Result verification (on by default)

The provider signs `(job_id, sha256(input), sha256(output))` with its node key
(domain `cloudiy/result/v2` — the signature binds the output to the exact input
submitted). `submit()`
**verifies that ed25519 signature by default** and raises `SignatureError` if it
is missing or invalid — an agent never acts on unverified output. The check is
pure-stdlib (the SDK stays zero-dependency). Opt out for a trusted-local/demo
node with `verify=False`, and pin the provider's hex identity with
`expect_pubkey="<node-id>"` (without a pin, a valid signature proves the output
was signed by `result.signed_by`, but not that it is the node you intended).

### Reliability

Idempotent reads (`info()`, `health()`, `status()`) retry transient failures
(connection error, timeout, HTTP 5xx) with exponential backoff — tune with
`CloudiyClient(node, retries=2)`. `submit()` is **never** auto-retried (a paid
job must not be resent and double-charged); a connection failure raises
`CloudiyError` with a clear message instead of a raw `urllib` error.

### For AI agents

`as_tool_schema()` emits an OpenAI/Anthropic-style function-tool definition; wire it to your agent and dispatch calls to `CloudiyClient.submit` — full example in [`examples/agent_tool.py`](examples/agent_tool.py).

### Payment model (x402 + Solana escrow)

Compute is priced **in USDC per job**. Without payment the node answers `402 Payment Required` with the quote (price, payout address, USDC mint, escrow program `9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN` on devnet). Settle via the escrow (`create_job` → provider paid on `release`, 4% protocol fee) and retry with the payment payload.
