// Aggregation — turn per-candidate dry-run counts into a network estimate.
//
// The dry run gives a would-be-Zentropi-call count per sampled candidate, each
// tagged with its engagement stratum. This module rolls those up into a
// distribution: per-stratum mean / median / p90 / p99 / max, plus a single
// reweighted "expected calls per candidate" and an optional projected network
// total.
//
// Why reweight: Zentropi cost is power-law in engagement, so a plain average
// over the sample is dominated by — or, if the tail is under-sampled, blind to —
// the viral stratum. The honest workflow is to (1) stratify the *whole* filtered
// population for free (engagement stage) to learn each stratum's true share,
// then (2) run the expensive dry run on a sample (optionally oversampling the
// tail for resolution), and (3) reweight the per-stratum means by the population
// shares. That makes the tail count exactly as much as it really does — no more,
// no less. When no population weights are supplied we fall back to the sample's
// own stratum distribution, which assumes the sample is representative.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::discovery::dry_run::CandidateDryRun;
use crate::discovery::engagement::EngagementStratum;

/// Canonical stratum order for stable, readable output.
const STRATA_ORDER: [EngagementStratum; 5] = [
    EngagementStratum::None,
    EngagementStratum::Low,
    EngagementStratum::Medium,
    EngagementStratum::High,
    EngagementStratum::Viral,
];

/// Distribution of would-be Zentropi calls within one engagement stratum.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StratumStats {
    pub stratum: &'static str,
    pub sample_size: usize,
    pub mean: f64,
    pub median: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: u64,
    pub total: u64,
    /// Share of the *population* this stratum represents (0..1), used as the
    /// reweighting coefficient.
    pub population_share: f64,
}

/// The rolled-up network estimate.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NetworkEstimate {
    pub per_stratum: Vec<StratumStats>,
    pub sample_candidates: usize,
    pub sample_zentropi_total: u64,
    /// Population-reweighted expected would-be Zentropi calls per candidate scan.
    pub expected_calls_per_candidate: f64,
    /// `expected_calls_per_candidate` × population size, when a size was given.
    pub projected_network_total: Option<f64>,
    /// Strata that carry population weight but had no dry-run samples — their
    /// mean is treated as 0, biasing the estimate *down*. Surfaced so the gap is
    /// visible rather than silent.
    pub unsampled_weighted_strata: Vec<&'static str>,
}

/// Linear-interpolated percentile of a pre-sorted slice. `p` is in [0, 100].
fn percentile(sorted: &[u64], p: f64) -> f64 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0] as f64,
        n => {
            let rank = (p / 100.0) * (n - 1) as f64;
            let lo = rank.floor() as usize;
            let hi = rank.ceil() as usize;
            let frac = rank - lo as f64;
            sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * frac
        }
    }
}

