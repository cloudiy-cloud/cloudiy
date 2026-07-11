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

/// One reference input → expected pair that checks a given model endpoint. The
/// input is a text `prompt` (chat/text models) or, for speech-to-text, an
/// `audio_b64` clip whose transcription is known.
#[derive(Clone, Debug)]
pub struct Canary {
    /// Endpoint key this canary checks (e.g. `"llama-ep"`, `"whisper-ep"`).
    pub model: String,
    /// The text input sent to the model (empty for audio canaries).
    pub prompt: String,
    /// Base64 audio input for speech-to-text canaries (`None` for text).
    pub audio_b64: Option<String>,
    /// The known-good answer (interpreted per `mode`).
    pub expected: String,
    pub mode: MatchMode,
}

impl Canary {
    /// A text-input canary (chat/text models).
    pub fn text(model: &str, prompt: &str, expected: &str, mode: MatchMode) -> Canary {
        Canary {
            model: model.into(),
            prompt: prompt.into(),
            audio_b64: None,
            expected: expected.into(),
            mode,
        }
    }
    /// An audio-input canary (speech-to-text): the transcription is `expected`.
    pub fn audio(model: &str, audio_b64: &str, expected: &str, mode: MatchMode) -> Canary {
        Canary {
            model: model.into(),
            prompt: String::new(),
            audio_b64: Some(audio_b64.into()),
            expected: expected.into(),
            mode,
        }
    }
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
        } else if !prev_space {
            // Any non-alphanumeric collapses to a single separating space.
            out.push(' ');
            prev_space = true;
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
/// not. Includes an embedded speech-to-text canary for whisper (a short audio
/// clip with a known transcription).
///
/// **Golden rule:** a canary must pass reliably on the *honest* model — a
/// near-zero false-negative rate — or it wrongly penalizes honest providers.
/// Prompts here are validated against the served model; ambiguous
/// instruction-following prompts (which small models flub) are avoided in
/// favor of echo / arithmetic / single-fact forms that even a 1B model nails.
pub fn default_bank() -> Vec<Canary> {
    vec![
        Canary::text(
            "llama-ep",
            "Reply with only the number and nothing else: 17 + 26",
            "43",
            MatchMode::Number,
        ),
        Canary::text(
            "llama-ep",
            "Repeat this word back exactly, nothing else: BANANA",
            "BANANA",
            MatchMode::Contains,
        ),
        Canary::text(
            "llama-ep",
            "What is the capital of France? Answer with the city name only.",
            "Paris",
            MatchMode::Contains,
        ),
        Canary::text(
            "llama-ep",
            "Repeat this token back exactly, nothing else: PING",
            "PING",
            MatchMode::Exact,
        ),
        // Speech-to-text: a short embedded clip ("the quick brown fox"). The
        // base model can mishear a word ("the"→"de"), so we check the stable
        // core phrase with a tolerant Contains — a wrong/absent model fails it.
        Canary::audio(
            "whisper-ep",
            include_str!("canary_whisper.b64"),
            "quick brown fox",
            MatchMode::Contains,
        ),
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
        let out =
            crate::gateway::serve_endpoint(&c.model, &c.prompt, c.audio_b64.as_deref()).await;
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

/// Per-cycle spend/rate cap for the prober (RFC-0006 §5.1/§10). Canary compute
/// isn't free — bound how many canary runs a directory issues per cycle so a
/// large provider set can't blow up the prober's cost or hammer the network.
/// Who *funds* paid canaries (x402 against a `--require-payment` provider) is
/// the open §10 question; this is the control knob for it.
#[derive(Clone, Debug)]
pub struct CanaryBudget {
    max_runs: usize,
    used: usize,
}

impl CanaryBudget {
    pub fn new(max_runs: usize) -> Self {
        CanaryBudget { max_runs, used: 0 }
    }
    /// Reserve one canary run; `false` when this cycle's budget is spent.
    pub fn try_spend(&mut self) -> bool {
        if self.used >= self.max_runs {
            return false;
        }
        self.used += 1;
        true
    }
    pub fn spent(&self) -> usize {
        self.used
    }
}

/// Probe a REMOTE provider (RFC-0006 §5.1): dial it and run each canary for
/// `model` as an endpoint job, scoring the answer. This is what a directory's
/// prober (and `cloudiy canary --to`) calls to earn/lose a provider's
/// reputation. `token` is a dev/admission token for providers that gate runs;
/// paid canaries (a provider running `--require-payment`) need a funded path —
/// who pays for canary compute is RFC-0006 §10.
pub async fn probe_remote(
    endpoint: &iroh::Endpoint,
    provider: &str,
    model: &str,
    token: Option<&str>,
) -> anyhow::Result<ProbeResult> {
    use cloudiy_common::proto::{self, Request, Response};
    let id: iroh::EndpointId = provider.parse().map_err(|_| anyhow::anyhow!("invalid provider id"))?;
    let mut result = ProbeResult::default();
    for c in default_bank()
        .into_iter()
        // Audio canaries can't be probed remotely — audio does not cross the
        // RunEndpoint frame today (it runs on the caller's local gateway). So
        // speech-to-text is self-check only (`cloudiy canary`) until then.
        .filter(|c| c.model == model && c.audio_b64.is_none())
    {
        let req = cloudiy_common::JobRequest {
            job_id: uuid::Uuid::new_v4().to_string(),
            kernel: format!("endpoint:{model}"),
            input_data: vec![],
            params: Default::default(),
            auth_token: token.unwrap_or_default().to_string(),
            consumer_pubkey: None,
            payment: None,
        };
        let rpc = Request::RunEndpoint {
            request: req,
            key: model.to_string(),
            prompt: c.prompt.clone(),
        };
        // Only a real answer yields a verdict. Unreachable / errored /
        // PaymentRequired → `None` → SKIP (no penalty): the provider didn't
        // cheat, we just couldn't evaluate. Conflating "couldn't probe" with
        // "wrong answer" would wrongly crater an honest but paid/offline node.
        let answer: Option<String> = async {
            let conn = endpoint.connect(id, proto::ALPN).await.ok()?;
            let (mut send, mut recv) = conn.open_bi().await.ok()?;
            proto::write_msg(&mut send, &rpc).await.ok()?;
            let resp: Response = proto::read_msg(&mut recv).await.ok()?;
            conn.close(0u32.into(), b"done");
            match resp {
                Response::Job(r) => {
                    let v: serde_json::Value =
                        serde_json::from_slice(&r.output_data).unwrap_or_default();
                    Some(
                        v.get("output")
                            .and_then(|s| s.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                }
                _ => None,
            }
        }
        .await;
        if let Some(answer) = answer {
            let ok = c.passes(&answer);
            let snippet: String = answer.chars().take(60).collect();
            result.items.push((c.prompt.clone(), ok, snippet));
        }
    }
    Ok(result)
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
        let c = Canary::text("llama-ep", "17+26", "43", MatchMode::Number);
        assert!(c.passes("43"));
        assert!(c.passes("The sum is 43."));
        assert!(!c.passes("The sum is 42.")); // wrong/cheaper model
        assert!(!c.passes("I cannot help with that")); // canned output
    }

    #[test]
    fn contains_canary_is_hardware_tolerant() {
        let c = Canary::text("llama-ep", "capital of France?", "Paris", MatchMode::Contains);
        assert!(c.passes("The capital of France is Paris."));
        assert!(c.passes("paris")); // case/format differences are fine
        assert!(!c.passes("The capital of France is London.")); // wrong model
        assert!(!c.passes("")); // empty / no work
    }

    #[test]
    fn exact_canary_matches_single_token() {
        let c = Canary::text("llama-ep", "one word", "BANANA", MatchMode::Exact);
        assert!(c.passes(" banana "));
        assert!(!c.passes("BANANA SPLIT"));
    }

    #[test]
    fn whisper_canary_is_audio_and_tolerant() {
        let w = default_bank()
            .into_iter()
            .find(|c| c.model == "whisper-ep")
            .expect("whisper canary in the bank");
        assert!(w.audio_b64.is_some(), "whisper canary carries embedded audio");
        assert!(w.prompt.is_empty(), "audio canary has no text prompt");
        // The base model can mishear "the"→"de" — the tolerant core phrase holds.
        assert!(w.passes("De quick brown fox."));
        assert!(!w.passes("hello world")); // wrong/absent model
    }

    #[test]
    fn budget_caps_runs_per_cycle() {
        let mut b = CanaryBudget::new(3);
        assert!(b.try_spend());
        assert!(b.try_spend());
        assert!(b.try_spend());
        assert!(!b.try_spend()); // exhausted — 4th is refused
        assert_eq!(b.spent(), 3);
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
