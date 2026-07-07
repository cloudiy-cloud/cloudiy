use serde::{Deserialize, Serialize};

/// A named functionality a node offers — first-class citizens of the
/// protocol. Resources describe *quantity*; capabilities describe
/// *functionality*.
///
/// Convention: lowercase name, optional `:version` suffix.
/// `docker`, `cuda:12.8`, `pytorch`, `ffmpeg`, `linux`, `arm64`, `ollama`…
///
/// Consumers request capabilities, never machines:
/// *"I need cuda:12.8 and pytorch"* — not *"I need machine X"*.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capability(pub String);

impl Capability {
    pub fn new(name: impl Into<String>) -> Self {
        Capability(name.into().to_lowercase())
    }

    pub fn name(&self) -> &str {
        self.0.split(':').next().unwrap_or(&self.0)
    }

    pub fn version(&self) -> Option<&str> {
        self.0.split_once(':').map(|(_, v)| v)
    }

    /// Does this offered capability satisfy a requirement?
    ///
    /// - requirement without version matches any version:
    ///   offered `cuda:12.8` satisfies required `cuda`
    /// - requirement with version needs an exact version match:
    ///   offered `cuda:12.8` satisfies `cuda:12.8`, not `cuda:11.0`
    /// - offered without version satisfies only versionless requirements
    pub fn satisfies(&self, required: &Capability) -> bool {
        if self.name() != required.name() {
            return false;
        }
        match required.version() {
            None => true,
            Some(v) => self.version() == Some(v),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Capability {
    fn from(s: &str) -> Self {
        Capability::new(s)
    }
}

/// True when every `required` capability is satisfied by some `offered` one.
pub fn satisfies_all(offered: &[Capability], required: &[Capability]) -> bool {
    required
        .iter()
        .all(|req| offered.iter().any(|off| off.satisfies(req)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versionless_requirement_matches_any_version() {
        assert!(Capability::from("cuda:12.8").satisfies(&"cuda".into()));
        assert!(Capability::from("pytorch").satisfies(&"pytorch".into()));
    }

    #[test]
    fn versioned_requirement_is_exact() {
        assert!(Capability::from("cuda:12.8").satisfies(&"cuda:12.8".into()));
        assert!(!Capability::from("cuda:12.8").satisfies(&"cuda:11.0".into()));
        assert!(!Capability::from("cuda").satisfies(&"cuda:12.8".into()));
    }

    #[test]
    fn different_names_never_match() {
        assert!(!Capability::from("opencl").satisfies(&"cuda".into()));
    }

    #[test]
    fn consumer_requests_functionality_not_machines() {
        let node = vec![
            "docker".into(),
            "linux".into(),
            "x86_64".into(),
            Capability::from("cuda:12.8"),
            "pytorch".into(),
        ];
        // "I need CUDA 12.8 and PyTorch."
        assert!(satisfies_all(&node, &["cuda:12.8".into(), "pytorch".into()]));
        assert!(!satisfies_all(&node, &["cuda:12.8".into(), "comfyui".into()]));
    }
}
