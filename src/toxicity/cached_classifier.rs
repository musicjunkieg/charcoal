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
//! the GPU every hour to classify nothing. Here it is paid at most once per
//! instance — one instance per run — and only when a cache MISS is about to
//! reach the endpoint. A run that classifies nothing, or finds every verdict
//! cached, never contacts it at all.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use tokio::sync::OnceCell;

use crate::db::{ClassifierVerdictRow, Database};
use crate::observability::cache_stats::CacheStats;

use super::cached::text_sha256;
use super::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};

pub struct CachedClassifier {
    inner: Arc<dyn ToxicityClassifier>,
    db: Arc<dyn Database>,
    stats: Arc<CacheStats>,
    /// Set once the endpoint has confirmed its identity for this run. Only a
    /// SUCCESS is remembered: `get_or_try_init` leaves the cell empty on an
    /// error, so a refused or failed probe is asked again next time rather
    /// than replayed.
    probed: OnceCell<()>,
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
            probed: OnceCell::new(),
        }
    }

    /// Probe the endpoint unless this run already has. Concurrent burst
    /// batches share one in-flight probe instead of each paying their own.
    async fn ensure_probed(&self) -> Result<()> {
        self.probed
            .get_or_try_init(|| self.inner.probe_identity())
            .await?;
        Ok(())
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
    /// at most once per run: the full scan's explicit fail-fast probe at start
    /// IS the run's probe, so the first miss afterwards does not pay another.
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
