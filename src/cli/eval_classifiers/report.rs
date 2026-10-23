//! The report. Headline rates come from HOLDOUT labels only; comparisons are
//! decided per class by the exact paired test in metrics.rs; every rate shows
//! its 95 % Wilson interval as description — never a blended score.

use std::collections::HashMap;
use std::fmt::Write;

use super::cache::VerdictRecord;
use super::cost::{estimate_usd, Price};
use super::data::{Answer, Source, Truth};
use super::metrics::{model_metrics, paired, Comparison, ModelMetrics, Paired, Rate, MIN_CLASS_N};

#[derive(Debug, Clone)]
pub struct ModelReport {
    pub name: String,
    pub calls: HashMap<String, Option<bool>>,
    pub holdout: ModelMetrics,
    pub diagnostic: ModelMetrics,
    pub usd_per_1k: Option<f64>,
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
}

pub fn labels_of(answers: &HashMap<String, Answer>, source: Source) -> HashMap<String, Truth> {
    answers
        .iter()
        .filter(|(_, a)| a.source == source)
        .map(|(id, a)| (id.clone(), a.truth))
        .collect()
}

/// Nearest-rank percentile.
fn percentile(sorted: &[u32], pct: usize) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let idx = (sorted.len() * pct).div_ceil(100).saturating_sub(1);
    sorted.get(idx).copied()
}

pub fn build(
    models: &[(String, HashMap<String, VerdictRecord>)],
    answers: &HashMap<String, Answer>,
    prices: &HashMap<String, Price>,
) -> Vec<ModelReport> {
    let holdout = labels_of(answers, Source::Holdout);
    let diagnostic = labels_of(answers, Source::Diagnostic);
    models
        .iter()
        .map(|(name, recs)| {
            let calls: HashMap<String, Option<bool>> = recs
                .iter()
                .map(|(id, r)| (id.clone(), r.called_toxic))
                .collect();
            // Latency: successful calls only — an error's latency is 0 (or a
            // timeout), which says nothing about how fast the model answers.
            let ok: Vec<&VerdictRecord> =
                recs.values().filter(|r| r.called_toxic.is_some()).collect();
            let mut lat: Vec<u32> = ok.iter().map(|r| r.latency_ms).collect();
            lat.sort_unstable();
            // Cost: EVERY attempted call, errors included (timeouts, request
            // errors, an unexpected identity). A failed call is still paid
            // for, so counting only successes showed an all-error paid model
            // as "unpriced" and understated a token-priced model that fails
            // on long posts (Codex review #2, item 4). `recs` already holds
            // the newest current-identity record per post, errors included
            // (that is what `cache::current_for` returns), so no other
            // selector is needed.
            let usd_per_1k = prices.get(name).filter(|_| !recs.is_empty()).map(|p| {
                let mean_chars = recs.values().map(|r| r.input_chars).sum::<usize>() / recs.len();
                estimate_usd(p, mean_chars) * 1000.0
            });
            ModelReport {
                name: name.clone(),
                holdout: model_metrics(name, &calls, &holdout),
                diagnostic: model_metrics(name, &calls, &diagnostic),
                calls,
                usd_per_1k,
                p50_ms: percentile(&lat, 50),
                p95_ms: percentile(&lat, 95),
            }
        })
        .collect()
}

pub fn vs_baseline(
    baseline: &ModelReport,
    candidate: &ModelReport,
    answers: &HashMap<String, Answer>,
) -> Paired {
    paired(
        &baseline.name,
        &baseline.calls,
        &candidate.name,
        &candidate.calls,
        &labels_of(answers, Source::Holdout),
    )
}

/// The line saying how many labels were dropped because their post's text
/// changed since labelling (Codex fix 2). Empty when none were: silence
/// means every label is in use.
pub fn stale_labels_note(stale: usize) -> String {
    if stale == 0 {
        return String::new();
    }
    format!(
        "{stale} labels ignored — post text changed since labelling; relabel (run `charcoal eval-classifiers label`)\n"
    )
}

/// The line saying how many labels were dropped because their post is no
/// longer in pool.jsonl (Codex review #2, item 2). Empty when none were.
pub fn removed_labels_note(removed: usize) -> String {
    if removed == 0 {
        return String::new();
    }
    format!(
        "{removed} labels ignored — post no longer in the pool (kept in labels.jsonl)
"
    )
}

pub(crate) fn rate_cell(r: &Rate) -> String {
    match (r.value(), r.wilson95()) {
        (Some(v), Some((lo, hi))) => {
            format!(
                "{:>5.1}% [{:.0}–{:.0}] ({}/{})",
                v * 100.0,
                lo * 100.0,
                hi * 100.0,
                r.hits,
                r.n
            )
        }
        _ => "no labels".to_string(),
    }
}

