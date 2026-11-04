//! Read-through stage-2 verdict cache (#343 §4.1).
//!
//! Wraps the configured `ToxicityClassifier` (RunPod CoPE-B in production).
//! Keyed by (text hash, model id, policy version) so a policy bump simply
//! stops matching old rows. Only decodable verdicts are stored — an
//! `ItemOutcome::Error` slot or a request-level `Err` is handed back exactly
//! as the inner classifier produced it, so the burst phase's downcasts on
//! `CostCeilingExceeded` / `ClassifierTransientError` keep working.
//!
//! It is also where the identity probe is paid, lazily (#394). The probe is a
//! real round trip to the GPU endpoint, and on RunPod serverless every round
//! trip bills a worker's idle tail. Paying it up front, before anything was
//! gathered, meant a refresh whose candidates were all deleted accounts woke
//! the GPU every hour to classify nothing. Here it is paid only when a cache
//! MISS is about to reach the endpoint, and a SUCCESS is paid at most once per
//! instance (one instance per run). A failure is shared with the batches
//! already waiting on it, and asked again by any later one. A run that classifies nothing, or finds every verdict
//! cached, never contacts it at all.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::db::{ClassifierVerdictRow, Database};
use crate::observability::cache_stats::CacheStats;

use super::cached::text_sha256;
use super::classifier::{
    ClassifierTransientError, ClassifierVerdict, ItemOutcome, ToxicityClassifier,
};
use super::cost_meter::CostCeilingExceeded;

/// A failed probe, kept so callers that were queued behind it can share it.
///
/// `anyhow::Error` is not `Clone`, so the failure is stored as the parts the
/// burst cares about and rebuilt per caller. The kind must survive: the burst
/// pauses on a transient failure or a cost cap and fails the scan on anything
/// else, so replaying a cold-worker timeout as a plain error would turn a
/// resumable pause into a failed scan.
#[derive(Clone)]
enum ProbeFailure {
    Transient(String),
    CostCapped { est_cents: u32, ceiling_cents: u32 },
    Other(String),
}

impl ProbeFailure {
    fn capture(err: &anyhow::Error) -> Self {
        if let Some(t) = err.downcast_ref::<ClassifierTransientError>() {
            ProbeFailure::Transient(t.0.clone())
        } else if let Some(c) = err.downcast_ref::<CostCeilingExceeded>() {
            ProbeFailure::CostCapped {
                est_cents: c.est_cents,
                ceiling_cents: c.ceiling_cents,
            }
        } else {
            ProbeFailure::Other(format!("{err:#}"))
        }
    }

    fn replay(&self) -> anyhow::Error {
        match self {
            ProbeFailure::Transient(detail) => ClassifierTransientError::new(detail.clone()).into(),
            ProbeFailure::CostCapped {
                est_cents,
                ceiling_cents,
            } => CostCeilingExceeded {
                est_cents: *est_cents,
                ceiling_cents: *ceiling_cents,
            }
            .into(),
            ProbeFailure::Other(detail) => anyhow::anyhow!("{detail}"),
        }
    }
}

pub struct CachedClassifier {
    inner: Arc<dyn ToxicityClassifier>,
    db: Arc<dyn Database>,
    stats: Arc<CacheStats>,
    /// Set once the endpoint has confirmed its identity for this run.
    probed: AtomicBool,
    /// How many probes have failed. A caller reads it BEFORE queueing for
    /// `probe_lock`; if it moved while the caller waited, the caller was
    /// queued behind that failure and shares it instead of asking again.
    failures: AtomicU64,
    /// Serialises probes and holds the most recent failure for replay.
    probe_lock: Mutex<Option<ProbeFailure>>,
}

impl CachedClassifier {
    pub fn new(
        inner: Arc<dyn ToxicityClassifier>,
        db: Arc<dyn Database>,
        stats: Arc<CacheStats>,
    ) -> Self {
        Self {
            inner,
            db,
            stats,
            probed: AtomicBool::new(false),
            failures: AtomicU64::new(0),
            probe_lock: Mutex::new(None),
        }
    }

