// Quickstart: an AI agent rents GPU compute on Cloudiy.
//
// The whole arc in one file — discover a provider, let an LLM decide to buy
// compute, settle the x402 quote, and refuse the result unless the provider
// signed it:
//
//  1. discover — inspect the provider: GPU, price, escrow program
//  2. decide   — Claude gets AsToolSchema() and calls the tool itself
//  3. pay      — the node quotes in USDC (402); the agent settles and retries
//  4. verify   — the ed25519 result signature is checked before the agent is
//     allowed to act on the output (this is the point)
//
// Runs with or without an Anthropic API key:
//
//	export ANTHROPIC_API_KEY=sk-ant-...   # real function-calling loop
//	go run . 127.0.0.1:8080               # without the key: scripted mock
//
// The mock drives the identical Cloudiy path — only the "which tool should I
// call" decision is faked. This example lives in its own module so the
// Anthropic SDK never becomes a dependency of the Cloudiy SDK itself.
//
// Prereq: a node on 127.0.0.1:8080 (`cloudiy share`), or pass one as argv[1].
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"

	"github.com/anthropics/anthropic-sdk-go"
	cloudiy "github.com/cloudiy-cloud/cloudiy/sdk/go"
)

const model = "claude-sonnet-5"

const task = "Add the vectors [1,2,3] and [10,20,30] using decentralized GPU " +
	"compute, then tell me the result."

var client *cloudiy.Client

// --- 1. discover ----------------------------------------------------------
// The HTTP SDK talks to one node you already know. To find nodes across the
// network, use the CLI (`cloudiy providers --via <directory>`) or the MCP tool
// `cloudiy_list_providers` — discovery rides the P2P/iroh transport, not HTTP.
func discover() error {
	info, err := client.Info()
	if err != nil {
		return err
	}
	id, _ := info["endpoint_id"].(string)
	escrow, _ := info["escrow_program"].(string)
	fmt.Printf("provider %s… — %v, %v MB VRAM, %v USDC/job\n  escrow %s… on %v\n",
		id[:16], info["gpu_model"], info["vram_mb"], info["price_usdc"],
		escrow[:16], info["network"])
	return nil
}

// --- 2+3. the tool the agent calls ---------------------------------------
// cloudiyGPURun runs a kernel on a decentralized GPU, paying the x402 quote if
// asked. The signature check is NOT optional: unsigned or tampered output
// errors out before the agent ever sees it.
func cloudiyGPURun(kernel, data string) (string, error) {
	opts := cloudiy.SubmitOptions{Kernel: kernel, Data: []byte(data)}
	res, err := client.Submit(opts)

	var quote *cloudiy.PaymentRequiredError
	if errors.As(err, &quote) {
		// The node wants USDC. A real agent settles the Cloudiy escrow on
		// Solana here; DemoPayment() stands in for that on devnet.
		fmt.Printf("  402 → paying %g USDC to %s…\n", quote.PriceUSDC, quote.PayTo[:16])
		opts.Payment = quote.DemoPayment()
		res, err = client.Submit(opts)
	}
	if err != nil {
		return "", err
	}

	// --- 4. verify (done by default inside Submit) ---
	// Reaching this line means the ed25519 signature over
	// (job_id, sha256(input), sha256(output)) verified against the node's key.
	fmt.Printf("  signature verified — signed by %s…\n", res.SignedBy[:16])
	return res.OutputText(), nil
}

func dispatch(name string, input map[string]any) (string, error) {
	if name != "cloudiy_gpu_run" {
		return "", fmt.Errorf("unknown tool: %s", name)
	}
	kernel, _ := input["kernel"].(string)
	data, _ := input["data"].(string)
	return cloudiyGPURun(kernel, data)
}

// --- the agent loop -------------------------------------------------------
func runWithClaude(node string) error {
	// AsToolSchema() already produces the Anthropic tool shape — feed it straight in.
	schema := cloudiy.AsToolSchema(node)
	props, _ := schema.InputSchema["properties"].(map[string]any)
	tool := anthropic.ToolParam{
		Name:        schema.Name,
		Description: anthropic.String(schema.Description),
		InputSchema: anthropic.ToolInputSchemaParam{Properties: props},
	}
	tools := []anthropic.ToolUnionParam{{OfTool: &tool}}

	ctx := context.Background()
	llm := anthropic.NewClient() // reads ANTHROPIC_API_KEY
	messages := []anthropic.MessageParam{
		anthropic.NewUserMessage(anthropic.NewTextBlock(task)),
	}

	for {
		resp, err := llm.Messages.New(ctx, anthropic.MessageNewParams{
			Model:     anthropic.Model(model),
			MaxTokens: 16000,
			Messages:  messages,
			Tools:     tools,
		})
		if err != nil {
			return err
		}
		messages = append(messages, resp.ToParam())

		var results []anthropic.ContentBlockParamUnion
		for _, block := range resp.Content {
			switch variant := block.AsAny().(type) {
			case anthropic.TextBlock:
				if resp.StopReason != anthropic.StopReasonToolUse {
					fmt.Printf("\nagent: %s\n", variant.Text)
				}
			case anthropic.ToolUseBlock:
				var in map[string]any
				if err := json.Unmarshal([]byte(variant.JSON.Input.Raw()), &in); err != nil {
					return err
				}
				fmt.Printf("agent calls %s(%s)\n", variant.Name, variant.JSON.Input.Raw())
				out, err := dispatch(variant.Name, in)
				if err != nil {
					return err
				}
				results = append(results, anthropic.NewToolResultBlock(block.ID, out, false))
			}
		}

		if resp.StopReason != anthropic.StopReasonToolUse {
			return nil
		}
		messages = append(messages, anthropic.NewUserMessage(results...))
	}
}

func runMock(node string) error {
	fmt.Println("(mock mode — no ANTHROPIC_API_KEY; the tool call below is scripted)")
	fmt.Println()
	schema := cloudiy.AsToolSchema(node)
	props, _ := schema.InputSchema["properties"].(map[string]any)
	keys := make([]string, 0, len(props))
	for k := range props {
		keys = append(keys, k)
	}
	fmt.Printf("tool schema handed to the LLM: %s [%s]\n", schema.Name, strings.Join(keys, ", "))

	input := map[string]any{"kernel": "vector_add", "data": "1,2,3;10,20,30"}
	raw, _ := json.Marshal(input)
	fmt.Printf("agent calls cloudiy_gpu_run(%s)\n", raw)
	out, err := dispatch("cloudiy_gpu_run", input)
	if err != nil {
		return err
	}
	fmt.Printf("\nagent: The result is %s.\n", strings.TrimSpace(out))
	return nil
}

func main() {
	node := "127.0.0.1:8080"
	if len(os.Args) > 1 {
		node = os.Args[1]
	}
	client = cloudiy.NewClient(node)

	if err := discover(); err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
	fmt.Println()

	var err error
	if os.Getenv("ANTHROPIC_API_KEY") != "" {
		err = runWithClaude(node)
	} else {
		err = runMock(node)
	}

	var sigErr *cloudiy.SignatureError
	if errors.As(err, &sigErr) {
		// The whole point: an agent must not act on unverified compute.
		fmt.Fprintf(os.Stderr, "\nREFUSED — %v\n", sigErr)
		os.Exit(1)
	}
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}
}