fn mean(values: &[u64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<u64>() as f64 / values.len() as f64
}

/// Aggregate dry-run results into a network estimate.
///
/// `population_weights`, when supplied, maps stratum name → relative population
/// count (any positive scale; they're normalized internally). When `None`, the
/// sample's own stratum counts are used as weights. `population_size`, when
/// supplied, projects the reweighted per-candidate expectation to a network
/// total.
pub fn aggregate(
    results: &[CandidateDryRun],
    population_weights: Option<&BTreeMap<String, f64>>,
    population_size: Option<f64>,
) -> NetworkEstimate {
    // Bucket the per-candidate call counts by stratum.
    let mut by_stratum: BTreeMap<&'static str, Vec<u64>> = BTreeMap::new();
    for stratum in STRATA_ORDER {
        by_stratum.insert(stratum.as_str(), Vec::new());
    }
    for r in results {
        by_stratum
            .entry(r.stratum.as_str())
            .or_default()
            .push(r.counts.zentropi_calls);
    }

    let sample_total = results.len();
    let weight_sum: f64 = match population_weights {
        Some(w) => w.values().copied().filter(|v| *v > 0.0).sum(),
        None => sample_total as f64,
    };

    let mut per_stratum = Vec::with_capacity(STRATA_ORDER.len());
    let mut expected = 0.0;
    let mut unsampled_weighted_strata = Vec::new();

    for stratum in STRATA_ORDER {
        let key = stratum.as_str();
        let mut values = by_stratum.remove(key).unwrap_or_default();
        values.sort_unstable();

        let m = mean(&values);
        let share = if weight_sum > 0.0 {
            match population_weights {
                Some(w) => w.get(key).copied().unwrap_or(0.0).max(0.0) / weight_sum,
                None => values.len() as f64 / weight_sum,
            }
        } else {
            0.0
        };

        if share > 0.0 && values.is_empty() {
            unsampled_weighted_strata.push(key);
        }
        expected += share * m;

        per_stratum.push(StratumStats {
            stratum: key,
            sample_size: values.len(),
            mean: m,
            median: percentile(&values, 50.0),
            p90: percentile(&values, 90.0),
            p99: percentile(&values, 99.0),
            max: values.last().copied().unwrap_or(0),
            total: values.iter().sum(),
            population_share: share,
        });
    }

    NetworkEstimate {
        per_stratum,
        sample_candidates: sample_total,
        sample_zentropi_total: results.iter().map(|r| r.counts.zentropi_calls).sum(),
        expected_calls_per_candidate: expected,
        projected_network_total: population_size.map(|n| expected * n),
        unsampled_weighted_strata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::counting_scorer::CountSnapshot;

    fn result(stratum: EngagementStratum, zentropi: u64) -> CandidateDryRun {
        CandidateDryRun {
            did: "did:x".to_string(),
            handle: "x".to_string(),
            fanout_amplifiers: 0,
            stratum,
            amplifiers_scored: 0,
            followers_scored: 0,
            counts: CountSnapshot {
                posts_classified: 0,
                posts_cleared: 0,
                zentropi_calls: zentropi,
            },
        }
    }

    #[test]
    fn percentile_interpolates() {
        let v: Vec<u64> = (1..=10).collect(); // [1..10]
        assert_eq!(percentile(&v, 0.0), 1.0);
        assert_eq!(percentile(&v, 100.0), 10.0);
        // median of 1..10 → 5.5
        assert!((percentile(&v, 50.0) - 5.5).abs() < 1e-9);
    }

    #[test]
    fn percentile_handles_empty_and_single() {
        assert_eq!(percentile(&[], 50.0), 0.0);
        assert_eq!(percentile(&[42], 99.0), 42.0);
    }

    #[test]
    fn mean_of_empty_is_zero() {
        assert_eq!(mean(&[]), 0.0);
        assert_eq!(mean(&[2, 4, 6]), 4.0);
    }

    #[test]
    fn unweighted_expected_equals_overall_mean() {
        // Without population weights, expected/candidate = plain sample mean.
        let results = vec![
            result(EngagementStratum::None, 0),
            result(EngagementStratum::Low, 100),
            result(EngagementStratum::Viral, 5000),
        ];
        let est = aggregate(&results, None, None);
        let overall = (0.0 + 100.0 + 5000.0) / 3.0;
        assert!((est.expected_calls_per_candidate - overall).abs() < 1e-6);
        assert_eq!(est.sample_candidates, 3);
        assert_eq!(est.sample_zentropi_total, 5100);
    }

    #[test]
    fn reweighting_corrects_oversampled_tail() {
        // Sample oversamples viral 50/50, but population is 99% none, 1% viral.
        let results = vec![
            result(EngagementStratum::None, 0),
            result(EngagementStratum::Viral, 5000),
        ];
        let mut weights = BTreeMap::new();
        weights.insert("none".to_string(), 99.0);
        weights.insert("viral".to_string(), 1.0);
        let est = aggregate(&results, Some(&weights), None);
        // expected = 0.99*0 + 0.01*5000 = 50, not the naive 2500.
        assert!((est.expected_calls_per_candidate - 50.0).abs() < 1e-6);
    }

    #[test]
    fn projection_multiplies_by_population_size() {
        let results = vec![result(EngagementStratum::Low, 10)];
        let est = aggregate(&results, None, Some(1000.0));
        assert_eq!(est.projected_network_total, Some(10_000.0));
    }

    #[test]
    fn flags_weighted_strata_with_no_samples() {
        // Population says viral exists, but we sampled none of it.
        let results = vec![result(EngagementStratum::None, 0)];
        let mut weights = BTreeMap::new();
        weights.insert("none".to_string(), 90.0);
        weights.insert("viral".to_string(), 10.0);
        let est = aggregate(&results, Some(&weights), None);
        assert_eq!(est.unsampled_weighted_strata, vec!["viral"]);
    }

    #[test]
    fn empty_results_are_all_zero() {
        let est = aggregate(&[], None, Some(100.0));
        assert_eq!(est.sample_candidates, 0);
        assert_eq!(est.expected_calls_per_candidate, 0.0);
        assert_eq!(est.projected_network_total, Some(0.0));
        assert_eq!(est.per_stratum.len(), 5);
    }

    #[test]
    fn per_stratum_distribution_is_computed() {
        let results = vec![
            result(EngagementStratum::Medium, 10),
            result(EngagementStratum::Medium, 20),
            result(EngagementStratum::Medium, 30),
        ];
        let est = aggregate(&results, None, None);
        let med = est
            .per_stratum
            .iter()
            .find(|s| s.stratum == "medium")
            .unwrap();
        assert_eq!(med.sample_size, 3);
        assert_eq!(med.mean, 20.0);
        assert_eq!(med.median, 20.0);
        assert_eq!(med.max, 30);
        assert_eq!(med.total, 60);
    }
}
