# cloudiy (Go)

Run GPU jobs on the [Cloudiy](https://github.com/w3-surfer/cloudiy) network from
Go — **zero third-party dependencies** (stdlib only), built for apps and **AI
agents**. Result signatures are verified with the standard library's
`crypto/ed25519`.

## Verify or reject — the whole point

Buying compute from a stranger is only safe if you can prove *which node*
produced *which output* for *which input*. Every result is ed25519-signed, and
this SDK checks it **before returning** — so an agent can't act on forged
compute:

```go
res, err := client.Submit(opts)   // err is *SignatureError if unsigned/tampered
_ = res.SignatureVerified         // true — proof, not trust
_ = res.SignedBy                  // which node actually computed it
```

That's on by default. [Details below.](#result-verification-on-by-default)

```bash
go get github.com/w3-surfer/cloudiy/sdk/go
```

```go
package main

import (
	"errors"
	"fmt"

	cloudiy "github.com/w3-surfer/cloudiy/sdk/go"
)

func main() {
	client := cloudiy.NewClient("127.0.0.1:8080")

	info, _ := client.Info() // GPU model, VRAM, price in USDC, escrow program
	fmt.Println(info["price_usdc"])

	opts := cloudiy.SubmitOptions{Kernel: "vector_add", Data: []byte("1,2,3;4,5,6")}
	res, err := client.Submit(opts)

	var pr *cloudiy.PaymentRequiredError
	if errors.As(err, &pr) { // x402: the node quoted its USDC price
		opts.Payment = pr.DemoPayment() // settle via escrow, then retry
		res, err = client.Submit(opts)
	}
	if err != nil {
		panic(err)
	}
	fmt.Println(res.OutputText(), res.SignatureVerified) // "5,7,9" true
}
```

## Result verification (on by default)

The provider signs `(job_id, sha256(input), sha256(output))` with its node key
(domain `cloudiy/result/v2` — the signature binds the output to the exact input
submitted). `Submit` **verifies that ed25519 signature by default** and returns
a `*SignatureError` if it is missing or invalid — an agent never acts on
unverified output. Set `SubmitOptions.NoVerify` for a trusted-local/demo node,
and `SubmitOptions.ExpectPubkey` to pin the provider's hex identity.

Verification is exposed directly too:

```go
ok := cloudiy.VerifyResult(signedBy, jobID, input, output, signatureHex)
```

## Reliability

Idempotent reads (`Info`, `Health`, `Status`) retry transient failures
(connection error, timeout, HTTP 5xx) with exponential backoff — tune with
`client.Retries`. `Submit` is **never** auto-retried (a paid job must not be
resent and double-charged); a connection failure returns a `*CloudiyError`.

## For AI agents

`cloudiy.AsToolSchema(node)` returns an OpenAI/Anthropic-style function-tool
definition; wire it to your function-calling LLM and dispatch calls to
`Client.Submit`.

## Tests

```bash
cd sdk/go && go test ./...          # offline signature vectors (vs the Rust signer)
go run ./e2e_http.go 127.0.0.1:8080 # HTTP e2e against a live `cloudiy share`
```

Apache-2.0 · part of the [Cloudiy SDKs](https://github.com/w3-surfer/cloudiy/tree/main/sdk).
