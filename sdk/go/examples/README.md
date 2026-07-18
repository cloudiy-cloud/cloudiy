# Go examples

```bash
cloudiy share                                  # terminal 1: a node on :8080
cd sdk/go/examples && go run .                 # terminal 2: the quickstart
```

`agent_rents_compute.go` — an AI agent rents GPU compute: discover a provider →
Claude calls the tool itself → pay the x402 quote in USDC → **verify the
provider's signature before trusting the output**. Runs without an API key
(scripted mock); `export ANTHROPIC_API_KEY=…` for the real `claude-sonnet-5`
loop. Its own module on purpose — the Anthropic SDK is an example-only
dependency, so `sdk/go` itself stays zero-dependency.
