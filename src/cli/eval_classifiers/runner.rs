//! Run one model over the pool:
//! 1. reuse only current verdicts (by the DECLARED identity, no network), and
//!    return without waking the backend if nothing is left or the budget
//!    cannot cover the first batch;
//! 2. probe the backend's identity before any paid call;
//! 3. send the rest in the backend's own batch size (as production does);
//! 4. reject verdicts from an unexpected model or policy;
//! 5. bound the probe and each batch with a last-resort timeout, set ABOVE
//!    the backend's own so a job the backend still owns is never abandoned;
//! 6. stop at the reservation cap or when the backend's real cost meter trips.

use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use super::cache::{
    append, latest_match, load_all, load_identity, save_identity, Identity, VerdictRecord,
};
use super::cost::{estimate_usd, price_for, Budget, Price};
use super::data::FixtureRow;
use super::reasons::ReasonSource;
use crate::toxicity::classifier::{ItemOutcome, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

/// Above the RunPod client's own warm-up (180–300 s) and steady timeouts:
/// this guards against a hang, it must not race the backend's own timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// The backend failed its identity probe, so the model was not run and no
/// verdicts were bought. (On RunPod the probe itself wakes the GPU, so a
/// failed probe may still have cost a cold start plus the billed idle tail —
/// skipping cannot undo that, but it spends nothing further.) The ONLY
/// `run_model` error a multi-model run may skip past: every other error (a
/// cache that will not parse, an identity or verdict file that will not
/// write — possibly after a PAID batch) stops the run, so it can neither keep
/// spending nor lose paid verdicts and exit 0 (Codex fix 4).
///
/// The message leaves out the model name on purpose: the caller already
/// says which model it skipped. The underlying error is kept as the
/// `source`, so `{:#}` still prints it and a caller can downcast it.
#[derive(Debug, thiserror::Error)]
#[error("identity probe failed — not running it")]
pub struct ProbeFailed {
    pub model: String,
    #[source]
    pub source: anyhow::Error,
}

/// Every post in a batch came back from another model or policy than the one
/// declared: the deployment itself is wrong, so later batches would be paid
/// for and never yield a usable verdict (Codex review #3). The run loop
/// treats it like [`ProbeFailed`]: skip the model, and if it is paid, end
/// paid spending for the run. The batch's error records are saved first.
#[derive(Debug, thiserror::Error)]
#[error("every post in a batch came back from another identity ({detail}) — stopping this model")]
pub struct IdentityMismatch {
    pub model: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStats {
    pub model: String,
    pub judged: usize,
    pub cached: usize,
    pub errors: usize,
    pub stopped_at_cap: bool,
}

/// A model as the run loop sees it: the classifier, plus — for S1's bundle —
/// where to collect the reasons behind each verdict.
pub struct EvalModel {
    pub classifier: Arc<dyn ToxicityClassifier>,
    pub reasons: Option<Arc<dyn ReasonSource>>,
}

impl From<Arc<dyn ToxicityClassifier>> for EvalModel {
    fn from(classifier: Arc<dyn ToxicityClassifier>) -> Self {
        Self {
            classifier,
            reasons: None,
        }
    }
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
    run_model_with_reasons(model, classifier, None, pool, dir, price, budget, timeout).await
}

/// [`run_model`], also saving each verdict's reasons when `reasons` is given.
#[allow(clippy::too_many_arguments)]
pub async fn run_model_with_reasons(
    model: &str,
    classifier: &dyn ToxicityClassifier,
    reasons: Option<&dyn ReasonSource>,
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
    // For every pool post, the newest record that matches `who` and the
    // post's present text is a successful verdict that passed the
    // per-verdict model/policy check when it was recorded, which
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
    if !admits(budget, price, first_batch_usd) {
        stats.stopped_at_cap = true;
        return Ok(stats);
    }

    // Only now is waking the backend worth paying for.
    // Typed, so the run loop can tell "skip this model" from "stop".
    // Bounded by the same last-resort timeout as a batch: a hung endpoint
    // must not stall the run forever. A probe that times out is a failed
    // probe like any other — for a paid model it may still have woken the
    // GPU, so the run loop's paid-probe rule applies (Codex review #2, item 5).
    let probe = match tokio::time::timeout(timeout, classifier.probe_identity()).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "identity probe timed out after {} s",
            timeout.as_secs_f64()
        )),
    };
    if let Err(e) = probe {
        return Err(ProbeFailed {
            model: model.to_string(),
            source: e,
        }
        .into());
    }
    save_identity(dir, model, who)?;

    for chunk in todo.chunks(batch) {
        let ests: Vec<f64> = chunk
            .iter()
            .map(|r| estimate_usd(price, r.content.chars().count()))
            .collect();
        // `admits` first: `try_spend` alone would let a paid model's $0
        // estimate through an exhausted budget.
        if !admits(budget, price, ests.iter().sum()) || !budget.try_spend(ests.iter().sum()) {
            stats.stopped_at_cap = true;
            break;
        }
        let contents: Vec<String> = chunk.iter().map(|r| r.content.clone()).collect();
        let mut records = Vec::with_capacity(chunk.len());
        // Set when every slot of this batch reported another identity. One
        // stray slot is that post's error and the run goes on; a whole batch
        // means the deployment is wrong, and stopping is the only way to stop
        // paying for verdicts that can never be used (Codex review #3).
        let mut deployment_mismatch: Option<String> = None;
        match tokio::time::timeout(timeout, classifier.classify_batch(&contents)).await {
            Ok(Ok(outcomes)) if outcomes.len() == chunk.len() => {
                let mut mismatched = 0;
                let mut last_detail = String::new();
                // Positional pairing: the runner trusts the backend to return
                // outcomes in the order the posts were sent (the trait carries
                // no ids), as the classifier trait's contract requires.
                for ((row, est), outcome) in chunk.iter().zip(&ests).zip(outcomes) {
                    records.push(match outcome {
                        ItemOutcome::Verdict(v) if v.model_id == who.model_id && v.policy_version == who.policy_version => {
                            let mut rec = VerdictRecord::from_verdict(&row.id, model, &row.content, &v, *est);
                            rec.reasons = reasons.and_then(|r| r.take_reasons(&row.content));
                            rec
                        }
                        ItemOutcome::Verdict(v) => {
                            // Drop any reasons for it: they belong to a verdict we refuse.
                            if let Some(r) = reasons {
                                let _ = r.take_reasons(&row.content);
                            }
                            let detail = format!(
                                "identity mismatch: backend reported {}/{}, deployment declares {}/{}",
                                v.model_id, v.policy_version, who.model_id, who.policy_version
                            );
                            mismatched += 1;
                            last_detail.clone_from(&detail);
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                        ItemOutcome::Error(detail) => {
                            VerdictRecord::from_error(&row.id, model, &row.content, who, &detail, *est)
                        }
                    });
                }
                if mismatched == chunk.len() {
                    deployment_mismatch = Some(last_detail);
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
        // Only after the batch's records are saved: they were paid for.
        if let Some(detail) = deployment_mismatch {
            return Err(IdentityMismatch {
                model: model.to_string(),
                detail,
            }
            .into());
        }
    }
    Ok(stats)
}

/// May a call estimated at `usd` go ahead? The budget must cover it, AND a
/// paid model gets nothing from an exhausted budget even when this call's
/// estimate rounds to $0 — whether a model is paid comes from its configured
/// price, never from one estimate (Codex review #2, item 1). Free models
/// (local Detoxify) are unaffected: `can_afford(0.0)` always holds.
fn admits(budget: &Budget, price: &Price, usd: f64) -> bool {
    budget.can_afford(usd) && !(price.is_paid() && budget.is_exhausted())
}

/// What happened to one model of a multi-model run.
#[derive(Debug)]
pub enum ModelOutcome {
    Ran(RunStats),
    /// Not run and no verdicts bought (a failed probe may still have woken
    /// the GPU); the run went on.
    Skipped {
        model: String,
        reason: String,
    },
}

/// What every model in one run shares, besides the budget.
pub struct RunPlan<'a> {
    pub pool: &'a [FixtureRow],
    /// The verdicts directory.
    pub dir: &'a Path,
    pub prices: &'a HashMap<String, Price>,
    pub timeout: Duration,
}

/// The `eval-classifiers run` loop: each named model in turn, sharing one
/// budget. `build(name, committed)` constructs a model (`committed` is
/// [`Budget::committed`]: the whole cap once the budget is exhausted); `report` is told
/// each model's outcome as it happens (the CLI prints it). Lives here rather
/// than in main.rs so its skip/stop rules are tested.
pub async fn run_models<B, R>(
    names: &[String],
    plan: &RunPlan<'_>,
    budget: &mut Budget,
    mut build: B,
    mut report: R,
) -> Result<()>
where
    B: FnMut(&str, f64) -> Result<EvalModel>,
    R: FnMut(&ModelOutcome),
{
    let RunPlan {
        pool,
        dir,
        prices,
        timeout,
    } = *plan;
    // Refuse a repeated name before anything runs. Running a model twice in
    // one run has no use, and for a paid backend the second pass could wake
    // the GPU again (Codex review #2, item 1b).
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if !seen.insert(name.as_str()) {
            anyhow::bail!("--models lists {name:?} twice — name each model once");
        }
    }
    for name in names {
        // Refuse an unpriced model before building (or paying for) anything.
        let price = price_for(prices, name)?;
        let built = match build(name, budget.committed()) {
            Ok(c) => c,
            Err(e) => {
                // A missing key skips that model, not the run.
                report(&ModelOutcome::Skipped {
                    model: name.clone(),
                    reason: format!("{e:#}"),
                });
                continue;
            }
        };
        match run_model_with_reasons(
            name,
            built.classifier.as_ref(),
            built.reasons.as_deref(),
            pool,
            dir,
            price,
            budget,
            timeout,
        )
        .await
        {
            // Reaching the cap does NOT end the run: a later model may cost
            // nothing (Detoxify is local) (Codex fix 3). But it DOES end all
            // paid work: the budget holds only estimates, and if the stop came
            // from a backend's real meter (CoPE-B's GPU seconds) the estimates
            // understate what was actually spent. Exhausting the budget makes
            // every later paid model fail its `can_afford` check before its
            // probe, so nothing more is woken; $0 models still run
            // (re-review I1).
            Ok(s) => {
                if s.stopped_at_cap {
                    budget.exhaust();
                }
                report(&ModelOutcome::Ran(s))
            }
            // A failed probe bought no verdicts, so there is nothing to lose
            // by moving on. But for a PAID model the probe itself may have
            // woken — and billed — the GPU, and no meter saw it. So treat it
            // like a cap stop: exhaust the budget, and no later paid model is
            // woken this run. A free model's failed probe cost nothing and
            // leaves the budget alone (Codex review #2, item 1a). A batch that
            // came back wholly from another identity gets the same treatment:
            // its records are already saved, and a paid model's misdeployed
            // endpoint must not be paid again this run (Codex review #3).
            Err(e)
                if e.downcast_ref::<ProbeFailed>().is_some()
                    || e.downcast_ref::<IdentityMismatch>().is_some() =>
            {
                if price.is_paid() {
                    budget.exhaust();
                }
                report(&ModelOutcome::Skipped {
                    model: name.clone(),
                    reason: format!("{e:#}"),
                })
            }
            // Anything else may follow a paid batch whose verdicts were not
            // saved: stop the run with a non-zero exit (Codex fix 4).
            Err(e) => return Err(e.context(format!("{name}: run stopped"))),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::load;
    use crate::cli::eval_classifiers::cost::Price;
    use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};
    use crate::toxicity::cost_meter::CostCeilingExceeded;
    use anyhow::Context;
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
        /// The probe sleeps this long before answering (a hung endpoint).
        probe_hangs: Option<Duration>,
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
                probe_hangs: None,
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
            if let Some(d) = self.probe_hangs {
                tokio::time::sleep(d).await;
            }
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
        // One post per batch, so its mismatch is batch-wide: the model stops
        // (Codex review #3) — but the error is still recorded first.
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
        assert!(err.downcast_ref::<IdentityMismatch>().is_some(), "{err:#}");
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

    fn priced(per_post: f64) -> Price {
        Price {
            usd_per_post: Some(per_post),
            usd_per_million_input_tokens: None,
            overhead_chars: 0,
        }
    }

    /// Runs `names` through [`run_models`], building each from `models`.
    async fn run_all(
        names: &[&str],
        models: &[(&str, Arc<Fake>)],
        prices: &HashMap<String, Price>,
        pool: &[FixtureRow],
        dir: &Path,
        budget: &mut Budget,
    ) -> (Result<()>, Vec<String>) {
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let plan = RunPlan {
            pool,
            dir,
            prices,
            timeout: T,
        };
        let mut seen = Vec::new();
        let res = run_models(
            &names,
            &plan,
            budget,
            |name, _spent| {
                let m = models
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, m)| Arc::clone(m))
                    .with_context(|| format!("no fake for {name}"))?;
                Ok(EvalModel::from(m as Arc<dyn ToxicityClassifier>))
            },
            |o| {
                seen.push(match o {
                    ModelOutcome::Ran(s) => {
                        format!(
                            "ran {} judged {} cap {}",
                            s.model, s.judged, s.stopped_at_cap
                        )
                    }
                    ModelOutcome::Skipped { model, .. } => format!("skipped {model}"),
                })
            },
        )
        .await;
        (res, seen)
    }

    /// Codex fix 3: reaching the cap on a paid model must not skip the
    /// models after it — a free one (Detoxify) still fits any budget.
    #[tokio::test]
    async fn a_free_model_still_runs_after_a_paid_one_reaches_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let prices: HashMap<String, Price> = [
            ("paid".to_string(), priced(0.4)),
            ("free".to_string(), Price::default()),
        ]
        .into();
        let paid = Arc::new(Fake::new());
        let free = Arc::new(Fake::new());
        let mut budget = Budget::new(0.5);
        let (res, seen) = run_all(
            &["paid", "free"],
            &[("paid", Arc::clone(&paid)), ("free", Arc::clone(&free))],
            &prices,
            &pool,
            dir.path(),
            &mut budget,
        )
        .await;
        res.unwrap();
        assert_eq!(
            seen,
            vec![
                "ran paid judged 1 cap true".to_string(),
                "ran free judged 2 cap false".to_string()
            ]
        );
        assert_eq!(free.calls.load(Ordering::SeqCst), 2);
        assert!(budget.spent() <= 0.5, "the cap still holds");
    }

    /// Re-review I1: the shared budget only holds ESTIMATES, but a backend's
    /// real cost meter can trip long before the estimates add up (CoPE-B's
    /// meter counts real GPU seconds). Once any model stops at the cap, a
    /// later PAID model must not be woken at all — it would get nearly the
    /// whole cap again — while a free one still runs.
    #[tokio::test]
    async fn after_a_real_meter_trip_no_later_paid_model_is_woken() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "capped"), row("c", "fine")];
        let prices: HashMap<String, Price> = [
            ("paid_a".to_string(), priced(0.001)),
            ("paid_b".to_string(), priced(0.001)),
            ("free".to_string(), Price::default()),
        ]
        .into();
        let paid_a = Arc::new(Fake {
            capped_on: Some("capped"),
            ..Fake::new()
        });
        let paid_b = Arc::new(Fake::new());
        let free = Arc::new(Fake::new());
        let mut budget = Budget::new(1.0);
        let (res, seen) = run_all(
            &["paid_a", "paid_b", "free"],
            &[
                ("paid_a", Arc::clone(&paid_a)),
                ("paid_b", Arc::clone(&paid_b)),
                ("free", Arc::clone(&free)),
            ],
            &prices,
            &pool,
            dir.path(),
            &mut budget,
        )
        .await;
        res.unwrap();
        assert_eq!(
            seen,
            vec![
                "ran paid_a judged 1 cap true".to_string(),
                "ran paid_b judged 0 cap true".to_string(),
                "ran free judged 3 cap false".to_string(),
            ]
        );
        assert_eq!(paid_b.probes.load(Ordering::SeqCst), 0, "B never woken");
        assert_eq!(paid_b.calls.load(Ordering::SeqCst), 0, "B never called");
        assert_eq!(free.calls.load(Ordering::SeqCst), 3, "free still runs");
    }

    /// Codex review #2, item 1a: a PAID model whose probe fails may already
    /// have woken (and billed) the GPU. A later paid model must not wake it
    /// again — no probe, no call — while a free model still runs.
    #[tokio::test]
    async fn a_failed_paid_probe_ends_paid_spending_for_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let prices: HashMap<String, Price> = [
            ("paid_a".to_string(), priced(0.001)),
            ("paid_b".to_string(), priced(0.001)),
            ("free".to_string(), Price::default()),
        ]
        .into();
        let paid_a = Arc::new(Fake {
            probe_fails: true,
            ..Fake::new()
        });
        let paid_b = Arc::new(Fake::new());
        let free = Arc::new(Fake::new());
        let mut budget = Budget::new(1.0);
        let (res, seen) = run_all(
            &["paid_a", "paid_b", "free"],
            &[
                ("paid_a", Arc::clone(&paid_a)),
                ("paid_b", Arc::clone(&paid_b)),
                ("free", Arc::clone(&free)),
            ],
            &prices,
            &pool,
            dir.path(),
            &mut budget,
        )
        .await;
        res.unwrap();
        assert_eq!(
            seen,
            vec![
                "skipped paid_a".to_string(),
                "ran paid_b judged 0 cap true".to_string(),
                "ran free judged 2 cap false".to_string(),
            ]
        );
        assert_eq!(paid_b.probes.load(Ordering::SeqCst), 0, "B never woken");
        assert_eq!(paid_b.calls.load(Ordering::SeqCst), 0, "B never called");
        assert_eq!(free.calls.load(Ordering::SeqCst), 2, "free still runs");
    }

    /// Item 1a, the other side: a FREE model's failed probe cost nothing, so
    /// it must not end paid spending.
    #[tokio::test]
    async fn a_failed_free_probe_does_not_stop_a_later_paid_model() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad")];
        let prices: HashMap<String, Price> = [
            ("free".to_string(), Price::default()),
            ("paid".to_string(), priced(0.001)),
        ]
        .into();
        let free = Arc::new(Fake {
            probe_fails: true,
            ..Fake::new()
        });
        let paid = Arc::new(Fake::new());
        let (res, seen) = run_all(
            &["free", "paid"],
            &[("free", free), ("paid", Arc::clone(&paid))],
            &prices,
            &pool,
            dir.path(),
            &mut Budget::new(1.0),
        )
        .await;
        res.unwrap();
        assert_eq!(
            seen,
            vec![
                "skipped free".to_string(),
                "ran paid judged 1 cap false".to_string(),
            ]
        );
        assert_eq!(paid.calls.load(Ordering::SeqCst), 1);
    }

    /// Item 1b: naming a model twice is a mistake (it would probe — and on
    /// RunPod wake — the same backend twice). Refused before anything runs.
    #[tokio::test]
    async fn a_model_named_twice_is_refused_up_front() {
        let dir = tempfile::tempdir().unwrap();
        let prices: HashMap<String, Price> = [
            ("a".to_string(), Price::default()),
            ("b".to_string(), Price::default()),
        ]
        .into();
        let a = Arc::new(Fake::new());
        let (res, seen) = run_all(
            &["a", "b", "a"],
            &[("a", Arc::clone(&a)), ("b", Arc::new(Fake::new()))],
            &prices,
            &[row("p", "bad")],
            dir.path(),
            &mut Budget::new(1.0),
        )
        .await;
        let err = format!("{:#}", res.unwrap_err());
        assert!(err.contains("\"a\"") && err.contains("twice"), "{err}");
        assert!(seen.is_empty(), "nothing ran: {seen:?}");
        assert_eq!(a.probes.load(Ordering::SeqCst), 0);
    }

    /// Item 1, the zero-estimate minor: "paid" is decided by the configured
    /// price, not by the batch estimate. A per-token model whose first batch
    /// estimates to $0 (an empty post, no overhead) is still paid, so an
    /// exhausted budget refuses it before its probe.
    #[tokio::test]
    async fn a_paid_model_with_a_zero_estimate_is_refused_once_exhausted() {
        let dir = tempfile::tempdir().unwrap();
        let per_token = Price {
            usd_per_post: None,
            usd_per_million_input_tokens: Some(1.0),
            overhead_chars: 0,
        };
        let mut budget = Budget::new(1.0);
        budget.exhaust();
        let model = Fake::new();
        let s = run_model(
            "m",
            &model,
            &[row("a", "")],
            dir.path(),
            &per_token,
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert!(s.stopped_at_cap);
        assert_eq!(model.probes.load(Ordering::SeqCst), 0, "no wake-up");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    }

    /// Codex fix 4: a failed probe is the one run_model error that is safe to
    /// skip (nothing was spent), so it carries its own type.
    #[tokio::test]
    async fn a_failed_probe_is_a_typed_probe_failure() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake {
            probe_fails: true,
            ..Fake::new()
        };
        let err = run_model(
            "m",
            &model,
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut Budget::new(1.0),
            T,
        )
        .await
        .unwrap_err();
        assert!(err.downcast_ref::<ProbeFailed>().is_some(), "{err:#}");
        // The cause survives (re-review M8) and the model name is not
        // repeated — the CLI prefixes it already (re-review M9).
        let shown = format!("{err:#}");
        assert!(shown.contains("endpoint serves policy-old"), "{shown}");
        assert!(!shown.starts_with("m:"), "{shown}");
    }

    /// Codex review #2, item 5: `--timeout-secs` bounds the identity probe
    /// too. A probe that hangs past it is a `ProbeFailed` (so a paid one
    /// ends paid spending), returned promptly, with no classify call.
    #[tokio::test]
    async fn a_hung_probe_times_out_as_a_probe_failure() {
        let dir = tempfile::tempdir().unwrap();
        let model = Fake {
            probe_hangs: Some(Duration::from_secs(30)),
            ..Fake::new()
        };
        let started = std::time::Instant::now();
        let err = run_model(
            "m",
            &model,
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut Budget::new(1.0),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not wait out the 30 s hang"
        );
        assert!(err.downcast_ref::<ProbeFailed>().is_some(), "{err:#}");
        assert!(format!("{err:#}").contains("timed out"), "{err:#}");
        assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    }

    /// Codex fix 4: a local file failure (here: the verdicts "directory" is a
    /// plain file) is NOT a probe failure — it could lose paid verdicts.
    #[tokio::test]
    async fn a_cache_failure_is_not_a_probe_failure() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("verdicts");
        std::fs::write(&not_a_dir, "").unwrap();
        let err = run_model(
            "m",
            &Fake::new(),
            &[row("a", "bad")],
            &not_a_dir,
            &Price::default(),
            &mut Budget::new(1.0),
            T,
        )
        .await
        .unwrap_err();
        assert!(err.downcast_ref::<ProbeFailed>().is_none(), "{err:#}");
    }

    /// Codex fix 4: the run skips a model whose probe fails and goes on...
    #[tokio::test]
    async fn the_run_skips_a_model_whose_probe_fails() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad")];
        let prices: HashMap<String, Price> = [
            ("broken".to_string(), Price::default()),
            ("ok".to_string(), Price::default()),
        ]
        .into();
        let broken = Arc::new(Fake {
            probe_fails: true,
            ..Fake::new()
        });
        let (res, seen) = run_all(
            &["broken", "ok"],
            &[("broken", broken), ("ok", Arc::new(Fake::new()))],
            &prices,
            &pool,
            dir.path(),
            &mut Budget::new(1.0),
        )
        .await;
        res.unwrap();
        assert_eq!(
            seen,
            vec![
                "skipped broken".to_string(),
                "ran ok judged 1 cap false".to_string()
            ]
        );
    }

    /// ...but stops, with an error, on anything else: a local file error
    /// after a paid batch would otherwise keep spending while losing what
    /// it paid for, and exit 0.
    #[tokio::test]
    async fn the_run_stops_on_a_local_file_error() {
        let dir = tempfile::tempdir().unwrap();
        let not_a_dir = dir.path().join("verdicts");
        std::fs::write(&not_a_dir, "").unwrap();
        let prices: HashMap<String, Price> = [
            ("first".to_string(), Price::default()),
            ("second".to_string(), Price::default()),
        ]
        .into();
        let second = Arc::new(Fake::new());
        let (res, seen) = run_all(
            &["first", "second"],
            &[
                ("first", Arc::new(Fake::new())),
                ("second", Arc::clone(&second)),
            ],
            &prices,
            &[row("a", "bad")],
            &not_a_dir,
            &mut Budget::new(1.0),
        )
        .await;
        assert!(res.is_err(), "a file error is not skippable");
        assert!(seen.is_empty(), "{seen:?}");
        assert_eq!(second.probes.load(Ordering::SeqCst), 0, "nothing after it");
    }

    /// The same rule when the paid batch itself is the thing whose verdicts
    /// cannot be saved: the run must not report success.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_append_after_a_paid_batch_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        // A dangling symlink reads as "no cache yet" (NotFound), so the run
        // probes and pays; appending then fails, because the link points
        // into a directory that does not exist.
        std::os::unix::fs::symlink(
            dir.path().join("missing").join("m.jsonl"),
            dir.path().join("m.jsonl"),
        )
        .unwrap();
        let model = Fake::new();
        let res = run_model(
            "m",
            &model,
            &[row("a", "bad")],
            dir.path(),
            &Price::default(),
            &mut Budget::new(1.0),
            T,
        )
        .await;
        let err = res.unwrap_err();
        assert_eq!(model.calls.load(Ordering::SeqCst), 1, "the batch was paid");
        assert!(err.downcast_ref::<ProbeFailed>().is_none(), "{err:#}");
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

    /// Codex review #3: when EVERY slot of a batch reports another identity,
    /// the deployment itself is wrong — further batches would be paid for and
    /// never yield a usable verdict. Stop the model after that batch, keeping
    /// the batch's (paid) error records.
    #[tokio::test]
    async fn a_batch_wide_identity_mismatch_stops_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let model = BatchFake::new(vec![("post", Slot::ForeignModel)]);
        let pool: Vec<FixtureRow> = (1..=8)
            .map(|i| row(&format!("p{i}"), &format!("post {i}")))
            .collect();
        let mut budget = Budget::new(1.0);
        let err = run_model(
            "m",
            &model,
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap_err();
        assert!(err.downcast_ref::<IdentityMismatch>().is_some(), "{err:#}");
        assert_eq!(
            model.batches.load(Ordering::SeqCst),
            1,
            "no second batch paid for"
        );
        let recs = load(dir.path(), "m").unwrap();
        assert_eq!(recs.len(), 4, "the first batch's records are kept");
        assert!(recs.values().all(|r| r.called_toxic.is_none()));
    }

    /// Codex review #3, in the run loop: a PAID model stopped by a
    /// deployment-wide mismatch ends paid spending (like a failed paid
    /// probe), and the run goes on to free models.
    #[tokio::test]
    async fn a_paid_identity_mismatch_ends_paid_spending_for_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let prices: HashMap<String, Price> = [
            ("paid_a".to_string(), priced(0.001)),
            ("paid_b".to_string(), priced(0.001)),
            ("free".to_string(), Price::default()),
        ]
        .into();
        let paid_a = Arc::new(Fake {
            reported_model: "someone-else",
            ..Fake::new()
        });
        let paid_b = Arc::new(Fake::new());
        let free = Arc::new(Fake::new());
        let mut budget = Budget::new(1.0);
        let (res, seen) = run_all(
            &["paid_a", "paid_b", "free"],
            &[
                ("paid_a", Arc::clone(&paid_a)),
                ("paid_b", Arc::clone(&paid_b)),
                ("free", Arc::clone(&free)),
            ],
            &prices,
            &pool,
            dir.path(),
            &mut budget,
        )
        .await;
        res.unwrap();
        assert_eq!(seen[0], "skipped paid_a");
        assert_eq!(
            paid_a.calls.load(Ordering::SeqCst),
            1,
            "stopped after one batch"
        );
        assert_eq!(paid_b.probes.load(Ordering::SeqCst), 0, "B never woken");
        assert_eq!(paid_b.calls.load(Ordering::SeqCst), 0, "B never called");
        assert_eq!(free.calls.load(Ordering::SeqCst), 2, "free still runs");
    }

    /// Review of Codex #4 fix, M2: once the budget is exhausted, the next
    /// model is built with the WHOLE cap counted as gone, so a real cost
    /// meter sized from it gets no headroom, even though `spent()` (the
    /// printed estimate) stays small.
    #[tokio::test]
    async fn a_model_built_after_exhaustion_is_given_no_headroom() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "bad"), row("b", "fine")];
        let prices: HashMap<String, Price> = [
            ("paid".to_string(), priced(0.001)),
            ("free".to_string(), Price::default()),
        ]
        .into();
        let paid = Arc::new(Fake {
            reported_model: "someone-else",
            ..Fake::new()
        });
        let free = Arc::new(Fake::new());
        let mut budget = Budget::new(1.0);
        let names = vec!["paid".to_string(), "free".to_string()];
        let plan = RunPlan {
            pool: &pool,
            dir: dir.path(),
            prices: &prices,
            timeout: T,
        };
        let mut given = Vec::new();
        run_models(
            &names,
            &plan,
            &mut budget,
            |name, committed| {
                given.push(committed);
                let m = if name == "paid" { &paid } else { &free };
                Ok(EvalModel::from(Arc::clone(m) as Arc<dyn ToxicityClassifier>))
            },
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(given[0], 0.0, "nothing committed before the first model");
        assert_eq!(given[1], 1.0, "the whole cap is gone after exhaustion");
        assert!(budget.spent() < 1.0, "the printed estimate is not the cap");
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

    /// A fake S1: answers toxic and has reasons for every post it judged.
    struct WithReasons {
        given: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl ToxicityClassifier for WithReasons {
        async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
            self.given.lock().unwrap().push(content.to_string());
            Ok(ClassifierVerdict {
                toxic_token: true,
                confidence: 0.9,
                latency_ms: 1,
                model_id: "s1".into(),
                policy_version: "v".into(),
            })
        }
        fn name(&self) -> &'static str {
            "s1"
        }
        fn model_id(&self) -> &'static str {
            "s1"
        }
        fn policy_version(&self) -> &'static str {
            "v"
        }
        fn threshold(&self) -> f32 {
            0.0
        }
    }
    impl crate::cli::eval_classifiers::reasons::ReasonSource for WithReasons {
        fn take_reasons(
            &self,
            content: &str,
        ) -> Option<crate::cli::eval_classifiers::reasons::ReasonSet> {
            let fired = content.contains("threat");
            Some(crate::cli::eval_classifiers::reasons::ReasonSet {
                answers: [(
                    "threat".to_string(),
                    crate::cli::eval_classifiers::reasons::ReasonAnswer {
                        fired,
                        confidence: 0.8,
                    },
                )]
                .into(),
                complete: true,
            })
        }
    }

    #[tokio::test]
    async fn reasons_are_saved_with_each_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let pool = vec![row("a", "a threat"), row("b", "fine")];
        let m = WithReasons {
            given: Default::default(),
        };
        let mut budget = Budget::new(1.0);
        run_model_with_reasons(
            "s1",
            &m,
            Some(&m),
            &pool,
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        let got = load(dir.path(), "s1").unwrap();
        assert_eq!(got["a"].reasons.as_ref().unwrap().fired(), vec!["threat"]);
        assert!(got["b"].reasons.as_ref().unwrap().fired().is_empty());
    }

    #[tokio::test]
    async fn a_model_without_a_reason_source_saves_no_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let m = WithReasons {
            given: Default::default(),
        };
        let mut budget = Budget::new(1.0);
        run_model(
            "s1",
            &m,
            &[row("a", "a threat")],
            dir.path(),
            &Price::default(),
            &mut budget,
            T,
        )
        .await
        .unwrap();
        assert!(load(dir.path(), "s1").unwrap()["a"].reasons.is_none());
    }
}
