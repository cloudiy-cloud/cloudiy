// Result-signature verification tests for the Go SDK.
//
// The known-good vector is generated from the Rust signer
// (crates/common/src/sig.rs via `cargo run -p cloudiy-common --example
// gen_vectors`), so this asserts cross-language agreement: Go's stdlib
// crypto/ed25519 accepts exactly what the provider produces and rejects any
// tampering. Run: `go test ./sdk/go/...` (or `cd sdk/go && go test`).
package cloudiy

import "testing"

// Vector from the Rust source of truth (seed = [7u8; 32]), v2 (input-bound).
const (
	pub = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c"
	sig = "b6998b170df90c982e1e09655cdd41ab63fa709300aa252e947f920fffaadbfc" +
		"7cd022d932c01f7219e0b16f66715c030dbc16eaa6abfc14baa10d14b87c0407"
	job = "job-abc-123"
)

// "the consumer's exact prompt" / "hello cloudiy result"
var (
	inp = []byte("the consumer's exact prompt")
	out = []byte("hello cloudiy result")
)

func TestValidSignature(t *testing.T) {
	if !VerifyResult(pub, job, inp, out, sig) {
		t.Fatal("valid signature was rejected")
	}
}

func TestRejectsWrongJobID(t *testing.T) {
	if VerifyResult(pub, "job-x", inp, out, sig) {
		t.Fatal("accepted a wrong job id")
	}
}

func TestRejectsTamperedInput(t *testing.T) {
	if VerifyResult(pub, job, append(append([]byte{}, inp...), '!'), out, sig) {
		t.Fatal("accepted a tampered input")
	}
}

func TestRejectsTamperedOutput(t *testing.T) {
	if VerifyResult(pub, job, inp, append(append([]byte{}, out...), '!'), sig) {
		t.Fatal("accepted a tampered output")
	}
}

func TestRejectsWrongPubkey(t *testing.T) {
	if VerifyResult("00"+pub[2:], job, inp, out, sig) {
		t.Fatal("accepted a wrong pubkey")
	}
}

func TestRejectsMalformedHex(t *testing.T) {
	if VerifyResult("zz", job, inp, out, sig) {
		t.Fatal("accepted a malformed pubkey hex")
	}
	if VerifyResult(pub, job, inp, out, "nothex") {
		t.Fatal("accepted a malformed signature hex")
	}
}

func TestDemoPaymentRoundtrips(t *testing.T) {
	pr := &PaymentRequiredError{Network: "solana-devnet"}
	if pr.DemoPayment() == "" {
		t.Fatal("demo payment payload was empty")
	}
}

func TestUUIDV4(t *testing.T) {
	id, err := uuidV4()
	if err != nil {
		t.Fatal(err)
	}
	if len(id) != 36 || id[14] != '4' {
		t.Fatalf("not a v4 uuid: %q", id)
	}
}
