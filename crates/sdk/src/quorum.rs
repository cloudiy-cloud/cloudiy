//! Quorum policy for replicated execution (RFC-0008).
//!
//! Running the same deterministic kernel on N independent providers only helps
//! if there is one agreed rule for reading the N answers. That rule lives here,
//! as a **pure** function over `(node, output, signature_verified)` triples, so
//! the CLI and any agent embedding the SDK score a quorum identically — and so
//! the interesting cases (ties, all-unsigned, no majority) are unit-tested
//! without a network.
//!
//! Two invariants worth stating, because settlement depends on them:
//!
//! - **Only signature-verified results vote.** An unsigned result is not a
//!   dissenting opinion, it is an absent one: a provider that doesn't sign can't
//!   be settled against on-chain anyway (`release_verified` needs the signature).
//! - **The tally is deterministic.** Groups are keyed in a `BTreeMap` and ties
//!   break on the lowest output hash, so the same inputs always name the same
//!   winner. A non-deterministic quorum would make settlement unreproducible.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// One replica's answer, as seen by the consumer.
#[derive(Clone, Debug)]
pub struct ReplicaResult {
    /// Provider node id that produced it.
    pub node: String,
    /// Raw output bytes.
    pub output: Vec<u8>,
    /// Whether the provider's result signature verified against the node we
    /// dialed. Unverified results do not vote.
    pub signature_verified: bool,
}

/// The agreeing group when a strict majority was reached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Winner {
    /// `sha256(output)` of the agreed answer.
    pub hash: [u8; 32],
    /// The agreed output bytes.
    pub output: Vec<u8>,
    /// Providers that returned it, in the order they were collected. These are
    /// the ones a consumer settles (`release_verified`).
    pub nodes: Vec<String>,
}

/// Outcome of scoring a set of replica results.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tally {
    /// `Some` only when the largest agreeing group met [`Tally::threshold`].
    pub winner: Option<Winner>,
    /// Size of the largest agreeing group (whether or not it met the threshold).
    pub best_agreement: usize,
    /// Signature-verified results that voted.
    pub voters: usize,
    /// Votes required — a strict majority of the *requested* replica count, not
    /// of the replies received, so losing replicas can't lower the bar.
    pub threshold: usize,
    /// Verified providers whose output differed from the largest group. These
    /// are the ones to flag for reputation — and never to settle.
    pub divergent: Vec<String>,
    /// Providers whose result carried no valid signature (excluded from voting).
    pub unsigned: Vec<String>,
}

impl Tally {
    /// True when a strict majority agreed.
    pub fn reached(&self) -> bool {
        self.winner.is_some()
    }

    /// Providers that must **not** be paid: they diverged from the majority or
    /// never produced a verifiable signature.
    pub fn unsettleable(&self) -> Vec<&str> {
        self.divergent
            .iter()
            .chain(self.unsigned.iter())
            .map(String::as_str)
            .collect()
    }
}

/// Votes required for a strict majority of `replicas`.
pub fn threshold(replicas: usize) -> usize {
    replicas / 2 + 1
}

