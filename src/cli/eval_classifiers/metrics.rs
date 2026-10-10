//! Error rates and model comparisons. Rates carry 95 % Wilson intervals as
//! description; a comparison is DECIDED by an exact paired test on the posts
//! where the two models disagree, with minimum label and coverage gates set
//! in advance (Codex review, round 2).

use std::collections::HashMap;

use super::data::Truth;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rate {
    pub hits: u32,
    pub n: u32,
}

impl Rate {
    pub fn value(&self) -> Option<f64> {
        (self.n > 0).then(|| self.hits as f64 / self.n as f64)
    }

    /// 95 % Wilson score interval — well-behaved at 0 and n.
    pub fn wilson95(&self) -> Option<(f64, f64)> {
        let p = self.value()?;
        let n = self.n as f64;
        let z = 1.959_964_f64;
        let z2 = z * z;
        let denom = 1.0 + z2 / n;
        let centre = (p + z2 / (2.0 * n)) / denom;
        let half = z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / denom;
        Some(((centre - half).max(0.0), (centre + half).min(1.0)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModelMetrics {
    pub model: String,
    pub false_alarm: Rate,
    pub miss: Rate,
    /// Labelled posts in scope.
    pub labelled: u32,
    /// Labelled posts the model actually answered.
    pub judged: u32,
    /// Labelled posts whose call failed, timed out or mismatched identity —
    /// never counted as clean.
    pub errors: u32,
}

impl ModelMetrics {
    pub fn coverage(&self) -> Option<f64> {
        (self.labelled > 0).then(|| self.judged as f64 / self.labelled as f64)
    }
}

fn is_wrong(called_toxic: bool, truth: Truth) -> bool {
    match truth {
        Truth::Toxic => !called_toxic,
        Truth::Clean => called_toxic,
    }
}

pub fn model_metrics(
    model: &str,
    calls: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> ModelMetrics {
    let mut m = ModelMetrics {
        model: model.to_string(),
        false_alarm: Rate::default(),
        miss: Rate::default(),
        labelled: labels.len() as u32,
        judged: 0,
        errors: 0,
    };
    for (id, truth) in labels {
        match calls.get(id) {
            None => {}
            Some(None) => m.errors += 1,
            Some(Some(called_toxic)) => {
                m.judged += 1;
                let rate = match truth {
                    Truth::Toxic => &mut m.miss,
                    Truth::Clean => &mut m.false_alarm,
                };
                rate.n += 1;
                if is_wrong(*called_toxic, *truth) {
                    rate.hits += 1;
                }
            }
        }
    }
    m
}

/// Gates, fixed before any data is seen.
pub const MIN_COVERAGE: f64 = 0.9;
pub const MIN_CLASS_N: u32 = 20;
pub const ALPHA: f64 = 0.05;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Comparison {
    Better,
    Worse,
    TooClose,
    NotEnoughLabels,
    LowCoverage,
}

/// Exact two-sided McNemar (sign) test: under "no difference" each of the
/// `b + c` discordant pairs is a fair coin. Exact rather than chi-squared
/// because the counts here are small. Fine for n ≤ 1000 in f64.
pub fn mcnemar_exact_p(b: u32, c: u32) -> f64 {
    let n = b + c;
    if n == 0 {
        return 1.0;
    }
    let k = b.min(c);
    let mut coef = 1.0_f64; // C(n, 0)
    let mut tail = 0.0_f64;
    for i in 0..=k {
        if i > 0 {
            coef = coef * f64::from(n - i + 1) / f64::from(i);
        }
        tail += coef;
    }
    (2.0 * tail / 2.0_f64.powi(n as i32)).min(1.0)
}

/// One truth class: coverage gate, label gate, then the exact paired test.
fn compare_class(
    base: &HashMap<String, Option<bool>>,
    cand: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
    class: Truth,
) -> Comparison {
    let in_class: Vec<&String> = labels
        .iter()
        .filter(|(_, t)| **t == class)
        .map(|(id, _)| id)
        .collect();
    if in_class.is_empty() {
        return Comparison::NotEnoughLabels;
    }
    let coverage = |calls: &HashMap<String, Option<bool>>| {
        let judged = in_class
            .iter()
            .filter(|id| matches!(calls.get(**id), Some(Some(_))))
            .count();
        judged as f64 / in_class.len() as f64
    };
    // Per class: a model that fails on the hard toxic posts cannot hide it
    // behind a perfect clean class (Codex review).
    if coverage(base) < MIN_COVERAGE || coverage(cand) < MIN_COVERAGE {
        return Comparison::LowCoverage;
    }
    let (mut shared, mut b, mut c) = (0u32, 0u32, 0u32);
    for id in &in_class {
        if let (Some(Some(bc)), Some(Some(cc))) = (base.get(*id), cand.get(*id)) {
            shared += 1;
            match (is_wrong(*bc, class), is_wrong(*cc, class)) {
                (false, true) => b += 1, // candidate wrong only
                (true, false) => c += 1, // baseline wrong only
                _ => {}
            }
        }
    }
    if shared < MIN_CLASS_N {
        return Comparison::NotEnoughLabels;
    }
    if mcnemar_exact_p(b, c) >= ALPHA {
        Comparison::TooClose
    } else if b < c {
        Comparison::Better
    } else {
        Comparison::Worse
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Paired {
    pub baseline: ModelMetrics,
    pub candidate: ModelMetrics,
    pub false_alarm: Comparison,
    pub miss: Comparison,
}

pub fn paired(
    base_name: &str,
    base: &HashMap<String, Option<bool>>,
    cand_name: &str,
    cand: &HashMap<String, Option<bool>>,
    labels: &HashMap<String, Truth>,
) -> Paired {
    let shared: HashMap<String, Truth> = labels
        .iter()
        .filter(|(id, _)| {
            matches!(base.get(*id), Some(Some(_))) && matches!(cand.get(*id), Some(Some(_)))
        })
        .map(|(id, t)| (id.clone(), *t))
        .collect();
    Paired {
        baseline: model_metrics(base_name, base, &shared),
        candidate: model_metrics(cand_name, cand, &shared),
        false_alarm: compare_class(base, cand, labels, Truth::Clean),
        miss: compare_class(base, cand, labels, Truth::Toxic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::data::Truth;

    fn map<T: Clone>(pairs: &[(&str, T)]) -> HashMap<String, T> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// `n` toxic labels t0.. and `n` clean labels c0...
    fn labels(n: usize) -> HashMap<String, Truth> {
        (0..n)
            .flat_map(|i| {
                [
                    (format!("t{i}"), Truth::Toxic),
                    (format!("c{i}"), Truth::Clean),
                ]
            })
            .collect()
    }

    /// Answers every label correctly, except the given ids (wrong) and
    /// `errors` (call failed).
    fn calls(
        labels: &HashMap<String, Truth>,
        wrong: &[String],
        errors: &[String],
    ) -> HashMap<String, Option<bool>> {
        labels
            .iter()
            .map(|(id, t)| {
                let right = *t == Truth::Toxic;
                let call = if errors.contains(id) {
                    None
                } else if wrong.contains(id) {
                    Some(!right)
                } else {
                    Some(right)
                };
                (id.clone(), call)
            })
            .collect()
    }

    fn ids(prefix: &str, range: std::ops::Range<usize>) -> Vec<String> {
        range.map(|i| format!("{prefix}{i}")).collect()
    }

    #[test]
    fn wilson_matches_known_values() {
        let (lo, hi) = Rate { hits: 0, n: 10 }.wilson95().unwrap();
        assert!(lo.abs() < 1e-9 && (hi - 0.2775).abs() < 1e-3, "{lo} {hi}");
        let (lo, hi) = Rate { hits: 5, n: 10 }.wilson95().unwrap();
        assert!(
            (lo - 0.2366).abs() < 1e-3 && (hi - 0.7634).abs() < 1e-3,
            "{lo} {hi}"
        );
        assert_eq!(Rate { hits: 0, n: 0 }.wilson95(), None);
    }

    #[test]
    fn rates_split_by_truth_and_errors_are_not_clean() {
        let labels = map(&[
            ("t1", Truth::Toxic),
            ("t2", Truth::Toxic),
            ("c1", Truth::Clean),
            ("c2", Truth::Clean),
            ("e", Truth::Toxic),
            ("gone", Truth::Clean),
        ]);
        let calls = map(&[
            ("t1", Some(true)),
            ("t2", Some(false)),
            ("c1", Some(true)),
            ("c2", Some(false)),
            ("e", None),
        ]);
        let m = model_metrics("m", &calls, &labels);
        assert_eq!(
            (m.miss.hits, m.miss.n),
            (1, 2),
            "the errored toxic post is not a miss"
        );
        assert_eq!((m.false_alarm.hits, m.false_alarm.n), (1, 2));
        assert_eq!((m.labelled, m.judged, m.errors), (6, 4, 1));
    }

    #[test]
    fn mcnemar_exact_values() {
        // 0 vs 6 discordant pairs: 2 × (1/2)^6 = 0.03125 (Codex's example).
        assert!((mcnemar_exact_p(0, 6) - 0.031_25).abs() < 1e-12);
        assert!((mcnemar_exact_p(0, 5) - 0.0625).abs() < 1e-12);
        assert_eq!(mcnemar_exact_p(1, 1), 1.0);
        assert_eq!(mcnemar_exact_p(0, 0), 1.0);
    }

    #[test]
    fn a_significantly_better_candidate_is_called_better() {
        let l = labels(25);
        // The baseline misses t0..t7; the candidate misses none: 8 discordant
        // pairs all favouring the candidate, p = 2/256 ≈ 0.0078.
        let base = calls(&l, &ids("t", 0..8), &[]);
        let cand = calls(&l, &[], &[]);
        let p = paired("base", &base, "cand", &cand, &l);
        assert_eq!(p.miss, Comparison::Better);
        assert_eq!(
            p.false_alarm,
            Comparison::TooClose,
            "no discordant clean pairs"
        );
    }

    /// Codex review: per-class coverage, tested exactly at the boundary.
    #[test]
    fn coverage_is_enforced_per_class_at_the_90_percent_boundary() {
        let l = labels(30);
        let base = calls(&l, &[], &[]);
        // 3 errors out of 30 toxic = 90 % coverage: allowed.
        let ok = calls(&l, &[], &ids("t", 0..3));
        assert_ne!(
            paired("b", &base, "c", &ok, &l).miss,
            Comparison::LowCoverage
        );
        // 4 errors out of 30 toxic = 86.7 %: refused, even though clean is perfect.
        let low = calls(&l, &[], &ids("t", 0..4));
        let p = paired("b", &base, "c", &low, &l);
        assert_eq!(p.miss, Comparison::LowCoverage);
        assert_ne!(
            p.false_alarm,
            Comparison::LowCoverage,
            "the clean class is judged on its own"
        );
    }

    /// Codex review: with too few paired labels no winner is named, however
    /// lopsided the counts.
    #[test]
    fn too_few_labels_per_class_gives_no_verdict() {
        let l = labels(10);
        let base = calls(&l, &ids("t", 0..6), &[]);
        let cand = calls(&l, &[], &[]);
        assert_eq!(
            paired("b", &base, "c", &cand, &l).miss,
            Comparison::NotEnoughLabels
        );
    }
}
