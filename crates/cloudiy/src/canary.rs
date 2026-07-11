//! Canary verification (RFC-0006 §5.1).
//!
//! A canary is a job whose correct answer is already known. Mixed
//! *indistinguishably* into a provider's job stream, it lets the network check
//! — statistically — that the provider is running the requested model on the
//! given input, without ever having to know the "right" answer to a real job.
//! A wrong/cheaper model, an ignored prompt, or canned output all fail a canary.
//!
//! This module is the **evaluable core**: the bank of `input → known-answer`
//! pairs, and the tolerant / fingerprint comparison that turns a provider's
//! answer into a pass/fail verdict. It is deliberately pure and hardware-neutral
//! — heterogeneous consumer GPUs produce slightly different logits, so canaries
//! are written to be *stable across hardware* (low-entropy answers or
//! model-discriminating facts), never byte-exact re-execution.
//!
//! Where canaries get injected into a live stream (so a provider can't tell a
//! checked job from a paying one) and how verdicts feed reputation is the
//! settlement layer (RFC-0006 §5/§6); this module is what that layer calls.

/// How a canary's expected answer is matched against the provider's output.
/// Chosen per canary so the comparison is robust to cross-hardware
/// nondeterminism rather than demanding an exact token match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// Case-insensitive exact match after trimming. For answers that are a
    /// single fixed token every correct model+hardware emits identically.
    Exact,
    /// The normalized expected text must appear as a substring of the
    /// normalized answer (lowercased, punctuation dropped, whitespace
    /// collapsed). For model-fingerprint checks — the right model *says X*.
    Contains,
    /// Parse the first number out of the answer and compare it to `expected`.
    /// For arithmetic/low-entropy canaries where models may pad with words.
    Number,
}

/// One reference `prompt → expected` pair that checks a given model endpoint.
#[derive(Clone, Debug)]
pub struct Canary {
    /// Endpoint key this canary checks (e.g. `"llama-ep"`).
    pub model: String,
    /// The exact input sent to the model.
    pub prompt: String,
    /// The known-good answer (interpreted per `mode`).
    pub expected: String,
    pub mode: MatchMode,
}

/// Normalize free text for tolerant comparison: lowercase, keep only
/// alphanumerics and spaces, collapse runs of whitespace.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = true; // trims leading space
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_space = false;
        } else if c.is_whitespace() || !c.is_alphanumeric() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// First signed integer/decimal number appearing in `s`, if any.
pub fn first_number(s: &str) -> Option<f64> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let starts = c.is_ascii_digit()
            || (c == b'-' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit());
        if starts {
            let start = i;
            if c == b'-' {
                i += 1;
            }
            let mut seen_dot = false;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit() || (bytes[i] == b'.' && !seen_dot))
            {
                if bytes[i] == b'.' {
                    seen_dot = true;
                }
                i += 1;
            }
            if let Ok(n) = s[start..i].trim_end_matches('.').parse::<f64>() {
                return Some(n);
            }
        } else {
            i += 1;
        }
    }
    None
}

impl Canary {
    /// True when `answer` is consistent with the model having honestly produced
    /// the known-good result for this canary.
    pub fn passes(&self, answer: &str) -> bool {
        match self.mode {
            MatchMode::Exact => answer.trim().eq_ignore_ascii_case(self.expected.trim()),
            MatchMode::Contains => {
                let hay = normalize(answer);
                let needle = normalize(&self.expected);
                !needle.is_empty() && hay.contains(&needle)
            }
            MatchMode::Number => match (first_number(answer), first_number(&self.expected)) {
                (Some(a), Some(e)) => (a - e).abs() < 1e-9,
                _ => false,
            },
        }
    }
}

/// The default embedded canary bank for the models this build serves. Each is
/// chosen to be stable across hardware: a correct model returns the expected
/// answer regardless of GPU, while a cheaper/wrong model or canned output does
/// not. (Whisper/audio canaries need reference audio and are not embedded yet.)
///
/// **Golden rule:** a canary must pass reliably on the *honest* model — a
/// near-zero false-negative rate — or it wrongly penalizes honest providers.
/// Prompts here are validated against the served model; ambiguous
/// instruction-following prompts (which small models flub) are avoided in
/// favor of echo / arithmetic / single-fact forms that even a 1B model nails.
pub fn default_bank() -> Vec<Canary> {
    vec![
        Canary {
            model: "llama-ep".into(),
            prompt: "Reply with only the number and nothing else: 17 + 26".into(),
            expected: "43".into(),
            mode: MatchMode::Number,
        },
        Canary {
            model: "llama-ep".into(),
            prompt: "Repeat this word back exactly, nothing else: BANANA".into(),
            expected: "BANANA".into(),
            mode: MatchMode::Contains,
        },
        Canary {
            model: "llama-ep".into(),
            prompt: "What is the capital of France? Answer with the city name only.".into(),
            expected: "Paris".into(),
            mode: MatchMode::Contains,
        },
        Canary {
            model: "llama-ep".into(),
            prompt: "Repeat this token back exactly, nothing else: PING".into(),
            expected: "PING".into(),
            mode: MatchMode::Exact,
        },
    ]
}