    /// Probe the endpoint unless this run already has. Concurrent burst
    /// batches share one in-flight probe instead of each paying their own,
    /// whether it succeeds or fails.
    ///
    /// Only a success is remembered for good. A failure is shared with the
    /// callers that were already waiting on it (PR #160 review: they used to
    /// re-probe one after another, each able to wait out the cold-start
    /// timeout), but a caller that arrives afterwards asks again, so a
    /// transient failure does not condemn the rest of the run (#394).
    async fn ensure_probed(&self) -> Result<()> {
        if self.probed.load(Ordering::SeqCst) {
            return Ok(());
        }
        // Read before queueing. A probe that fails in the short gap between
        // this read and the lock is also shared with this caller even though
        // it did not wait on it; that only ever saves a probe, never skips a
        // needed one, because a success is checked first.
        let failures_seen = self.failures.load(Ordering::SeqCst);
        let mut last_failure = self.probe_lock.lock().await;
        if self.probed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.failures.load(Ordering::SeqCst) != failures_seen {
            if let Some(failure) = last_failure.as_ref() {
                return Err(failure.replay());
            }
        }
        match self.inner.probe_identity().await {
            Ok(()) => {
                self.probed.store(true, Ordering::SeqCst);
                Ok(())
            }
            Err(err) => {
                *last_failure = Some(ProbeFailure::capture(&err));
                self.failures.fetch_add(1, Ordering::SeqCst);
                Err(err)
            }
        }
    }

    /// A cached row rebuilt as a verdict. `latency_ms` is 0: nothing was
    /// called. Model/policy are the inner's current values — by construction
    /// the row was looked up under exactly those.
    fn verdict_from_row(&self, row: &ClassifierVerdictRow) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: row.toxic_token,
            confidence: row.confidence as f32,
            latency_ms: 0,
            model_id: self.inner.model_id().to_string(),
            policy_version: self.inner.policy_version().to_string(),
        }
    }
}

