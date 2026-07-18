# Python examples

```bash
cloudiy share                                  # terminal 1: a node on :8080
python3 agent_rents_compute.py                 # terminal 2: the quickstart
```

`agent_rents_compute.py` — an AI agent rents GPU compute: discover a provider →
Claude calls the tool itself → pay the x402 quote in USDC → **verify the
provider's signature before trusting the output**. Runs without an API key
(scripted mock); `export ANTHROPIC_API_KEY=…` and `pip install anthropic` for
the real `claude-sonnet-5` function-calling loop. `agent_tool.py` is the
minimal tool-schema snippet.
