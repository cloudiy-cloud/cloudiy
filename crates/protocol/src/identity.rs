use serde::{Deserialize, Serialize};

/// An opaque, verifiable actor on the network.
///
/// Human, AI agent, company, backend service, DAO or another protocol —
/// the protocol never distinguishes. Today the string is an ed25519 public
/// key (iroh EndpointId / Solana pubkey); tomorrow it may be a DID. The
/// protocol only requires that it is stable and verifiable by the transport
/// or settlement layer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Identity(pub String);

impl Identity {
    pub fn new(id: impl Into<String>) -> Self {
        Identity(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Identity {
    fn from(s: &str) -> Self {
        Identity(s.to_string())
    }
}