/// Score replica results against a strict majority of `replicas`.
///
/// `replicas` is the number of providers the run *asked* for. Using it (rather
/// than the number that replied) keeps the bar fixed: if 3 of 5 replicas never
/// answer, the remaining 2 agreeing with each other is not a quorum.
pub fn tally(results: &[ReplicaResult], replicas: usize) -> Tally {
    let threshold = threshold(replicas);

    let unsigned: Vec<String> = results
        .iter()
        .filter(|r| !r.signature_verified)
        .map(|r| r.node.clone())
        .collect();

    // Group verified results by output hash. BTreeMap keeps grouping and tie
    // ordering deterministic.
    let mut groups: BTreeMap<[u8; 32], Vec<String>> = BTreeMap::new();
    let mut bytes: BTreeMap<[u8; 32], Vec<u8>> = BTreeMap::new();
    for r in results.iter().filter(|r| r.signature_verified) {
        let h: [u8; 32] = Sha256::digest(&r.output).into();
        groups.entry(h).or_default().push(r.node.clone());
        bytes.entry(h).or_insert_with(|| r.output.clone());
    }

    let voters: usize = groups.values().map(Vec::len).sum();

    // Largest group; ties break on the lowest hash (BTreeMap order) so the
    // choice is reproducible. A tie can never *win* anyway — two groups of equal
    // size can't both be a strict majority — but the reference group is still
    // used to attribute divergence.
    let best = groups
        .iter()
        .max_by_key(|(hash, nodes)| (nodes.len(), std::cmp::Reverse(**hash)));

    let Some((best_hash, best_nodes)) = best else {
        return Tally {
            winner: None,
            best_agreement: 0,
            voters,
            threshold,
            divergent: Vec::new(),
            unsigned,
        };
    };
    let best_agreement = best_nodes.len();

    let divergent: Vec<String> = groups
        .iter()
        .filter(|(h, _)| *h != best_hash)
        .flat_map(|(_, nodes)| nodes.iter().cloned())
        .collect();

    let winner = (best_agreement >= threshold).then(|| Winner {
        hash: *best_hash,
        output: bytes.get(best_hash).cloned().unwrap_or_default(),
        nodes: best_nodes.clone(),
    });

    Tally {
        winner,
        best_agreement,
        voters,
        threshold,
        divergent,
        unsigned,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(node: &str, output: &[u8], verified: bool) -> ReplicaResult {
        ReplicaResult {
            node: node.to_string(),
            output: output.to_vec(),
            signature_verified: verified,
        }
    }

    #[test]
    fn threshold_is_a_strict_majority() {
        assert_eq!(threshold(1), 1);
        assert_eq!(threshold(2), 2); // 2-of-2: no majority with a single vote
        assert_eq!(threshold(3), 2);
        assert_eq!(threshold(4), 3);
        assert_eq!(threshold(5), 3);
    }

    #[test]
    fn unanimous_agreement_wins() {
        let out = b"42";
        let t = tally(&[r("a", out, true), r("b", out, true)], 2);
        let w = t.winner.as_ref().expect("quorum");
        assert_eq!(w.output, out);
        assert_eq!(w.nodes, vec!["a", "b"]);
        assert!(t.divergent.is_empty());
        assert!(t.unsigned.is_empty());
        assert_eq!(t.voters, 2);
    }

    #[test]
    fn majority_wins_and_names_the_divergent_provider() {
        let t = tally(
            &[
                r("a", b"42", true),
                r("b", b"42", true),
                r("c", b"99", true), // the liar
            ],
            3,
        );
        let w = t.winner.as_ref().expect("quorum");
        assert_eq!(w.output, b"42");
        assert_eq!(w.nodes, vec!["a", "b"]);
        assert_eq!(t.divergent, vec!["c"]);
        // The divergent provider is never settled.
        assert_eq!(t.unsettleable(), vec!["c"]);
    }

    #[test]
    fn unsigned_results_do_not_vote() {
        // 'b' returns the same bytes but unsigned — it cannot make a quorum.
        let t = tally(&[r("a", b"42", true), r("b", b"42", false)], 2);
        assert!(!t.reached(), "one vote cannot satisfy a 2-of-2 threshold");
        assert_eq!(t.voters, 1);
        assert_eq!(t.unsigned, vec!["b"]);
        assert_eq!(t.unsettleable(), vec!["b"]);
    }

    #[test]
    fn threshold_uses_requested_replicas_not_replies() {
        // Asked for 5, only 2 answered and agreed: not a quorum (needs 3).
        let t = tally(&[r("a", b"42", true), r("b", b"42", true)], 5);
        assert!(!t.reached());
        assert_eq!(t.best_agreement, 2);
        assert_eq!(t.threshold, 3);
    }

    #[test]
    fn an_even_split_is_no_quorum() {
        let t = tally(
            &[
                r("a", b"42", true),
                r("b", b"42", true),
                r("c", b"99", true),
                r("d", b"99", true),
            ],
            4,
        );
        assert!(!t.reached(), "2-2 is not a strict majority of 4");
        assert_eq!(t.best_agreement, 2);
    }

    #[test]
    fn no_verified_results_yields_no_winner() {
        let t = tally(&[r("a", b"42", false), r("b", b"99", false)], 2);
        assert!(!t.reached());
        assert_eq!(t.voters, 0);
        assert_eq!(t.best_agreement, 0);
        assert_eq!(t.unsigned, vec!["a", "b"]);
    }

    #[test]
    fn empty_results_are_handled() {
        let t = tally(&[], 3);
        assert!(!t.reached());
        assert_eq!(t.voters, 0);
        assert!(t.unsettleable().is_empty());
    }

    #[test]
    fn tally_is_deterministic_across_orderings() {
        // Same multiset of answers, different arrival order → same winner.
        let a = tally(
            &[
                r("a", b"42", true),
                r("b", b"99", true),
                r("c", b"42", true),
            ],
            3,
        );
        let b = tally(
            &[
                r("c", b"42", true),
                r("a", b"42", true),
                r("b", b"99", true),
            ],
            3,
        );
        assert_eq!(a.winner.unwrap().hash, b.winner.unwrap().hash);
        assert_eq!(a.divergent, b.divergent);
    }
}
