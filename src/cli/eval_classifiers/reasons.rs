//! Reason families for explainable flags (#407).
//!
//! The master policy alone decides toxic or clean. These six families only
//! explain a flag the master already made, so the protected user can see WHY.
//! They are distinct concepts on purpose: Zentropi's S1 model card warns that
//! bundling overlapping shades of one concept costs accuracy.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every reason family, in display order. Labels, verdicts and the report
/// all use exactly these keys.
pub const REASON_KEYS: [&str; 6] = [
    "personal_attack",
    "identity_hate",
    "mockery",
    "threat",
    "pile_on",
    "bad_faith",
];

/// One family's answer for one post.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ReasonAnswer {
    pub fired: bool,
    /// The hosted API's confidence in its own answer (0.5–1.0), not P(true).
    pub confidence: f32,
}

/// The reason answers for one post.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReasonSet {
    pub answers: BTreeMap<String, ReasonAnswer>,
    /// False when any family was missing or garbled in the response. The
    /// master verdict is still kept; the report leaves incomplete sets out of
    /// its reason rates rather than counting a missing answer as "no".
    pub complete: bool,
}

impl ReasonSet {
    /// The families that said yes, in [`REASON_KEYS`] order.
    pub fn fired(&self) -> Vec<&'static str> {
        REASON_KEYS
            .iter()
            .copied()
            .filter(|k| self.answers.get(*k).is_some_and(|a| a.fired))
            .collect()
    }

    /// The most confident family that said yes. Ties go to the earlier
    /// family in [`REASON_KEYS`], so the answer never depends on map order.
    pub fn top(&self) -> Option<&'static str> {
        let mut best: Option<(&'static str, f32)> = None;
        for k in self.fired() {
            let c = self.answers[k].confidence;
            if best.is_none_or(|(_, b)| c > b) {
                best = Some((k, c));
            }
        }
        best.map(|(k, _)| k)
    }
}

/// A classifier that can also say which reasons it found. Kept apart from
/// `ToxicityClassifier` so production's trait is untouched: the eval runner
/// asks for reasons right after each verdict and saves them alongside.
pub trait ReasonSource: Send + Sync {
    /// The reasons recorded for the latest answer on exactly this text,
    /// removed so the store does not grow over a run.
    fn take_reasons(&self, content: &str) -> Option<ReasonSet>;
}

/// Refuse unknown or repeated reason keys, naming the valid ones, so a typo
/// in a hand-edited label is caught instead of silently never matching.
pub fn check_reason_keys(keys: &[String]) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for k in keys {
        if !REASON_KEYS.contains(&k.as_str()) {
            bail!("unknown reason {k:?} (valid: {})", REASON_KEYS.join(", "));
        }
        if !seen.insert(k.as_str()) {
            bail!("reason {k:?} is listed twice");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(pairs: &[(&str, bool, f32)], complete: bool) -> ReasonSet {
        ReasonSet {
            answers: pairs
                .iter()
                .map(|(k, fired, c)| {
                    (
                        k.to_string(),
                        ReasonAnswer {
                            fired: *fired,
                            confidence: *c,
                        },
                    )
                })
                .collect(),
            complete,
        }
    }

    #[test]
    fn fired_lists_only_yes_answers_in_family_order() {
        let s = set(
            &[
                ("threat", true, 0.9),
                ("mockery", false, 0.8),
                ("personal_attack", true, 0.6),
            ],
            true,
        );
        assert_eq!(s.fired(), vec!["personal_attack", "threat"]);
    }

    #[test]
    fn top_is_the_most_confident_fired_reason_ties_go_to_family_order() {
        let s = set(&[("threat", true, 0.7), ("mockery", true, 0.95)], true);
        assert_eq!(s.top(), Some("mockery"));
        let tie = set(
            &[("threat", true, 0.8), ("personal_attack", true, 0.8)],
            true,
        );
        assert_eq!(tie.top(), Some("personal_attack"));
        assert_eq!(set(&[("threat", false, 0.9)], true).top(), None);
    }

    #[test]
    fn a_reason_set_round_trips_through_json() {
        let s = set(&[("pile_on", true, 0.66)], false);
        let back: ReasonSet = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn reason_keys_must_be_known_and_unique() {
        assert!(check_reason_keys(&[]).is_ok());
        assert!(check_reason_keys(&["mockery".into(), "threat".into()]).is_ok());
        let err = format!("{:#}", check_reason_keys(&["mocking".into()]).unwrap_err());
        assert!(
            err.contains("mocking") && err.contains("personal_attack"),
            "{err}"
        );
        let dup = format!(
            "{:#}",
            check_reason_keys(&["threat".into(), "threat".into()]).unwrap_err()
        );
        assert!(dup.contains("twice"), "{dup}");
    }
}
