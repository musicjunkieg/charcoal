//! How well S1's reasons explain its flags (#407, spec §6). Holdout only,
//! like the headline: diagnostic and seed posts were picked by hand or by
//! model behaviour and would bias these rates.

use std::collections::HashMap;
use std::fmt::Write;

use super::cache::VerdictRecord;
use super::data::{Answer, Source, Truth};
use super::metrics::Rate;
use super::reasons::REASON_KEYS;
use super::report::rate_cell;

/// Below this many ticked posts a family's rate is noise, so none is shown.
pub const MIN_TICKED: u32 = 10;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FamilyStats {
    pub key: &'static str,
    /// Of posts where Bryan ticked this family: how often S1 said yes.
    pub when_ticked: Rate,
    /// Of posts where he ticked others but not this one: how often S1 said yes.
    pub when_not_ticked: Rate,
    /// Posts the master called clean where this family still fired.
    pub on_clean_calls: u32,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReasonStats {
    pub flagged: u32,
    /// Flagged with no reason: what the protected user would see as
    /// "flagged under your overall policy; no specific reason matched".
    pub unexplained: u32,
    pub clean_calls: u32,
    /// A high count means some reason policy is broader than the master.
    pub clean_with_reasons: u32,
    pub incomplete: u32,
    pub top_agree: Rate,
    pub families: Vec<FamilyStats>,
}

pub fn reason_stats(
    verdicts: &HashMap<String, VerdictRecord>,
    answers: &HashMap<String, Answer>,
    ticked: &HashMap<String, Vec<String>>,
) -> ReasonStats {
    let mut s = ReasonStats {
        families: REASON_KEYS
            .iter()
            .map(|k| FamilyStats {
                key: k,
                ..FamilyStats::default()
            })
            .collect(),
        ..ReasonStats::default()
    };
    for (id, rec) in verdicts {
        let Some(answer) = answers.get(id).filter(|a| a.source == Source::Holdout) else {
            continue;
        };
        let (Some(called), Some(set)) = (rec.called_toxic, rec.reasons.as_ref()) else {
            continue;
        };
        if !set.complete {
            s.incomplete += 1;
            continue;
        }
        let fired = set.fired();
        if called {
            s.flagged += 1;
            if fired.is_empty() {
                s.unexplained += 1;
            }
        } else {
            s.clean_calls += 1;
            if !fired.is_empty() {
                s.clean_with_reasons += 1;
            }
            for f in s.families.iter_mut().filter(|f| fired.contains(&f.key)) {
                f.on_clean_calls += 1;
            }
        }
        let Some(ticks) = ticked.get(id).filter(|t| !t.is_empty()) else {
            continue;
        };
        if answer.truth != Truth::Toxic {
            continue;
        }
        for f in &mut s.families {
            let rate = if ticks.iter().any(|t| t == f.key) {
                &mut f.when_ticked
            } else {
                &mut f.when_not_ticked
            };
            rate.n += 1;
            if fired.contains(&f.key) {
                rate.hits += 1;
            }
        }
        if let Some(top) = set.top() {
            s.top_agree.n += 1;
            if ticks.iter().any(|t| t == top) {
                s.top_agree.hits += 1;
            }
        }
    }
    s
}

pub fn render_reasons(model: &str, s: &ReasonStats) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "\nREASONS — {model}, holdout posts");
    let _ = writeln!(
        out,
        "  flagged with no reason: {} of {}",
        s.unexplained, s.flagged
    );
    let _ = writeln!(
        out,
        "  clean calls where a reason still fired: {} of {}",
        s.clean_with_reasons, s.clean_calls
    );
    let _ = writeln!(out, "  left out (incomplete reasons): {}", s.incomplete);
    let _ = writeln!(
        out,
        "  top reason is one you ticked: {}",
        rate_cell(&s.top_agree)
    );
    let _ = writeln!(
        out,
        "  {:<16} {:<28} {:<28} {:>9}",
        "family", "fired when you ticked it", "fired when you didn't", "on clean"
    );
    for f in &s.families {
        if f.when_ticked.n < MIN_TICKED {
            let _ = writeln!(
                out,
                "  {:<16} too few examples ({} < {MIN_TICKED}) {:>40}",
                f.key, f.when_ticked.n, f.on_clean_calls
            );
            continue;
        }
        let _ = writeln!(
            out,
            "  {:<16} {:<28} {:<28} {:>9}",
            f.key,
            rate_cell(&f.when_ticked),
            rate_cell(&f.when_not_ticked),
            f.on_clean_calls
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::data::{Answer, Source, Truth};
    use crate::cli::eval_classifiers::reasons::{ReasonAnswer, ReasonSet};

    fn rec(
        id: &str,
        toxic: bool,
        fired: &[(&str, f32)],
        complete: bool,
    ) -> (String, VerdictRecord) {
        let answers = crate::cli::eval_classifiers::reasons::REASON_KEYS
            .iter()
            .map(|k| {
                let hit = fired.iter().find(|(f, _)| f == k);
                (
                    k.to_string(),
                    ReasonAnswer {
                        fired: hit.is_some(),
                        confidence: hit.map_or(0.9, |(_, c)| *c),
                    },
                )
            })
            .collect();
        (
            id.to_string(),
            VerdictRecord {
                id: id.into(),
                model: "s1-bundle".into(),
                content_hash: String::new(),
                model_id: "cope-b-s1".into(),
                policy_version: "v".into(),
                called_toxic: Some(toxic),
                error: None,
                latency_ms: 1,
                input_chars: 1,
                est_usd: 0.0,
                reasons: Some(ReasonSet { answers, complete }),
            },
        )
    }
    fn ans(truth: Truth, source: Source) -> Answer {
        Answer { truth, source }
    }

    #[test]
    fn counts_unexplained_flags_reasons_on_clean_and_incomplete_sets() {
        let v: HashMap<_, _> = [
            rec("a", true, &[], true),                 // unexplained flag
            rec("b", true, &[("mockery", 0.9)], true), // explained flag
            rec("c", false, &[("threat", 0.7)], true), // reason on a clean call
            rec("d", false, &[], true),                // clean, quiet
            rec("e", true, &[("threat", 0.9)], false), // incomplete: left out
            rec("s", true, &[], true),                 // seed: not holdout
        ]
        .into();
        let mut a: HashMap<String, Answer> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|id| (id.to_string(), ans(Truth::Toxic, Source::Holdout)))
            .collect();
        a.insert("s".into(), ans(Truth::Toxic, Source::Seed));
        let s = reason_stats(&v, &a, &HashMap::new());
        assert_eq!((s.flagged, s.unexplained), (2, 1));
        assert_eq!((s.clean_calls, s.clean_with_reasons), (2, 1));
        assert_eq!(s.incomplete, 1);
        let threat = s.families.iter().find(|f| f.key == "threat").unwrap();
        assert_eq!(threat.on_clean_calls, 1);
    }

    #[test]
    fn scores_reasons_against_what_was_ticked() {
        let v: HashMap<_, _> = [
            rec("a", true, &[("mockery", 0.95), ("threat", 0.6)], true),
            rec("b", true, &[("threat", 0.9)], true),
            rec("c", true, &[("personal_attack", 0.8)], true),
        ]
        .into();
        let a: HashMap<String, Answer> = ["a", "b", "c"]
            .iter()
            .map(|id| (id.to_string(), ans(Truth::Toxic, Source::Holdout)))
            .collect();
        let ticked: HashMap<String, Vec<String>> = [
            ("a".to_string(), vec!["mockery".to_string()]),
            ("b".to_string(), vec!["mockery".to_string()]),
            // "c" has no ticks: left out of the agreement figures.
        ]
        .into();
        let s = reason_stats(&v, &a, &ticked);
        assert_eq!(s.top_agree, Rate { hits: 1, n: 2 });
        let mockery = s.families.iter().find(|f| f.key == "mockery").unwrap();
        assert_eq!(mockery.when_ticked, Rate { hits: 1, n: 2 });
        assert_eq!(mockery.when_not_ticked, Rate { hits: 0, n: 0 });
        let threat = s.families.iter().find(|f| f.key == "threat").unwrap();
        assert_eq!(threat.when_ticked, Rate { hits: 0, n: 0 });
        assert_eq!(threat.when_not_ticked, Rate { hits: 2, n: 2 });
    }

    #[test]
    fn a_rare_family_says_too_few_examples_instead_of_a_rate() {
        let s = ReasonStats {
            families: vec![FamilyStats {
                key: "pile_on",
                when_ticked: Rate { hits: 2, n: 3 },
                when_not_ticked: Rate { hits: 0, n: 40 },
                on_clean_calls: 0,
            }],
            ..ReasonStats::default()
        };
        let out = render_reasons("s1-bundle", &s);
        assert!(
            out.contains("pile_on") && out.contains("too few examples (3 < 10)"),
            "{out}"
        );
        assert!(out.contains("REASONS"), "{out}");
    }

    #[test]
    fn the_dev_pricing_note_appears_only_for_s1() {
        use crate::cli::eval_classifiers::report::dev_pricing_note;
        assert_eq!(dev_pricing_note(&["cope-b"]), "");
        assert!(dev_pricing_note(&["cope-b", "s1-bundle"]).contains("free for development"));
    }
}