#[async_trait]
impl ToxicityClassifier for CachedClassifier {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let mut out = self
            .classify_batch(std::slice::from_ref(&content.to_string()))
            .await?;
        match out.pop() {
            Some(ItemOutcome::Verdict(v)) => Ok(v),
            Some(ItemOutcome::Error(e)) => bail!("classifier slot did not decode: {e}"),
            None => bail!("classify_batch returned no result for a single text"),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        if contents.is_empty() {
            return Ok(Vec::new());
        }
        let model_id = self.inner.model_id();
        let policy_version = self.inner.policy_version();
        let hashes: Vec<String> = contents.iter().map(|c| text_sha256(c)).collect();
        let cached = self
            .db
            .get_classifier_verdicts(model_id, policy_version, &hashes)
            .await?;

        // Distinct misses in first-seen order (duplicates classify once).
        let mut miss_slot: HashMap<&str, usize> = HashMap::new();
        let mut miss_texts: Vec<String> = Vec::new();
        for (content, hash) in contents.iter().zip(&hashes) {
            if !cached.contains_key(hash) && !miss_slot.contains_key(hash.as_str()) {
                miss_slot.insert(hash.as_str(), miss_texts.len());
                miss_texts.push(content.clone());
            }
        }

        let hits = hashes.iter().filter(|h| cached.contains_key(*h)).count() as u64;
        self.stats.hit(hits);
        self.stats.miss(contents.len() as u64 - hits);

        // Request-level errors propagate untouched (see module docs).
        let fresh = if miss_texts.is_empty() {
            Vec::new()
        } else {
            // The first miss of the run is the first moment the endpoint is
            // actually needed, so it is the moment to confirm its identity.
            self.ensure_probed().await?;
            self.inner.classify_batch(&miss_texts).await?
        };
        if fresh.len() != miss_texts.len() {
            bail!(
                "inner classifier returned {} outcomes for {} texts",
                fresh.len(),
                miss_texts.len()
            );
        }

        // Only cache a verdict whose REPORTED identity is the one this
        // classifier advertises. The row is keyed on the advertised
        // model/policy and a hit is rebuilt with that identity
        // (`verdict_from_row`) — the row itself stores no provenance. So caching
        // a foreign verdict would launder it: the first finalize correctly
        // rejects it as foreign evidence, the bounded re-gather hits the cache,
        // and the replay now claims to be current, publishing a score the
        // wrong model or policy produced (#344 Codex review P1). A foreign
        // verdict still flows out below with its true provenance; it just is
        // not remembered, so the next request goes back to the endpoint.
        let rows: Vec<ClassifierVerdictRow> = miss_slot
            .iter()
            .filter_map(|(hash, &slot)| match &fresh[slot] {
                ItemOutcome::Verdict(v)
                    if v.model_id == model_id && v.policy_version == policy_version =>
                {
                    Some(ClassifierVerdictRow {
                        text_sha256: (*hash).to_string(),
                        toxic_token: v.toxic_token,
                        confidence: f64::from(v.confidence),
                    })
                }
                ItemOutcome::Verdict(_) | ItemOutcome::Error(_) => None,
            })
            .collect();
        self.db
            .upsert_classifier_verdicts(model_id, policy_version, &rows)
            .await?;

        let out = hashes
            .iter()
            .map(|hash| match cached.get(hash) {
                Some(row) => ItemOutcome::Verdict(self.verdict_from_row(row)),
                None => fresh[miss_slot[hash.as_str()]].clone(),
            })
            .collect();
        Ok(out)
    }

    fn max_batch_size(&self) -> usize {
        self.inner.max_batch_size()
    }
    /// Contacts the live endpoint — never answered from the verdict cache,
    /// whose rows are exactly the stale identity the probe checks for — but
    /// a success at most once per run: an explicit probe IS the run's probe, so the
    /// first miss afterwards does not pay another. (Scans no longer call this
    /// up front — #423 — and rely on the first-miss probe instead.)
    async fn probe_identity(&self) -> anyhow::Result<()> {
        self.ensure_probed().await
    }
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn model_id(&self) -> &'static str {
        self.inner.model_id()
    }
    fn policy_version(&self) -> &'static str {
        self.inner.policy_version()
    }
    fn threshold(&self) -> f32 {
        self.inner.threshold()
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    fn kind(err: &anyhow::Error) -> (&'static str, String) {
        if let Some(t) = err.downcast_ref::<ClassifierTransientError>() {
            ("transient", t.0.clone())
        } else if let Some(c) = err.downcast_ref::<CostCeilingExceeded>() {
            ("cost", format!("{}/{}", c.est_cents, c.ceiling_cents))
        } else {
            ("other", format!("{err:#}"))
        }
    }

    proptest! {
        // Invariant: replaying a captured probe failure yields an error of the
        // SAME kind with the same detail. The burst pauses on a transient
        // error or a cost cap and fails the scan on any other, so a replay
        // that changed the kind would change what a waiting batch's scan does.
        #[test]
        fn a_replayed_failure_keeps_its_kind_and_detail(
            which in 0u8..3,
            detail in "[a-z ]{0,30}",
            est in any::<u32>(),
            ceiling in any::<u32>(),
        ) {
            let original: anyhow::Error = match which {
                0 => ClassifierTransientError::new(detail.clone()).into(),
                1 => CostCeilingExceeded { est_cents: est, ceiling_cents: ceiling }.into(),
                _ => anyhow::anyhow!("{detail}"),
            };
            let replayed = ProbeFailure::capture(&original).replay();
            prop_assert_eq!(kind(&replayed), kind(&original));
        }
    }
}
