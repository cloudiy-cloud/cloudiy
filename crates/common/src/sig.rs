//! Result signing: the provider signs `(job_id, sha256(output))` with its
//! node key (the same ed25519 key behind its iroh EndpointId), giving the
//! consumer an offline-verifiable proof of *which node* produced *which
//! output* for *which job* — the artifact the on-chain escrow will require
//! to release payment.
//!
//! The message is domain-separated so these signatures can never be confused
//! with iroh TLS handshake signatures or future Cloudiy message types.

use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"cloudiy/result/v1";

fn signing_payload(job_id: &str, output: &[u8]) -> Vec<u8> {
    let output_hash = Sha256::digest(output);
    let mut msg = Vec::with_capacity(DOMAIN.len() + 1 + job_id.len() + 1 + 32);
    msg.extend_from_slice(DOMAIN);
    msg.push(0);
    msg.extend_from_slice(job_id.as_bytes());
    msg.push(0);
    msg.extend_from_slice(&output_hash);
    msg
}

/// Sign a job result with the node key. Returns the hex-encoded signature.
pub fn sign_result(secret: &iroh::SecretKey, job_id: &str, output: &[u8]) -> String {
    hex::encode(secret.sign(&signing_payload(job_id, output)).to_bytes())
}

/// Verify a result signature against the signer's EndpointId.
pub fn verify_result(
    signer: &iroh::EndpointId,
    job_id: &str,
    output: &[u8],
    signature_hex: &str,
) -> bool {
    let Ok(bytes) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(sig) = iroh::Signature::try_from(bytes.as_slice()) else {
        return false;
    };
    signer
        .verify(&signing_payload(job_id, output), &sig)
        .is_ok()
}

/// Random per-session access code (128 bits, base58) for providers started
/// without an explicit `--token`.
pub fn generate_access_code() -> String {
    let bytes: [u8; 16] = rand::random();
    bs58::encode(bytes).into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let bytes: [u8; 32] = rand::random();
        let secret = iroh::SecretKey::from_bytes(&bytes);
        let id = secret.public();

        let sig = sign_result(&secret, "job-1", b"output");
        assert!(verify_result(&id, "job-1", b"output", &sig));

        // Any mutation must invalidate the signature.
        assert!(!verify_result(&id, "job-2", b"output", &sig));
        assert!(!verify_result(&id, "job-1", b"tampered", &sig));
        assert!(!verify_result(&id, "job-1", b"output", "deadbeef"));

        // A different node cannot claim the result.
        let other: [u8; 32] = rand::random();
        let other_id = iroh::SecretKey::from_bytes(&other).public();
        assert!(!verify_result(&other_id, "job-1", b"output", &sig));
    }
}
