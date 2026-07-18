//go:build ignore

// HTTP end-to-end for the Go SDK against a live `cloudiy share` node.
//
// Unlike cloudiy_test.go (offline vectors), this drives the real wire: Info(),
// the x402 402->quote path, and — on a GPU node — a paid submit whose signed
// result is verified end-to-end. On a CPU-only node (e.g. CI) the kernel path
// has no GPU, so the paid submit returns an honest "no GPU" error; this asserts
// that graceful outcome instead of a signed result.
//
// Run: go run ./sdk/go/e2e_http.go <node-addr>
package main

import (
	"errors"
	"fmt"
	"os"
	"strings"

	cloudiy "github.com/w3-surfer/cloudiy/sdk/go"
)

const (
	kernel = "vector_add"
	expect = "11,22,33"
)

var data = []byte("1,2,3;10,20,30")

func main() {
	node := "127.0.0.1:8080"
	if len(os.Args) > 1 {
		node = os.Args[1]
	}
	if err := run(node); err != nil {
		fmt.Fprintf(os.Stderr, "FAIL %v\n", err)
		os.Exit(1)
	}
}

func run(node string) error {
	client := cloudiy.NewClient(node)

	// 1) Info() over HTTP.
	info, err := client.Info()
	if err != nil {
		return err
	}
	nodeID, _ := info["endpoint_id"].(string)
	if nodeID == "" {
		return fmt.Errorf("info() missing endpoint_id: %v", info)
	}
	fmt.Printf("ok  Info() -> node %s… price=%v USDC\n", nodeID[:16], info["price_usdc"])

	// 2) x402: no payment, no token -> quoted, not served.
	_, err = client.Submit(cloudiy.SubmitOptions{Kernel: kernel, Data: data})
	var pr *cloudiy.PaymentRequiredError
	if !errors.As(err, &pr) {
		return fmt.Errorf("submit without payment should return PaymentRequiredError, got %v", err)
	}
	if pr.PayTo == "" {
		return fmt.Errorf("PaymentRequiredError missing PayTo")
	}
	fmt.Printf("ok  402 quote -> %g USDC to %s…\n", pr.PriceUSDC, pr.PayTo[:16])

	// 3) Paid submit (demo x402). GPU node -> verified signed result; CPU-only
	//    node -> honest "no GPU" error.
	res, err := client.Submit(cloudiy.SubmitOptions{Kernel: kernel, Data: data, Payment: pr.DemoPayment()})
	if err != nil {
		var ce *cloudiy.CloudiyError
		if errors.As(err, &ce) && strings.Contains(ce.Message, "no GPU") {
			fmt.Printf("skip signed submit — CPU-only node (no GPU): %s\n", ce.Message)
			fmt.Println("all Go SDK HTTP e2e checks passed (CPU-only)")
			return nil
		}
		return err
	}
	if !res.SignatureVerified {
		return fmt.Errorf("result was not signature-verified")
	}
	if res.SignedBy != nodeID {
		return fmt.Errorf("signedBy %s != %s", res.SignedBy, nodeID)
	}
	if strings.TrimSpace(res.OutputText()) != expect {
		return fmt.Errorf("output %q != %q", res.OutputText(), expect)
	}
	fmt.Printf("ok  paid submit -> %q, signature verified\n", strings.TrimSpace(res.OutputText()))

	// 4) Wrong provider pin is refused even for good output.
	_, err = client.Submit(cloudiy.SubmitOptions{
		Kernel: kernel, Data: data, Payment: pr.DemoPayment(),
		ExpectPubkey: "00" + nodeID[2:],
	})
	var se *cloudiy.SignatureError
	if !errors.As(err, &se) && !errors.As(err, &pr) {
		return fmt.Errorf("wrong ExpectPubkey should be refused, got %v", err)
	}
	fmt.Println("ok  wrong ExpectPubkey refused")

	fmt.Println("all Go SDK HTTP e2e checks passed (GPU)")
	return nil
}
