# cloudify-sdk (Python)

Run GPU jobs on the [Cloudify](https://github.com/w3-surfer/gpuasas) network from Python — zero dependencies, built for apps and **AI agents**.

```bash
pip install ./sdk/python          # (from the repo; PyPI release planned)
```

```python
from cloudify_sdk import CloudifyClient, PaymentRequired

client = CloudifyClient("127.0.0.1:8080", token="<access-code>")

print(client.info())              # GPU model, VRAM, price in USDC, escrow program

try:
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6")
except PaymentRequired as quote:  # x402: the node quoted its USDC price
    result = client.submit(kernel="vector_add", data="1,2,3;4,5,6",
                           payment=quote.demo_payment())

print(result.output_text)         # "5,7,9"
print(result.payment_receipt)     # x402 settlement receipt
```

### For AI agents

`as_tool_schema()` emits an OpenAI/Anthropic-style function-tool definition; wire it to your agent and dispatch calls to `CloudifyClient.submit` — full example in [`examples/agent_tool.py`](examples/agent_tool.py).

### Payment model (x402 + Solana escrow)

Compute is priced **in USDC per job**. Without payment the node answers `402 Payment Required` with the quote (price, payout address, USDC mint, escrow program `9zMBC7JDA8SJ2mk3ATYqRuJvn14MQyZVg9q3XPnzc1TN` on devnet). Settle via the escrow (`create_job` → provider paid on `release`, 4% protocol fee) and retry with the payment payload.
