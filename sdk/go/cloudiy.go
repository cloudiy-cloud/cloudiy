// Package cloudiy is a thin client for running GPU jobs on the Cloudiy network
// (USDC on Solana, x402). Zero third-party dependencies — stdlib only — built
// for apps and AI agents.
//
//	client := cloudiy.NewClient("127.0.0.1:8080")
//	info, _ := client.Info()
//	fmt.Println(info["price_usdc"])
//
//	res, err := client.Submit(cloudiy.SubmitOptions{Kernel: "vector_add", Data: []byte("1,2,3;4,5,6")})
//	var pr *cloudiy.PaymentRequiredError
//	if errors.As(err, &pr) { // x402: the node quoted its USDC price
//	    res, err = client.Submit(cloudiy.SubmitOptions{
//	        Kernel: "vector_add", Data: []byte("1,2,3;4,5,6"), Payment: pr.DemoPayment(),
//	    })
//	}
//	fmt.Println(string(res.Output), res.SignatureVerified)
//
// The provider's ed25519 result signature is verified by default: a missing or
// invalid signature returns a *SignatureError so an agent never trusts unverified
// output. Verification binds the output to the exact input submitted (domain
// cloudiy/result/v2), mirroring the Rust/Python/JS SDKs.
package cloudiy

import (
	"bytes"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// Version of the Go SDK.
const Version = "0.3.0"

const resultDomain = "cloudiy/result/v2"

// -- errors -----------------------------------------------------------------

// CloudiyError is a transport/protocol failure talking to the node (unreachable
// node, HTTP error, or a node-reported job error).
type CloudiyError struct{ Message string }

func (e *CloudiyError) Error() string { return e.Message }

// SignatureError means the result's provider signature was missing, invalid, or
// not from the expected node — the output must not be trusted.
type SignatureError struct{ Message string }

func (e *SignatureError) Error() string { return e.Message }

// PaymentRequiredError is the x402 quote returned when the node wants USDC
// before executing. Fields mirror the accepts[0] entry of the x402 requirements.
type PaymentRequiredError struct {
	Raw            map[string]any
	PriceMicroUSDC int64
	PriceUSDC      float64
	PayTo          string
	Asset          string
	Network        string
	EscrowProgram  string
}

func (e *PaymentRequiredError) Error() string {
	return fmt.Sprintf("payment required: %g USDC to %s (escrow %s)",
		e.PriceUSDC, e.PayTo, e.EscrowProgram)
}

// DemoPayment returns a base64 x402 payload for flow demos — real settlement
// uses the Cloudiy escrow program on Solana devnet.
func (e *PaymentRequiredError) DemoPayment() string {
	network := e.Network
	if network == "" {
		network = "solana-devnet"
	}
	payload, _ := json.Marshal(map[string]any{
		"x402Version": 1,
		"scheme":      "exact",
		"network":     network,
		"payload":     map[string]any{"note": "demo payment — settlement via Cloudiy escrow (devnet)"},
	})
	return base64.StdEncoding.EncodeToString(payload)
}

func newPaymentRequired(raw map[string]any) *PaymentRequiredError {
	var offer map[string]any
	if accepts, ok := raw["accepts"].([]any); ok && len(accepts) > 0 {
		offer, _ = accepts[0].(map[string]any)
	}
	if offer == nil {
		offer = map[string]any{}
	}
	priceMicro := int64(asFloat(offer["maxAmountRequired"]))
	escrow := ""
	if extra, ok := offer["extra"].(map[string]any); ok {
		escrow, _ = extra["escrowProgram"].(string)
	}
	return &PaymentRequiredError{
		Raw:            raw,
		PriceMicroUSDC: priceMicro,
		PriceUSDC:      float64(priceMicro) / 1_000_000,
		PayTo:          asString(offer["payTo"]),
		Asset:          asString(offer["asset"]),
		Network:        asString(offer["network"]),
		EscrowProgram:  escrow,
	}
}

// -- signature verification -------------------------------------------------

// resultSigningPayload builds the exact bytes the provider signs:
// domain ‖ 0 ‖ job_id ‖ 0 ‖ sha256(input) ‖ 0 ‖ sha256(output).
func resultSigningPayload(jobID string, input, output []byte) []byte {
	inH := sha256.Sum256(input)
	outH := sha256.Sum256(output)
	var b bytes.Buffer
	b.WriteString(resultDomain)
	b.WriteByte(0)
	b.WriteString(jobID)
	b.WriteByte(0)
	b.Write(inH[:])
	b.WriteByte(0)
	b.Write(outH[:])
	return b.Bytes()
}

// VerifyResult reports whether signatureHex is a valid provider signature over
// (jobID, sha256(input), sha256(output)) by the node whose hex EndpointId is
// signedBy. input must be the exact bytes submitted, so a provider that ran a
// different prompt cannot produce a verifying signature. Same construction as
// the Rust cloudiy_common::sig (v2). Uses the stdlib crypto/ed25519.
func VerifyResult(signedBy, jobID string, input, output []byte, signatureHex string) bool {
	pub, err := hex.DecodeString(signedBy)
	if err != nil || len(pub) != ed25519.PublicKeySize {
		return false
	}
	sig, err := hex.DecodeString(signatureHex)
	if err != nil || len(sig) != ed25519.SignatureSize {
		return false
	}
	return ed25519.Verify(pub, resultSigningPayload(jobID, input, output), sig)
}

// -- client -----------------------------------------------------------------

// Client is an HTTP client for a Cloudiy node (`cloudiy share` exposes the API).
// For the P2P transport (dial-by-NodeID, NAT traversal) use the Rust SDK; this
// client targets the node's HTTP endpoint — ideal for agents and backends.
type Client struct {
	base  string
	Token string
	// Retries is the number of extra attempts idempotent GETs (Info/Health/
	// Status) make on a transient failure (connection error, timeout, HTTP 5xx).
	// Submit is never auto-retried — a paid job must not be resent and
	// double-charged.
	Retries int
	http    *http.Client
}

// NewClient targets a node ("host:port" or a full URL). Timeout defaults to 90s
// (GPU jobs are bounded to 60s node-side); override via Client.SetTimeout.
func NewClient(node string) *Client {
	base := node
	if !strings.Contains(node, "://") {
		base = "http://" + node
	}
	return &Client{
		base:    base,
		Retries: 2,
		http:    &http.Client{Timeout: 90 * time.Second},
	}
}

// SetTimeout sets the per-request timeout.
func (c *Client) SetTimeout(d time.Duration) { c.http.Timeout = d }

// WithToken sets the auth token / access code and returns the client for chaining.
func (c *Client) WithToken(token string) *Client { c.Token = token; return c }

// get fetches path and decodes JSON, retrying transient failures (connection
// error, timeout, HTTP 5xx) with exponential backoff. Idempotent, so safe.
func (c *Client) get(path string) (map[string]any, error) {
	attempts := c.Retries + 1
	if attempts < 1 {
		attempts = 1
	}
	var lastErr error
	for i := 0; i < attempts; i++ {
		res, err := c.http.Get(c.base + path)
		if err != nil {
			lastErr = &CloudiyError{fmt.Sprintf("cannot reach node at %s (%s): %v", c.base, path, err)}
		} else {
			body, _ := io.ReadAll(res.Body)
			res.Body.Close()
			if res.StatusCode < 300 {
				var out map[string]any
				if err := json.Unmarshal(body, &out); err != nil {
					return nil, &CloudiyError{fmt.Sprintf("GET %s: bad JSON: %v", path, err)}
				}
				return out, nil
			}
			// 5xx is transient (retry); 4xx is the caller's fault (don't).
			if res.StatusCode < 500 {
				return nil, &CloudiyError{fmt.Sprintf("GET %s -> HTTP %d", path, res.StatusCode)}
			}
			lastErr = &CloudiyError{fmt.Sprintf("GET %s -> HTTP %d", path, res.StatusCode)}
		}
		if i < attempts-1 {
			time.Sleep(time.Duration(200*(1<<i)) * time.Millisecond) // 200ms, 400ms, …
		}
	}
	return nil, lastErr
}

// Health returns the node's liveness/uptime.
func (c *Client) Health() (map[string]any, error) { return c.get("/health") }

// Info returns node capabilities: GPU model, VRAM, price (USDC), escrow program.
func (c *Client) Info() (map[string]any, error) { return c.get("/info") }

// Status returns the status of a job by id.
func (c *Client) Status(jobID string) (map[string]any, error) { return c.get("/status/" + jobID) }

// JobResult is a completed job's output and its verification state.
type JobResult struct {
	JobID    string
	Output   []byte
	Status   string
	ProviderPubkey string
	PaymentReceipt map[string]any
	// SignatureVerified is true when the result carried a valid ed25519
	// signature from the provider node (and matched ExpectPubkey when pinned).
	SignatureVerified bool
	// Signature is the hex ed25519 result signature.
	Signature string
	// SignedBy is the hex node key (iroh EndpointId) that produced the signature.
	SignedBy string
}

// OutputText returns the output decoded as UTF-8 text.
func (r *JobResult) OutputText() string { return string(r.Output) }

// SubmitOptions configures a Submit call.
type SubmitOptions struct {
	Kernel string
	Data   []byte
	Params map[string]string
	// Token overrides the client's token for this call.
	Token string
	// Payment is a base64 x402 payment payload.
	Payment string
	// Verify verifies the provider's result signature. Defaults to true; set
	// NoVerify to accept unsigned results from a trusted-local/demo node.
	NoVerify bool
	// ExpectPubkey pins the provider's hex node identity.
	ExpectPubkey string
}

// Submit runs a kernel on the node's GPU. It returns a *PaymentRequiredError
// (via errors.As) with the x402 quote when the node wants USDC and no valid
// Payment/Token was given. The provider's ed25519 result signature is verified
// by default: a missing or invalid signature returns a *SignatureError. Submit
// is not auto-retried (a paid job must not be resent and double-charged).
func (c *Client) Submit(opts SubmitOptions) (*JobResult, error) {
	params := opts.Params
	if params == nil {
		params = map[string]string{}
	}
	token := opts.Token
	if token == "" {
		token = c.Token
	}
	jobID, err := uuidV4()
	if err != nil {
		return nil, &CloudiyError{"failed to generate job id: " + err.Error()}
	}

	reqBody, _ := json.Marshal(map[string]any{
		"job_id":         jobID,
		"kernel":         opts.Kernel,
		"input_data":     bytesToInts(opts.Data),
		"params":         params,
		"auth_token":     token,
		"consumer_pubkey": nil,
		"payment":        nilIfEmpty(opts.Payment),
	})

	req, err := http.NewRequest("POST", c.base+"/submit", bytes.NewReader(reqBody))
	if err != nil {
		return nil, &CloudiyError{err.Error()}
	}
	req.Header.Set("Content-Type", "application/json")
	if opts.Payment != "" {
		req.Header.Set("X-PAYMENT", opts.Payment)
	}

	res, err := c.http.Do(req)
	if err != nil {
		// Not auto-retried; surface the connection failure clearly.
		return nil, &CloudiyError{fmt.Sprintf("cannot reach node at %s (/submit): %v", c.base, err)}
	}
	body, _ := io.ReadAll(res.Body)
	res.Body.Close()

	if res.StatusCode == http.StatusPaymentRequired {
		var raw map[string]any
		if err := json.Unmarshal(body, &raw); err != nil {
			return nil, &CloudiyError{"402 with unparseable requirements: " + err.Error()}
		}
		return nil, newPaymentRequired(raw)
	}
	if res.StatusCode >= 300 {
		return nil, &CloudiyError{fmt.Sprintf("HTTP %d: %s", res.StatusCode, truncate(body, 300))}
	}

	var raw struct {
		JobID          string `json:"job_id"`
		OutputData     []int  `json:"output_data"`
		Status         string `json:"status"`
		ErrorMessage   string `json:"error_message"`
		ProviderPubkey string `json:"provider_pubkey"`
		Signature      string `json:"signature"`
		SignedBy       string `json:"signed_by"`
	}
	if err := json.Unmarshal(body, &raw); err != nil {
		return nil, &CloudiyError{"bad job response JSON: " + err.Error()}
	}
	if raw.Status == "error" {
		msg := raw.ErrorMessage
		if msg == "" {
			msg = "unknown error"
		}
		return nil, &CloudiyError{msg}
	}

	output := intsToBytes(raw.OutputData)

	var receipt map[string]any
	if h := res.Header.Get("x-payment-response"); h != "" {
		if dec, err := base64.StdEncoding.DecodeString(h); err == nil {
			_ = json.Unmarshal(dec, &receipt)
		}
	}

	// Verify the provider signature over (job_id, sha256(input), sha256(output))
	// — binds the output to THIS input. With a pin, the signer must also BE that
	// node; without it, a valid signature proves integrity but not intended node.
	verified := raw.Signature != "" && raw.SignedBy != "" &&
		(opts.ExpectPubkey == "" || raw.SignedBy == opts.ExpectPubkey) &&
		VerifyResult(raw.SignedBy, raw.JobID, opts.Data, output, raw.Signature)

	if !opts.NoVerify && !verified {
		switch {
		case raw.Signature == "" || raw.SignedBy == "":
			return nil, &SignatureError{"result was not signed by the provider — refusing to trust output (set NoVerify to accept unsigned)"}
		case opts.ExpectPubkey != "" && raw.SignedBy != opts.ExpectPubkey:
			return nil, &SignatureError{fmt.Sprintf("result signed by %s but expected %s — refusing to trust output", raw.SignedBy, opts.ExpectPubkey)}
		default:
			return nil, &SignatureError{"invalid provider signature — result may be tampered; refusing to trust output"}
		}
	}

	return &JobResult{
		JobID:             raw.JobID,
		Output:            output,
		Status:            raw.Status,
		ProviderPubkey:    raw.ProviderPubkey,
		PaymentReceipt:    receipt,
		SignatureVerified: verified,
		Signature:         raw.Signature,
		SignedBy:          raw.SignedBy,
	}, nil
}

// ToolSchema is an OpenAI/Anthropic-style function-tool definition.
type ToolSchema struct {
	Name        string         `json:"name"`
	Description string         `json:"description"`
	InputSchema map[string]any `json:"input_schema"`
}

// AsToolSchema returns a function-calling tool schema so LLM agents can invoke
// Cloudiy GPU compute as a tool. Pair with Client.Submit.
func AsToolSchema(node string) ToolSchema {
	if node == "" {
		node = "127.0.0.1:8080"
	}
	return ToolSchema{
		Name: "cloudiy_gpu_run",
		Description: "Run a compute kernel on a decentralized GPU (Cloudiy network, node " + node +
			"). Payment in USDC on Solana via x402. Kernels: vector_add ('a1,a2,...;b1,b2,...'), " +
			"matrix_mul ('m,k,n;A row-major;B row-major').",
		InputSchema: map[string]any{
			"type": "object",
			"properties": map[string]any{
				"kernel": map[string]any{"type": "string", "enum": []string{"vector_add", "matrix_mul"}},
				"data":   map[string]any{"type": "string", "description": "Kernel input in the documented format"},
			},
			"required": []string{"kernel", "data"},
		},
	}
}

// -- helpers ----------------------------------------------------------------

// uuidV4 returns a random RFC 4122 v4 UUID string using crypto/rand (no deps).
func uuidV4() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	b[6] = (b[6] & 0x0f) | 0x40 // version 4
	b[8] = (b[8] & 0x3f) | 0x80 // variant 10
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16]), nil
}

// The node's JobRequest.input_data / JobResponse.output_data are JSON arrays of
// byte values (0-255), so bytes cross the wire as []int.
func bytesToInts(b []byte) []int {
	out := make([]int, len(b))
	for i, v := range b {
		out[i] = int(v)
	}
	return out
}

func intsToBytes(ns []int) []byte {
	out := make([]byte, len(ns))
	for i, n := range ns {
		out[i] = byte(n)
	}
	return out
}

func asFloat(v any) float64 {
	switch n := v.(type) {
	case float64:
		return n
	case json.Number:
		f, _ := n.Float64()
		return f
	case string:
		var f float64
		fmt.Sscanf(n, "%g", &f)
		return f
	default:
		return 0
	}
}

func asString(v any) string {
	s, _ := v.(string)
	return s
}

func nilIfEmpty(s string) any {
	if s == "" {
		return nil
	}
	return s
}

func truncate(b []byte, n int) string {
	if len(b) > n {
		return string(b[:n])
	}
	return string(b)
}
