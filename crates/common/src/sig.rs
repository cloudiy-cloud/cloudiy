//! Result signing: the provider signs `(job_id, sha256(input), sha256(output))`
//! with its node key (the same ed25519 key behind its iroh EndpointId), giving
//! the consumer an offline-verifiable proof of *which node* produced *which
//! output* for *which input* under *which job* — the artifact the on-chain
//! escrow (and canary/delivery verification, RFC-0006) needs to release
//! payment and to bind the output to the consumer's exact input.
//!
//! The message is domain-separated (`v2` binds the input; `v1` did not) so
//! these signatures can never be confused with iroh TLS handshake signatures,
//! an older result format, or future Cloudiy message types.

use sha2::{Digest, Sha256};

const DOMAIN: &[u8] = b"cloudiy/result/v2";

fn signing_payload(job_id: &str, input: &[u8], output: &[u8]) -> Vec<u8> {
    let input_hash = Sha256::digest(input);
    let output_hash = Sha256::digest(output);
    let mut msg = Vec::with_capacity(DOMAIN.len() + 1 + job_id.len() + 1 + 32 + 1 + 32);
    msg.extend_from_slice(DOMAIN);
    msg.push(0);
    msg.extend_from_slice(job_id.as_bytes());
    msg.push(0);
    msg.extend_from_slice(&input_hash);
    msg.push(0);
    msg.extend_from_slice(&output_hash);
    msg
}

/// Sign a job result with the node key: binds `job_id`, the consumer's `input`
/// and the produced `output`. Returns the hex-encoded signature.
pub fn sign_result(secret: &iroh::SecretKey, job_id: &str, input: &[u8], output: &[u8]) -> String {
    hex::encode(secret.sign(&signing_payload(job_id, input, output)).to_bytes())
}

/// Verify a result signature against the signer's EndpointId. The `input` must
/// be the exact bytes the consumer submitted, so a provider that ran a
/// different prompt cannot produce a signature that verifies.
pub fn verify_result(
    signer: &iroh::EndpointId,
    job_id: &str,
    input: &[u8],
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
        .verify(&signing_payload(job_id, input, output), &sig)
        .is_ok()
}

/// Random per-session access code (128 bits, base58) for providers started
/// without an explicit `--token`.
pub fn generate_access_code() -> String {
    let bytes: [u8; 16] = rand::random();
    bs58::encode(bytes).into_string()
}

// --------------------------------------------------------------- announce

const ANNOUNCE_DOMAIN: &[u8] = b"cloudiy/announce/v1";

/// How long an announcement stays valid — heartbeats refresh well within it.
pub const ANNOUNCE_TTL_SECS: i64 = 180;

/// Tolerated forward clock skew between announcer and verifier.
const ANNOUNCE_MAX_SKEW_SECS: i64 = 30;

fn announce_signing_payload(issued_at: i64, payload: &str) -> Vec<u8> {
    let mut msg = Vec::with_capacity(ANNOUNCE_DOMAIN.len() + 1 + 8 + 1 + payload.len());
    msg.extend_from_slice(ANNOUNCE_DOMAIN);
    msg.push(0);
    msg.extend_from_slice(&issued_at.to_be_bytes());
    msg.push(0);
    msg.extend_from_slice(payload.as_bytes());
    msg
}

/// Sign a provider announcement with the node key.
pub fn sign_announcement(
    secret: &iroh::SecretKey,
    announcement: &cloudiy_protocol::ProviderAnnouncement,
    issued_at: i64,
) -> anyhow::Result<crate::SignedAnnouncement> {
    let payload = serde_json::to_string(announcement)?;
    let signature = secret.sign(&announce_signing_payload(issued_at, &payload));
    Ok(crate::SignedAnnouncement {
        payload,
        issued_at,
        signed_by: secret.public().to_string(),
        signature: hex::encode(signature.to_bytes()),
    })
}

