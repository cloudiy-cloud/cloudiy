"""Quickstart: an AI agent rents GPU compute on Cloudiy.

The whole arc in one file — discover a provider, let an LLM decide to buy
compute, settle the x402 quote, and refuse the result unless the provider
signed it:

    1. discover  — inspect the provider: GPU, price, escrow program
    2. decide    — Claude gets as_tool_schema() and calls the tool itself
    3. pay       — the node quotes in USDC (402); the agent settles and retries
    4. verify    — the ed25519 result signature is checked before the agent
                   is allowed to act on the output (this is the point)

Runs with or without an Anthropic API key:

    export ANTHROPIC_API_KEY=sk-ant-...   # real function-calling loop
    python3 agent_rents_compute.py        # without the key: scripted mock

The mock drives the identical Cloudiy path — only the "which tool should I
call" decision is faked — so the payment and signature flow is exercised
either way. `anthropic` is needed only for the real loop; the SDK itself
stays zero-dependency.

Prereq: a node on 127.0.0.1:8080 (`cloudiy share`), or pass one as argv[1].
"""

import json
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from cloudiy_sdk import (  # noqa: E402
    CloudiyClient,
    PaymentRequired,
    SignatureError,
    as_tool_schema,
)

NODE = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:8080"
MODEL = "claude-sonnet-5"

client = CloudiyClient(NODE)


# --- 1. discover ----------------------------------------------------------
# The HTTP SDK talks to one node you already know. To find nodes across the
# network, use the CLI (`cloudiy providers --via <directory>`) or the MCP tool
# `cloudiy_list_providers` — discovery rides the P2P/iroh transport, not HTTP.
def discover() -> dict:
    info = client.info()
    print(
        f"provider {info['endpoint_id'][:16]}… — {info['gpu_model']}, "
        f"{info['vram_mb']} MB VRAM, {info['price_usdc']} USDC/job\n"
        f"  escrow {info['escrow_program'][:16]}… on {info['network']}"
    )
    return info


# --- 2+3. the tool the agent calls ---------------------------------------
def cloudiy_gpu_run(kernel: str, data: str) -> str:
    """Run a kernel on a decentralized GPU, paying the x402 quote if asked.

    Returns text for the LLM. The signature check is NOT optional: unsigned or
    tampered output raises before the agent ever sees it.
    """
    try:
        result = client.submit(kernel=kernel, data=data)
    except PaymentRequired as quote:
        # The node wants USDC. A real agent settles the Cloudiy escrow on
        # Solana here; demo_payment() stands in for that on devnet.
        print(f"  402 → paying {quote.price_usdc} USDC to {quote.pay_to[:16]}…")
        result = client.submit(kernel=kernel, data=data, payment=quote.demo_payment())

    # --- 4. verify (done by default inside submit) ---
    # Reaching this line means the ed25519 signature over
    # (job_id, sha256(input), sha256(output)) verified against the node's key.
    print(f"  signature verified — signed by {result.signed_by[:16]}…")
    return result.output_text


TOOLS = [as_tool_schema(NODE)]


def dispatch(name: str, args: dict) -> str:
    if name == "cloudiy_gpu_run":
        return cloudiy_gpu_run(**args)
    raise ValueError(f"unknown tool: {name}")


# --- the agent loop -------------------------------------------------------
TASK = (
    "Add the vectors [1,2,3] and [10,20,30] using decentralized GPU compute, "
    "then tell me the result."
)


def run_with_claude() -> None:
    """Real Anthropic function-calling loop."""
    import anthropic

    llm = anthropic.Anthropic()
    messages = [{"role": "user", "content": TASK}]

    response = llm.messages.create(
        model=MODEL, max_tokens=16000, tools=TOOLS, messages=messages
    )

    while response.stop_reason == "tool_use":
        messages.append({"role": "assistant", "content": response.content})
        results = []
        for block in response.content:
            if block.type == "tool_use":
                print(f"agent calls {block.name}({json.dumps(block.input)})")
                results.append(
                    {
                        "type": "tool_result",
                        "tool_use_id": block.id,
                        "content": dispatch(block.name, block.input),
                    }
                )
        messages.append({"role": "user", "content": results})
        response = llm.messages.create(
            model=MODEL, max_tokens=16000, tools=TOOLS, messages=messages
        )

    for block in response.content:
        if block.type == "text":
            print(f"\nagent: {block.text}")


def run_mock() -> None:
    """Same flow, scripted tool call — no API key needed."""
    print(f"(mock mode — no ANTHROPIC_API_KEY; the tool call below is scripted)\n")
    print(f"tool schema handed to the LLM: {TOOLS[0]['name']} "
          f"{list(TOOLS[0]['input_schema']['properties'])}")
    args = {"kernel": "vector_add", "data": "1,2,3;10,20,30"}
    print(f"agent calls cloudiy_gpu_run({json.dumps(args)})")
    output = dispatch("cloudiy_gpu_run", args)
    print(f"\nagent: The result is {output.strip()}.")


if __name__ == "__main__":
    discover()
    print()
    try:
        if os.environ.get("ANTHROPIC_API_KEY"):
            run_with_claude()
        else:
            run_mock()
    except SignatureError as e:
        # The whole point: an agent must not act on unverified compute.
        print(f"\nREFUSED — {e}", file=sys.stderr)
        sys.exit(1)