fn word(c: Comparison) -> String {
    match c {
        Comparison::Better => "better".to_string(),
        Comparison::Worse => "worse".to_string(),
        Comparison::TooClose => "too close to call — label more".to_string(),
        Comparison::NotEnoughLabels => {
            format!("not enough holdout labels (need {MIN_CLASS_N} per class) — label more")
        }
        Comparison::LowCoverage => "no verdict — coverage below 90 %".to_string(),
    }
}

pub fn render(
    reports: &[ModelReport],
    answers: &HashMap<String, Answer>,
    baseline: &str,
) -> String {
    let mut s = String::new();
    let count = |src: Source| answers.values().filter(|a| a.source == src).count();
    let _ = writeln!(
        s,
        "Labels: {} holdout (scored), {} diagnostic, {} seed (not scored)\n",
        count(Source::Holdout),
        count(Source::Diagnostic),
        count(Source::Seed)
    );
    let _ = writeln!(s, "HOLDOUT — the headline numbers");
    let _ = writeln!(
        s,
        "{:<14} {:<28} {:<28} {:>9} {:>6} {:>10} {:>12}",
        "model",
        "false alarms (clean→toxic)",
        "misses (toxic→clean)",
        "coverage",
        "errors",
        "$/1k posts",
        "p50/p95 ms"
    );
    for r in reports {
        let m = &r.holdout;
        let cov = m
            .coverage()
            .map_or("-".to_string(), |c| format!("{:.0}%", c * 100.0));
        let cost = r
            .usd_per_1k
            .map_or("unpriced".to_string(), |c| format!("{c:.4}"));
        let lat = match (r.p50_ms, r.p95_ms) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => "-".to_string(),
        };
        let _ = writeln!(
            s,
            "{:<14} {:<28} {:<28} {:>9} {:>6} {:>10} {:>12}",
            r.name,
            rate_cell(&m.false_alarm),
            rate_cell(&m.miss),
            cov,
            m.errors,
            cost,
            lat
        );
    }
    match reports.iter().find(|r| r.name == baseline) {
        Some(base) => {
            let _ = writeln!(
                s,
                "\nCompared with {baseline} (exact paired test, holdout, α = 0.05):"
            );
            for r in reports.iter().filter(|r| r.name != baseline) {
                let p = vs_baseline(base, r, answers);
                let _ = writeln!(
                    s,
                    "  {}: false alarms {}; misses {}",
                    r.name,
                    word(p.false_alarm),
                    word(p.miss)
                );
            }
        }
        None => {
            let _ = writeln!(
                s,
                "\n(baseline {baseline:?} has no current verdicts — run it first)"
            );
        }
    }
    let _ = writeln!(
        s,
        "\nWHERE MODELS DIFFER — diagnostic labels, picked by disagreement: NOT population rates"
    );
    for r in reports {
        let m = &r.diagnostic;
        let _ = writeln!(
            s,
            "  {:<14} wrong on {}/{} diagnostic posts",
            r.name,
            m.false_alarm.hits + m.miss.hits,
            m.judged
        );
    }
    s
}

