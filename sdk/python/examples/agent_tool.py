"""Cloudify as an AI-agent tool.

Shows the two integration styles:
1. plain function an agent framework can call (LangChain tool, MCP handler…)
2. function-calling schema + dispatcher for OpenAI/Anthropic-style tool use

Run a local node first:  cloudify share --token demo
Then:                    python examples/agent_tool.py
"""

import json

from cloudify_sdk import CloudifyClient, PaymentRequired, as_tool_schema

NODE = "127.0.0.1:8080"
client = CloudifyClient(NODE, token="demo")


# 1. Plain callable — drop into LangChain's @tool, CrewAI, MCP, etc.
def cloudify_gpu_run(kernel: str, data: str) -> str:
    """Run `kernel` on a decentralized GPU and return the text output.
    Handles the x402 payment flow automatically (demo settlement)."""
    try:
        result = client.submit(kernel=kernel, data=data)
    except PaymentRequired as quote:
        # An autonomous agent would settle the USDC escrow here.
        result = client.submit(kernel=kernel, data=data, payment=quote.demo_payment())
    return result.output_text


# 2. Tool schema for function-calling LLMs
TOOLS = [as_tool_schema(NODE)]


def dispatch_tool_call(name: str, arguments: dict) -> str:
    if name == "cloudify_gpu_run":
        return cloudify_gpu_run(**arguments)
    raise ValueError(f"unknown tool: {name}")


if __name__ == "__main__":
    print("Tool schema handed to the LLM:")
    print(json.dumps(TOOLS, indent=2)[:400], "…\n")

    print("Agent calls cloudify_gpu_run(vector_add, '1,2,3;4,5,6'):")
    print(" →", cloudify_gpu_run("vector_add", "1,2,3;4,5,6"))

    print("Agent calls cloudify_gpu_run(matrix_mul, '2,2,2;1,2,3,4;5,6,7,8'):")
    print(" →", cloudify_gpu_run("matrix_mul", "2,2,2;1,2,3,4;5,6,7,8"))
