//! Run one model over the pool:
//! 1. probe the backend's identity before any paid call;
//! 2. reuse only current verdicts;
//! 3. send the rest in the backend's own batch size (as production does);
//! 4. reject verdicts from an unexpected model or policy;
//! 5. bound each batch with a last-resort timeout, set ABOVE the backend's own
//!    so a job the backend still owns is never abandoned;
//! 6. stop at the reservation cap or when the backend's real cost meter trips.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

use super::cache::{append, load, save_identity, Identity, VerdictRecord};
use super::cost::{estimate_usd, Budget, Price};
use super::data::FixtureRow;
use crate::toxicity::classifier::{ItemOutcome, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

/// Above the RunPod client's own warm-up (180–300 s) and steady timeouts:
/// this guards against a hang, it must not race the backend's own timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStats {
    pub model: String,
    pub judged: usize,
    pub cached: usize,
    pub errors: usize,
    pub stopped_at_cap: bool,
}

pub async fn run_model(
    model: &str,
    classifier: &dyn ToxicityClassifier,
    pool: &[FixtureRow],
    dir: &Path,
    price: &Price,
    budget: &mut Budget,
    timeout: Duration,
) -> Result<RunStats> {
    classifier
        .probe_identity()
        .await
        .with_context(|| format!("{model}: identity probe failed — not running it"))?;
    let who = Identity {
        model_id: classifier.model_id(),
        policy_version: classifier.policy_version(),
    };
    save_identity(dir, model, who)?;

    let cached = load(dir, model)?;
    let mut stats = RunStats {
        model: model.to_string(),
        judged: 0,
        cached: 0,
        errors: 0,
        stopped_at_cap: false,
    };
    let todo: Vec<&FixtureRow> = pool
        .iter()
        .filter(|row| {
            let hit = cached
                .get(&row.id)
                .is_some_and(|r| r.reusable(&row.content, who));
            if hit {
                stats.cached += 1;
            }
            !hit
        })
        .collect();

    for chunk in todo.chunks(classifier.max_batch_size().max(1)) {
        let ests: Vec<f64> = chunk
            .iter()
            .map(|r| estimate_usd(price, r.content.chars().count()))
            .collect();
        if !budget.try_spend(ests.iter().sum()) {
            stats.stopped_at_cap = true;
            break;
        }
        let contents: Vec<String> = chunk.iter().map(|r| r.content.clone()).collect();
        let mut records = Vec::with_capacity(chunk.len());
        match tokio::time::timeout(timeout, classifier.classify_batch(&contents)).await {
            Ok(Ok(outcomes)) if outcomes.len() == chunk.len() => {
                for ((row, est), outcome) in chunk.iter().zip(&ests).zip(outcomes) {
                    records.push(match outcome {
                        ItemOutcome::Verdict(v) if v.model_id == who.model_id && v.policy_version == who.policy_version => {
                            VerdictRecord::from_verdict(&row.id, model, &row.content, &v, *est)
                        }
                        ItemOutcome::Verdict(v) => {
                            let detail = format!(
                                "identity mismatch: backend reported {}/{}, deployment declares {}/{}",
                                v.model_id, v.policy_version, who.model_id, who.policy_version
                            );
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                        ItemOutcome::Error(detail) => {
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                    });
                }
            }
            Ok(Ok(outcomes)) => {
                let detail = format!(
                    "backend returned {} outcomes for {} posts",
                    outcomes.len(),
                    chunk.len()
                );
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(
                        &row.id,
                        model,
                        &row.content,
                        who,
                        &detail,
                        *est,
                    ));
                }
            }
            // The backend's REAL cost meter tripped: stop, record nothing for
            // this batch (it was refused, not judged).
            Ok(Err(e)) if e.downcast_ref::<CostCeilingExceeded>().is_some() => {
                stats.stopped_at_cap = true;
                break;
            }
            Ok(Err(e)) => {
                let detail = format!("{e:#}");
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(
                        &row.id,
                        model,
                        &row.content,
                        who,
                        &detail,
                        *est,
                    ));
                }
            }
            Err(_) => {
                let detail = format!("timed out after {} s", timeout.as_secs_f64());
                for (row, est) in chunk.iter().zip(&ests) {
                    records.push(VerdictRecord::from_error(
                        &row.id,
                        model,
                        &row.content,
                        who,
                        &detail,
                        *est,
                    ));
                }
            }
        }
        for rec in &records {
            if rec.called_toxic.is_some() {
                stats.judged += 1;
            } else {
                stats.errors += 1;
            }
            // Written as each batch returns, so a crash or a cap keeps every paid verdict.
            append(dir, rec)?;
        }
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use crate::toxicity::cost_meter::CostCeilingExceeded;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        fail_on: Option<&'static str>,
        hang_on: Option<&'static str>,
        capped_on: Option<&'static str>,
        policy: &'static str,
        reported_model: &'static str,
        probe_fails: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail_on: None,
                hang_on: None,
                capped_on: None,
                policy: "p-1",
                reported_model: "fake-1",
                probe_fails: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl ToxicityClassifier for Fake {
        async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.hang_on.is_some_and(|h| content.contains(h)) {
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
            if self.capped_on.is_some_and(|c| content.contains(c)) {
                return Err(CostCeilingExceeded {
                    est_cents: 100,
                    ceiling_cents: 100,
                }
                .into());
            }
            if self.fail_on.is_some_and(|f| content.contains(f)) {
                anyhow::bail!("upstream 503");
            }
            Ok(ClassifierVerdict {
                toxic_token: content.contains("bad"),
                confidence: 0.9,
                latency_ms: 5,
                model_id: self.reported_model.into(),
                policy_version: self.policy.into(),
            })
        }
        async fn probe_identity(&self) -> anyhow::Result<()> {
            if self.probe_fails {
                anyhow::bail!("endpoint serves policy-old, deployment declares p-1");
            }
            Ok(())
        }
        fn name(&self) -> &'static str {
            "fake"
        }
        fn model_id(&self) -> &'static str {
            "fake-1"
        }
        fn policy_version(&self) -> &'static str {
            self.policy
        }
        fn threshold(&self) -> f32 {
            0.5
        }
    }

    fn row(id: &str, content: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": content})).unwrap()
    }

    const T: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn judges_each_post_once_and_reuses_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let model = Fake::new();
        let mut budget = Budget::new(1.0);
        let s = run_model(
            "m",
            &model,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.cached), (2, 0));
        let s = run_model(
            "m",
            &model,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.cached), (0, 2), "nothing re-paid");
        assert_eq!(model.calls.load(Ordering::SeqCst), 2);
        assert!(
            crate::cli::eval_classifiers::cache::load_identity(dir.path(), "m")
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn edited_posts_and_new_policies_are_rejudged() {
        let dir = tempfile::tempdir().unwrap();
        let mut budget = Budget::new(1.0);
        run_model(
            "m",
            &Fake::new(),
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        let edited = Fake::new();
        run_model(
            "m",
            &edited,
            &[row("a", "bad, edited")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(edited.calls.load(Ordering::SeqCst), 1);
        let repoliced = Fake {
            policy: "p-2",
            ..Fake::new()
        };
        run_model(
            "m",
            &repoliced,
            &[row("a", "bad, edited")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(repoliced.calls.load(Ordering::SeqCst), 1);
    }

    /// Codex review, round 2: a backend that fails its identity probe is not
    /// paid for at all.
    #[tokio::test]
    async fn a_failed_identity_probe_stops_before_any_call() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake {
            probe_fails: true,
            ..Fake::new()
        };
        let mut budget = Budget::new(1.0);
        let err = run_model(
            "m",
            &model,
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap_err();
        assert!(format!("{err:#}").contains("identity"), "{err:#}");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    }

    /// Codex review, round 2: a verdict from an unexpected model is an error.
    #[tokio::test]
    async fn a_verdict_from_a_foreign_model_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake {
            reported_model: "someone-else",
            ..Fake::new()
        };
        let mut budget = Budget::new(1.0);
        let s = run_model(
            "m",
            &model,
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.errors), (0, 1));
        let rec = &load(dir.path(), "m").unwrap()["a"];
        assert_eq!(rec.called_toxic, None);
        assert!(rec.error.as_deref().unwrap().contains("identity mismatch"));
    }

    #[tokio::test]
    async fn an_error_is_recorded_as_an_error_and_retried_next_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad 503")];
        let mut budget = Budget::new(1.0);
        let s = run_model(
            "m",
            &Fake {
                fail_on: Some("503"),
                ..Fake::new()
            },
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(s.errors, 1);
        assert_eq!(
            load(dir.path(), "m").unwrap()["a"].called_toxic,
            None,
            "an error, never clean"
        );
        let ok = Fake::new();
        run_model(
            "m",
            &ok,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(ok.calls.load(Ordering::SeqCst), 1, "the error was retried");
    }

    #[tokio::test]
    async fn a_hung_call_times_out_as_an_error_and_the_run_continues() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "hang"), row("b", "bad")];
        let model = Fake {
            hang_on: Some("hang"),
            ..Fake::new()
        };
        let mut budget = Budget::new(1.0);
        let started = std::time::Instant::now();
        let s = run_model(
            "m",
            &model,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            Duration::from_millis(50),
        )
        .await
        .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not wait out the 30 s hang"
        );
        assert_eq!((s.errors, s.judged), (1, 1));
        assert!(load(dir.path(), "m").unwrap()["a"]
            .error
            .as_deref()
            .unwrap()
            .contains("timed out"));
    }

    #[tokio::test]
    async fn the_reservation_cap_stops_the_run_and_keeps_what_was_paid_for() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "x"), row("b", "y"), row("c", "z")];
        let price = Price {
            usd_per_post: Some(0.4),
            usd_per_million_input_tokens: None,
            overhead_chars: 0,
        };
        let mut budget = Budget::new(1.0);
        let s = run_model("m", &Fake::new(), &pool, dir.path(), &price, &mut budget, T)
            .await
            .unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 2);
        assert_eq!(
            load(dir.path(), "m").unwrap().len(),
            2,
            "both paid verdicts kept"
        );
    }

    /// Codex review, round 2: the backend's REAL cost meter tripping stops the
    /// run too — the cap is not only an estimate.
    #[tokio::test]
    async fn the_backends_real_cost_ceiling_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "capped"), row("c", "bad")];
        let model = Fake {
            capped_on: Some("capped"),
            ..Fake::new()
        };
        let mut budget = Budget::new(1.0);
        let s = run_model(
            "m",
            &model,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(s.judged, 1);
        assert_eq!(model.calls.load(Ordering::SeqCst), 2, "c was never sent");
    }
}
