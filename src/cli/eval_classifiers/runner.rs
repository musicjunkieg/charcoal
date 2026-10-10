//! Run one model over the pool:
//! 1. reuse only current verdicts (by the DECLARED identity, no network), and
//!    return without waking the backend if nothing is left or the budget
//!    cannot cover the first batch;
//! 2. probe the backend's identity before any paid call;
//! 3. send the rest in the backend's own batch size (as production does);
//! 4. reject verdicts from an unexpected model or policy;
//! 5. bound each batch with a last-resort timeout, set ABOVE the backend's own
//!    so a job the backend still owns is never abandoned;
//! 6. stop at the reservation cap or when the backend's real cost meter trips.

use anyhow::{Context, Result};
use std::path::Path;
use std::time::Duration;

use super::cache::{
    append, latest_match, load_all, load_identity, save_identity, Identity, VerdictRecord,
};
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
    // The DECLARED identity: local, no network. Deciding what needs judging
    // with it lets an all-cached run finish without waking the backend.
    let who = Identity {
        model_id: classifier.model_id(),
        policy_version: classifier.policy_version(),
    };

    // The FULL history, not just each id's last record: a newer record under
    // another identity must not hide a verdict already paid for under this
    // one (Codex fix 1). Reusable = the newest record for this text and
    // identity is a success; if that newest one is an error, re-judge.
    let cached = load_all(dir, model)?;
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
            let hit = cached.get(&row.id).is_some_and(|h| {
                latest_match(h, &row.content, who).is_some_and(|r| r.reusable(&row.content, who))
            });
            if hit {
                stats.cached += 1;
            }
            !hit
        })
        .collect();

    // Nothing to judge: return before the probe. On RunPod a probe is a real
    // GPU wake-up (cold start + ~60 s billed idle) that no meter can stop,
    // because nothing has been spent yet. The identity file is still brought
    // in line, though: it records the identity whose verdicts are current.
    // Every pool post's latest record is a successful verdict that passed the
    // per-verdict model/policy check under `who` when it was recorded, which
    // is stronger evidence than a probe. Without this, an earlier run under
    // another policy that probed but recorded nothing leaves the file
    // pointing at that policy, and report/label would drop this model.
    if todo.is_empty() {
        let recorded = load_identity(dir, model)?;
        let matches = recorded
            .as_ref()
            .is_some_and(|r| r.model_id == who.model_id && r.policy_version == who.policy_version);
        if !matches {
            save_identity(dir, model, who)?;
        }
        return Ok(stats);
    }
    let batch = classifier.max_batch_size().max(1);
    // Would the first batch fit? Asked WITHOUT reserving: if not, the probe
    // would be the run's only spend, so stop before it.
    let first_batch_usd: f64 = todo
        .iter()
        .take(batch)
        .map(|r| estimate_usd(price, r.content.chars().count()))
        .sum();
    if !budget.can_afford(first_batch_usd) {
        stats.stopped_at_cap = true;
        return Ok(stats);
    }

    // Only now is waking the backend worth paying for.
    classifier
        .probe_identity()
        .await
        .with_context(|| format!("{model}: identity probe failed — not running it"))?;
    save_identity(dir, model, who)?;

    for chunk in todo.chunks(batch) {
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
                // Positional pairing: the runner trusts the backend to return
                // outcomes in the order the posts were sent (the trait carries
                // no ids), as the classifier trait's contract requires.
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
    use crate::cli::eval_classifiers::cache::load;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use crate::toxicity::cost_meter::CostCeilingExceeded;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fake {
        calls: AtomicUsize,
        /// Counts identity probes: each one is a real GPU wake-up on RunPod.
        probes: AtomicUsize,
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
                probes: AtomicUsize::new(0),
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
            self.probes.fetch_add(1, Ordering::SeqCst);
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

    /// Final round 2, item 1: a P2 run that probes (identity file -> P2) but
    /// records nothing must not strand a later all-cached P1 run.
    #[tokio::test]
    async fn an_all_cached_run_restores_the_identity_whose_verdicts_are_current() {
        use crate::cli::eval_classifiers::cache::current_for;
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let mut budget = Budget::new(1.0);
        let p1 = Fake::new();
        run_model(
            "m",
            &p1,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        // P2 probes fine, then the cost ceiling trips on the first batch.
        let p2 = Fake {
            policy: "p-2",
            capped_on: Some(""),
            ..Fake::new()
        };
        run_model(
            "m",
            &p2,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(p2.probes.load(Ordering::SeqCst), 1, "P2 did probe");
        // Back to P1: everything is cached, so no probe -- yet the identity
        // file (now P2) must be repaired.
        let p1b = Fake::new();
        let s = run_model(
            "m",
            &p1b,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.cached), (0, 2));
        assert_eq!(p1b.probes.load(Ordering::SeqCst), 0, "no GPU wake-up");
        assert!(
            !current_for(dir.path(), "m", &pool).unwrap().is_empty(),
            "report/label must still see the P1 verdicts"
        );
    }

    /// Codex fix 1: P1 -> P2 -> P1. The P2 records are newer, but they must
    /// not hide the P1 verdicts already paid for: going back to P1 re-pays
    /// nothing, wakes nothing, and report/label see the P1 verdicts.
    #[tokio::test]
    async fn returning_to_an_earlier_identity_reuses_its_verdicts() {
        use crate::cli::eval_classifiers::cache::current_for;
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let mut budget = Budget::new(1.0);
        for policy in ["p-1", "p-2"] {
            let m = Fake {
                policy,
                ..Fake::new()
            };
            let s = run_model(
                "m",
                &m,
                &pool,
                dir.path(),
                &Price::default(),
                &mut budget,
                T,
            )
            .await
            .unwrap();
            assert_eq!(s.judged, 2, "{policy} judges both posts");
        }
        let back = Fake::new();
        let s = run_model(
            "m",
            &back,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.cached), (0, 2));
        assert_eq!(back.probes.load(Ordering::SeqCst), 0, "no GPU wake-up");
        assert_eq!(back.calls.load(Ordering::SeqCst), 0, "nothing re-paid");
        let cur = current_for(dir.path(), "m", &pool).unwrap();
        assert_eq!(cur.len(), 2);
        assert!(cur.values().all(|r| r.policy_version == "p-1"), "{cur:?}");
        assert_eq!(cur["a"].called_toxic, Some(true));
        assert_eq!(cur["b"].called_toxic, Some(false));
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

    /// Final review fix 1: a probe is a paid GPU wake-up. When every post is
    /// already answered there is nothing to pay for, so nothing is woken.
    #[tokio::test]
    async fn an_all_cached_run_never_wakes_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let mut budget = Budget::new(1.0);
        let first = Fake::new();
        run_model(
            "m",
            &first,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!(first.probes.load(Ordering::SeqCst), 1);
        let again = Fake::new();
        let s = run_model(
            "m",
            &again,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert_eq!((s.judged, s.cached, s.errors), (0, 2, 0));
        assert!(!s.stopped_at_cap);
        assert_eq!(again.probes.load(Ordering::SeqCst), 0, "no wake-up");
        assert_eq!(again.calls.load(Ordering::SeqCst), 0, "no classify call");
    }

    /// Final review fix 1: if even the first batch does not fit the budget,
    /// stop before the probe — the probe itself would be the only spend.
    #[tokio::test]
    async fn a_cap_below_one_batch_stops_before_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let price = Price {
            usd_per_post: Some(0.4),
            usd_per_million_input_tokens: None,
            overhead_chars: 0,
        };
        let mut budget = Budget::new(0.1);
        let model = Fake::new();
        let s = run_model(
            "m",
            &model,
            &[row("a", "x")],
            dir.path(),
            &price,
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(model.probes.load(Ordering::SeqCst), 0, "no wake-up");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
        assert_eq!(budget.spent(), 0.0, "nothing reserved");
        assert!(
            crate::cli::eval_classifiers::cache::load_identity(dir.path(), "m")
                .unwrap()
                .is_none(),
            "an unverified identity is never recorded"
        );
    }

    /// What one slot of a scripted batch answers. Keyed by post text.
    #[derive(Clone, Copy)]
    enum Slot {
        Ok,
        Undecodable,
        ForeignModel,
        ForeignPolicy,
    }

    /// A backend with CoPE-B's shape: four posts per request, its own
    /// `classify_batch`, and per-slot outcomes.
    struct BatchFake {
        batches: AtomicUsize,
        /// Post text containing this word → that slot's behaviour.
        script: Vec<(&'static str, Slot)>,
        /// Return one outcome fewer than posts sent.
        short: bool,
    }

    impl BatchFake {
        fn new(script: Vec<(&'static str, Slot)>) -> Self {
            Self {
                batches: AtomicUsize::new(0),
                script,
                short: false,
            }
        }
    }

    #[async_trait::async_trait]
    impl ToxicityClassifier for BatchFake {
        async fn classify(&self, _content: &str) -> anyhow::Result<ClassifierVerdict> {
            anyhow::bail!("the batch path must be used")
        }
        async fn classify_batch(&self, contents: &[String]) -> anyhow::Result<Vec<ItemOutcome>> {
            self.batches.fetch_add(1, Ordering::SeqCst);
            let mut out: Vec<ItemOutcome> = contents
                .iter()
                .map(|c| {
                    let slot = self
                        .script
                        .iter()
                        .find(|(w, _)| c.contains(w))
                        .map_or(Slot::Ok, |(_, s)| *s);
                    let (model_id, policy) = match slot {
                        Slot::Undecodable => {
                            return ItemOutcome::Error("slot did not decode".into())
                        }
                        Slot::Ok => ("fake-1", "p-1"),
                        Slot::ForeignModel => ("someone-else", "p-1"),
                        Slot::ForeignPolicy => ("fake-1", "p-old"),
                    };
                    ItemOutcome::Verdict(ClassifierVerdict {
                        toxic_token: c.contains("bad"),
                        confidence: 0.9,
                        latency_ms: 5,
                        model_id: model_id.into(),
                        policy_version: policy.into(),
                    })
                })
                .collect();
            if self.short {
                out.pop();
            }
            Ok(out)
        }
        fn max_batch_size(&self) -> usize {
            4
        }
        fn name(&self) -> &'static str {
            "batch-fake"
        }
        fn model_id(&self) -> &'static str {
            "fake-1"
        }
        fn policy_version(&self) -> &'static str {
            "p-1"
        }
        fn threshold(&self) -> f32 {
            0.5
        }
    }

    async fn run_batch(model: &BatchFake, pool: &[FixtureRow], dir: &Path) -> RunStats {
        let mut budget = Budget::new(1.0);
        run_model("m", model, pool, dir, &Price::default(), &mut budget, T)
            .await
            .unwrap()
    }

    fn four() -> Vec<FixtureRow> {
        vec![
            row("a", "bad one"),
            row("b", "fine two"),
            row("c", "bad three"),
            row("d", "fine four"),
        ]
    }

    /// Final review fix 5: a wrong-count reply cannot be paired to posts, so
    /// every post in that batch is an error — none is guessed at.
    #[tokio::test]
    async fn a_short_batch_reply_makes_every_post_in_it_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let model = BatchFake {
            short: true,
            ..BatchFake::new(vec![])
        };
        let mut pool = four();
        pool.push(row("e", "bad five"));
        let s = run_batch(&model, &pool, dir.path()).await;
        assert_eq!(model.batches.load(Ordering::SeqCst), 2, "4 + 1 posts");
        // Both batches come back one short: all five posts are errors.
        assert_eq!((s.judged, s.errors), (0, 5));
        let recs = load(dir.path(), "m").unwrap();
        for id in ["a", "b", "c", "d", "e"] {
            assert_eq!(recs[id].called_toxic, None, "{id}");
            assert!(
                recs[id].error.as_deref().unwrap().contains("outcomes for"),
                "{id}: {:?}",
                recs[id].error
            );
        }
    }

    /// Final review fix 5: one slot failing does not taint its neighbours.
    #[tokio::test]
    async fn a_per_slot_error_marks_only_that_post() {
        let dir = tempfile::tempdir().unwrap();
        let model = BatchFake::new(vec![("three", Slot::Undecodable)]);
        let s = run_batch(&model, &four(), dir.path()).await;
        assert_eq!(model.batches.load(Ordering::SeqCst), 1);
        assert_eq!((s.judged, s.errors), (3, 1));
        let recs = load(dir.path(), "m").unwrap();
        assert_eq!(recs["c"].called_toxic, None);
        assert!(recs["c"]
            .error
            .as_deref()
            .unwrap()
            .contains("did not decode"));
        assert_eq!(recs["a"].called_toxic, Some(true));
        assert_eq!(recs["b"].called_toxic, Some(false));
        assert_eq!(recs["d"].called_toxic, Some(false));
    }

    /// Final review fix 5: in a mixed batch, the one slot reporting a foreign
    /// model is an error; the rest are kept.
    #[tokio::test]
    async fn a_foreign_model_in_one_slot_is_an_error_for_that_post_only() {
        let dir = tempfile::tempdir().unwrap();
        let model = BatchFake::new(vec![("two", Slot::ForeignModel)]);
        let s = run_batch(&model, &four(), dir.path()).await;
        assert_eq!((s.judged, s.errors), (3, 1));
        let recs = load(dir.path(), "m").unwrap();
        assert_eq!(recs["b"].called_toxic, None);
        assert!(recs["b"]
            .error
            .as_deref()
            .unwrap()
            .contains("identity mismatch"));
        assert_eq!(recs["a"].called_toxic, Some(true));
        assert_eq!(recs["c"].called_toxic, Some(true));
    }

    /// Final review fix 5: the right model under the wrong policy is still
    /// an identity mismatch, not a verdict.
    #[tokio::test]
    async fn a_policy_only_mismatch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let model = BatchFake::new(vec![("four", Slot::ForeignPolicy)]);
        let s = run_batch(&model, &four(), dir.path()).await;
        assert_eq!((s.judged, s.errors), (3, 1));
        let rec = &load(dir.path(), "m").unwrap()["d"];
        assert_eq!(rec.called_toxic, None);
        let err = rec.error.as_deref().unwrap();
        assert!(
            err.contains("identity mismatch") && err.contains("p-old"),
            "{err}"
        );
    }
}