/// Outcome of probing a provider with a set of canaries.
#[derive(Clone, Debug, Default)]
pub struct ProbeResult {
    /// Per-canary `(prompt, passed, answer-snippet)`.
    pub items: Vec<(String, bool, String)>,
}

impl ProbeResult {
    pub fn passed(&self) -> usize {
        self.items.iter().filter(|(_, ok, _)| *ok).count()
    }
    pub fn total(&self) -> usize {
        self.items.len()
    }
    /// Fraction in `0.0..=1.0` (1.0 when there were no canaries to run).
    pub fn score(&self) -> f64 {
        if self.items.is_empty() {
            1.0
        } else {
            self.passed() as f64 / self.total() as f64
        }
    }
}

/// Probe THIS node's locally-served model with its canaries, running each
/// prompt through the real worker (`serve_endpoint`) and scoring the answer.
/// Used to self-check a provider bootstrapping its own node, and the building
/// block a directory-driven prober (RFC-0006 §6) will reuse against remotes.
pub async fn probe_local(model: &str) -> ProbeResult {
    let mut result = ProbeResult::default();
    for c in default_bank().into_iter().filter(|c| c.model == model) {
        let out = crate::gateway::serve_endpoint(&c.model, &c.prompt, None).await;
        let answer = out
            .get("output")
            .and_then(|v| v.as_str())
            .or_else(|| out.get("error").and_then(|v| v.as_str()))
            .unwrap_or("")
            .to_string();
        let ok = c.passes(&answer);
        let snippet: String = answer.chars().take(60).collect();
        result.items.push((c.prompt.clone(), ok, snippet));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_and_strips() {
        assert_eq!(normalize("  The  Capital, is: Paris! "), "the capital is paris");
        assert_eq!(normalize("BANANA."), "banana");
        assert_eq!(normalize("---"), "");
    }

    #[test]
    fn first_number_finds_leading_and_embedded() {
        assert_eq!(first_number("43"), Some(43.0));
        assert_eq!(first_number("The answer is 43."), Some(43.0));
        assert_eq!(first_number("-2.5 degrees"), Some(-2.5));
        assert_eq!(first_number("no digits here"), None);
    }

    #[test]
    fn number_canary_tolerates_padding() {
        let c = Canary {
            model: "llama-ep".into(),
            prompt: "17+26".into(),
            expected: "43".into(),
            mode: MatchMode::Number,
        };
        assert!(c.passes("43"));
        assert!(c.passes("The sum is 43."));
        assert!(!c.passes("The sum is 42.")); // wrong/cheaper model
        assert!(!c.passes("I cannot help with that")); // canned output
    }

    #[test]
    fn contains_canary_is_hardware_tolerant() {
        let c = Canary {
            model: "llama-ep".into(),
            prompt: "capital of France?".into(),
            expected: "Paris".into(),
            mode: MatchMode::Contains,
        };
        assert!(c.passes("The capital of France is Paris."));
        assert!(c.passes("paris")); // case/format differences are fine
        assert!(!c.passes("The capital of France is London.")); // wrong model
        assert!(!c.passes("")); // empty / no work
    }

    #[test]
    fn exact_canary_matches_single_token() {
        let c = Canary {
            model: "llama-ep".into(),
            prompt: "one word".into(),
            expected: "BANANA".into(),
            mode: MatchMode::Exact,
        };
        assert!(c.passes(" banana "));
        assert!(!c.passes("BANANA SPLIT"));
    }

    #[test]
    fn score_reflects_pass_fraction() {
        let mut r = ProbeResult::default();
        r.items.push(("a".into(), true, "x".into()));
        r.items.push(("b".into(), false, "y".into()));
        assert_eq!(r.passed(), 1);
        assert_eq!(r.total(), 2);
        assert!((r.score() - 0.5).abs() < 1e-9);
        assert_eq!(ProbeResult::default().score(), 1.0);
    }
}