/// Verify a signed announcement: freshness window, valid signature by
/// `signed_by`, and the announced identity actually being the signer (a
/// node can never announce resources on behalf of another node).
/// Returns the parsed announcement on success.
pub fn verify_announcement(
    sa: &crate::SignedAnnouncement,
    now: i64,
) -> Result<cloudiy_protocol::ProviderAnnouncement, String> {
    // `issued_at` comes straight off the wire and is checked BEFORE the
    // signature, so it must never trap or wrap: a peer sending `i64::MIN`
    // would otherwise overflow `now - issued_at`. Saturating arithmetic keeps
    // the comparison well-defined (and an out-of-window value is rejected).
    if now.saturating_sub(sa.issued_at) > ANNOUNCE_TTL_SECS {
        return Err("announcement expired".to_string());
    }
    if sa.issued_at.saturating_sub(now) > ANNOUNCE_MAX_SKEW_SECS {
        return Err("announcement from the future (clock skew?)".to_string());
    }
    let signer: iroh::EndpointId = sa
        .signed_by
        .parse()
        .map_err(|_| "invalid signer id".to_string())?;
    let sig_bytes = hex::decode(&sa.signature).map_err(|_| "invalid signature hex".to_string())?;
    let sig = iroh::Signature::try_from(sig_bytes.as_slice())
        .map_err(|_| "invalid signature length".to_string())?;
    signer
        .verify(&announce_signing_payload(sa.issued_at, &sa.payload), &sig)
        .map_err(|_| "signature verification failed".to_string())?;

    let announcement: cloudiy_protocol::ProviderAnnouncement =
        serde_json::from_str(&sa.payload).map_err(|e| format!("invalid payload: {e}"))?;
    if announcement.identity.as_str() != sa.signed_by {
        return Err("announced identity does not match signer".to_string());
    }
    Ok(announcement)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_sign_and_verify_roundtrip() {
        let bytes: [u8; 32] = rand::random();
        let secret = iroh::SecretKey::from_bytes(&bytes);
        let ann = cloudiy_protocol::ProviderAnnouncement {
            identity: cloudiy_protocol::Identity::new(secret.public().to_string()),
            resources: Default::default(),
            capabilities: vec!["wgsl".into()],
            region: None,
            price_micro_usdc_per_hour: 10_000,
            reputation: 0.0,
            utilization: 0.0,
            health: cloudiy_protocol::Health::Healthy,
            served_models: vec![],
            warm_models: vec![],
        };
        let now = 1_800_000_000;
        let sa = sign_announcement(&secret, &ann, now).unwrap();

        // Valid within TTL.
        let back = verify_announcement(&sa, now + 10).unwrap();
        assert_eq!(back.identity, ann.identity);

        // Expired, tampered payload, and identity mismatch all fail.
        assert!(verify_announcement(&sa, now + ANNOUNCE_TTL_SECS + 1).is_err());
        let mut tampered = sa.clone();
        tampered.payload = tampered.payload.replace("10000", "1");
        assert!(verify_announcement(&tampered, now).is_err());

        // Announcing on behalf of another node fails (identity != signer).
        let other: [u8; 32] = rand::random();
        let other_secret = iroh::SecretKey::from_bytes(&other);
        let forged = sign_announcement(&other_secret, &ann, now).unwrap();
        assert!(verify_announcement(&forged, now).is_err());
    }

    #[test]
    fn sign_and_verify_roundtrip() {
        let bytes: [u8; 32] = rand::random();
        let secret = iroh::SecretKey::from_bytes(&bytes);
        let id = secret.public();

        let sig = sign_result(&secret, "job-1", b"input", b"output");
        assert!(verify_result(&id, "job-1", b"input", b"output", &sig));

        // Any mutation must invalidate the signature — job id, input, output.
        assert!(!verify_result(&id, "job-2", b"input", b"output", &sig));
        assert!(!verify_result(&id, "job-1", b"tampered-in", b"output", &sig));
        assert!(!verify_result(&id, "job-1", b"input", b"tampered-out", &sig));
        assert!(!verify_result(&id, "job-1", b"input", b"output", "deadbeef"));

        // A different node cannot claim the result.
        let other: [u8; 32] = rand::random();
        let other_id = iroh::SecretKey::from_bytes(&other).public();
        assert!(!verify_result(&other_id, "job-1", b"input", b"output", &sig));
    }
}