/// S1's $0 is Zentropi's free development tier, not a production price
/// (spec §4.5), so say so wherever it shows up.
pub fn dev_pricing_note(models: &[&str]) -> String {
    if !models.iter().any(|m| m.starts_with("s1-")) {
        return String::new();
    }
    "note: s1-* shows $0 because Zentropi's hosted API is free for development; production pricing is unpublished\n".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;
    use crate::cli::eval_classifiers::metrics::Comparison;

    /// 40 toxic + 40 clean posts, all HOLDOUT.
    fn holdout() -> HashMap<String, Answer> {
        (0..80)
            .map(|i| {
                (
                    format!("p{i}"),
                    Answer {
                        truth: if i < 40 { Truth::Toxic } else { Truth::Clean },
                        source: Source::Holdout,
                    },
                )
            })
            .collect()
    }

    fn model(
        name: &str,
        decide: impl Fn(usize) -> Option<bool>,
    ) -> (String, HashMap<String, VerdictRecord>) {
        let recs = (0..80)
            .map(|i| {
                let id = format!("p{i}");
                let call = decide(i);
                let r = VerdictRecord {
                    id: id.clone(),
                    model: name.into(),
                    content_hash: String::new(),
                    model_id: name.into(),
                    policy_version: "p".into(),
                    called_toxic: call,
                    error: call.is_none().then(|| "timed out".into()),
                    latency_ms: 10 + i as u32,
                    input_chars: 400,
                    est_usd: 0.0,
                    reasons: None,
                };
                (id, r)
            })
            .collect();
        (name.to_string(), recs)
    }

    /// Good baseline: misses p0,p1; false alarms on p40,p41.
    fn good(i: usize) -> Option<bool> {
        Some((2..40).contains(&i) || (40..42).contains(&i))
    }

    #[test]
    fn deliberately_bad_models_are_caught() {
        let reports = build(
            &[
                model("cope-b", good),
                model("always-clean", |_| Some(false)),
                model("coin-flip", |i| Some(i % 2 == 0)),
            ],
            &holdout(),
            &HashMap::new(),
        );
        assert_eq!(
            vs_baseline(&reports[0], &reports[1], &holdout()).miss,
            Comparison::Worse,
            "always-clean misses everything"
        );
        let p = vs_baseline(&reports[0], &reports[2], &holdout());
        assert_eq!(
            (p.false_alarm, p.miss),
            (Comparison::Worse, Comparison::Worse),
            "coin-flip loses on both"
        );
        let text = render(&reports, &holdout(), "cope-b");
        // The coin-flip line also says "misses worse", so check the line that
        // belongs to the always-clean model specifically.
        let line = text
            .lines()
            .find(|l| l.contains("always-clean: false alarms"))
            .unwrap_or_else(|| panic!("no always-clean comparison line in:\n{text}"));
        assert!(line.contains("misses worse"), "{line}");
    }

    #[test]
    fn a_better_candidate_is_reported_as_better() {
        // Arguments reversed: the bad model is the baseline, so a swap of
        // better/worse anywhere in the report path would show up here.
        let reports = build(
            &[
                model("always-clean", |_| Some(false)),
                model("cope-b", good),
            ],
            &holdout(),
            &HashMap::new(),
        );
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!(
            p.miss,
            Comparison::Better,
            "good catches what always-clean misses"
        );
        // always-clean never false-alarms; good does on 2 posts -> 0 vs 2
        // discordant pairs, p = 0.5, nowhere near significant.
        assert_eq!(p.false_alarm, Comparison::TooClose);
        let text = render(&reports, &holdout(), "always-clean");
        let line = text
            .lines()
            .find(|l| l.contains("cope-b: false alarms"))
            .unwrap_or_else(|| panic!("no cope-b comparison line in:\n{text}"));
        assert!(line.contains("misses better"), "{line}");
    }

    #[test]
    fn close_models_are_too_close_to_call() {
        let reports = build(
            &[
                model("cope-b", |i| Some(i < 39 || i == 79)),
                model("other", |i| Some(i < 38 || i == 79)),
            ],
            &holdout(),
            &HashMap::new(),
        );
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!(
            (p.false_alarm, p.miss),
            (Comparison::TooClose, Comparison::TooClose)
        );
        assert!(render(&reports, &holdout(), "cope-b").contains("too close to call — label more"));
    }

    #[test]
    fn a_model_erroring_on_hard_posts_gets_no_verdict_on_that_class() {
        let dodger = model("dodger", |i| if i < 10 { None } else { Some(i < 40) });
        let reports = build(
            &[model("cope-b", good), dodger],
            &holdout(),
            &HashMap::new(),
        );
        let p = vs_baseline(&reports[0], &reports[1], &holdout());
        assert_eq!(p.miss, Comparison::LowCoverage, "30/40 toxic answered");
        assert!(render(&reports, &holdout(), "cope-b")
            .contains("misses no verdict — coverage below 90 %"));
    }

    #[test]
    fn too_few_holdout_labels_say_so() {
        let few: HashMap<String, Answer> = holdout()
            .into_iter()
            .filter(|(id, _)| id[1..].parse::<usize>().unwrap() % 4 == 0)
            .collect();
        let reports = build(
            &[
                model("cope-b", good),
                model("always-clean", |_| Some(false)),
            ],
            &few,
            &HashMap::new(),
        );
        assert_eq!(
            vs_baseline(&reports[0], &reports[1], &few).miss,
            Comparison::NotEnoughLabels,
            "10 toxic < 20"
        );
        assert!(render(&reports, &few, "cope-b").contains("not enough holdout labels"));
    }

    #[test]
    fn diagnostic_and_seed_labels_do_not_move_the_headline() {
        let mut answers = holdout();
        for i in 80..100 {
            answers.insert(
                format!("p{i}"),
                Answer {
                    truth: Truth::Toxic,
                    source: Source::Diagnostic,
                },
            );
        }
        for i in 100..105 {
            answers.insert(
                format!("p{i}"),
                Answer {
                    truth: Truth::Clean,
                    source: Source::Seed,
                },
            );
        }
        let (name, mut recs) = model("cope-b", good);
        for i in 80..105 {
            let id = format!("p{i}");
            let mut r = recs["p0"].clone();
            r.id = id.clone();
            r.called_toxic = Some(i >= 100);
            recs.insert(id, r);
        }
        let r = &build(&[(name, recs)], &answers, &HashMap::new())[0];
        assert_eq!(
            (r.holdout.miss.hits, r.holdout.miss.n),
            (2, 40),
            "headline = holdout only"
        );
        assert_eq!((r.diagnostic.miss.hits, r.diagnostic.miss.n), (20, 20));
        // Seed calls are all "toxic" on clean-labelled posts, so a seed leak
        // would surface as false alarms. Holdout: only p40,p41 (2 of 40).
        assert_eq!(
            (r.holdout.false_alarm.hits, r.holdout.false_alarm.n),
            (2, 40),
            "seed false alarms must not leak into the headline"
        );
        // Diagnostic labels are all toxic: no clean posts, so no rate at all.
        assert_eq!(r.diagnostic.false_alarm.n, 0);
        assert_eq!(r.diagnostic.false_alarm.value(), None);
        assert_eq!((r.holdout.labelled, r.diagnostic.labelled), (80, 20));
    }

    /// Codex fix 2: labels dropped because their post changed are counted
    /// out loud, with what to do about it; none dropped, nothing printed.
    #[test]
    fn stale_labels_are_counted_with_the_fix() {
        let line = stale_labels_note(3);
        assert!(
            line.contains("3 labels ignored") && line.contains("relabel"),
            "{line}"
        );
        assert_eq!(stale_labels_note(0), "");
    }

    /// Codex review #2, item 2: labels for posts removed from the pool are
    /// counted out loud too; none, nothing printed.
    #[test]
    fn removed_labels_are_counted() {
        let line = removed_labels_note(2);
        assert!(
            line.contains("2 labels ignored") && line.contains("no longer in the pool"),
            "{line}"
        );
        assert_eq!(removed_labels_note(0), "");
    }

    fn per_token(name: &str) -> HashMap<String, Price> {
        [(
            name.to_string(),
            Price {
                usd_per_post: None,
                usd_per_million_input_tokens: Some(0.042),
                overhead_chars: 0,
            },
        )]
        .into()
    }

    /// Codex review #2, item 4: a failed call is still paid for. A paid model
    /// whose every call failed has a cost, not "unpriced".
    #[test]
    fn an_all_error_paid_model_still_shows_a_cost() {
        let r = &build(&[model("jev", |_| None)], &holdout(), &per_token("jev"))[0];
        // 400 chars ≈ 100 tokens, same as a model that succeeded.
        assert!((r.usd_per_1k.unwrap() - 0.0042).abs() < 1e-9, "{r:?}");
        assert_eq!((r.p50_ms, r.p95_ms), (None, None), "no successful latency");
        let text = render(std::slice::from_ref(r), &holdout(), "jev");
        assert!(!text.contains("unpriced"), "{text}");
    }

    /// Item 4: a token-priced model that fails on LONG posts must have those
    /// failed posts in its cost, not just the short ones it answered.
    #[test]
    fn a_mixed_models_cost_counts_its_failed_calls() {
        let (name, mut recs) = model("jev", |i| (i < 40).then_some(true));
        for r in recs.values_mut().filter(|r| r.called_toxic.is_none()) {
            r.input_chars = 4000;
        }
        let r = &build(&[(name, recs)], &holdout(), &per_token("jev"))[0];
        // Mean = (40 × 400 + 40 × 4000) / 80 = 2200 chars ≈ 550 tokens;
        // 1,000 posts ≈ 550k tokens × $0.042/M = $0.0231.
        assert!((r.usd_per_1k.unwrap() - 0.0231).abs() < 1e-9, "{r:?}");
        // Latency stays successes-only: 10..=49 ms over the first 40 posts.
        assert_eq!((r.p50_ms, r.p95_ms), (Some(29), Some(47)));
    }

    #[test]
    fn cost_and_latency_columns() {
        let prices: HashMap<String, Price> = [(
            "jev".to_string(),
            Price {
                usd_per_post: None,
                usd_per_million_input_tokens: Some(0.042),
                overhead_chars: 0,
            },
        )]
        .into();
        let r = &build(&[model("jev", |_| Some(true))], &holdout(), &prices)[0];
        // 400 chars ≈ 100 tokens; 1,000 posts ≈ 100k tokens ≈ $0.0042.
        assert!((r.usd_per_1k.unwrap() - 0.0042).abs() < 1e-9);
        // Latencies are 10..=89 ms: nearest-rank p50 = 49, p95 = 85.
        assert_eq!((r.p50_ms, r.p95_ms), (Some(49), Some(85)));
        assert_eq!(
            build(&[model("x", |_| Some(true))], &holdout(), &HashMap::new())[0].usd_per_1k,
            None
        );
    }
}
