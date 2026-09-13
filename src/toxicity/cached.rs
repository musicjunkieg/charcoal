//! Read-through ONNX score cache (#343 §4.1).
//!
//! A text's toxicity score is a property of (text, model), not of the scan
//! that met it, so it is scored once and shared by every protected user. The
//! key is the SHA-256 of the exact string the model scored — never the text
//! itself — so the table holds no readable content. Only the headline
//! `toxicity` is cached: it is the only number the two-stage scorer reads
//! from stage 1, and the attribute breakdown would multiply the row size
//! for nothing.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::db::{Database, OnnxScoreRow};
use crate::observability::cache_stats::CacheStats;

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Lowercase hex SHA-256 of `text`'s UTF-8 bytes — the cache key for both
/// `onnx_scores` and `classifier_verdicts`.
pub fn text_sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub struct CachedToxicityScorer {
    inner: Box<dyn ToxicityScorer>,
    db: Arc<dyn Database>,
    model_id: &'static str,
    stats: Arc<CacheStats>,
}

impl CachedToxicityScorer {
    pub fn new(
        inner: Box<dyn ToxicityScorer>,
        db: Arc<dyn Database>,
        model_id: &'static str,
        stats: Arc<CacheStats>,
    ) -> Self {
        Self {
            inner,
            db,
            model_id,
            stats,
        }
    }
}

#[async_trait]
impl ToxicityScorer for CachedToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let mut out = self
            .score_batch(std::slice::from_ref(&text.to_string()))
            .await?;
        match out.pop() {
            Some(r) => Ok(r),
            None => bail!("score_batch returned no result for a single text"),
        }
    }

    /// Look every hash up in one query, send only the distinct misses to the
    /// inner scorer in one batch, persist them, and reassemble in input order.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let hashes: Vec<String> = texts.iter().map(|t| text_sha256(t)).collect();
        let cached = self.db.get_onnx_scores(self.model_id, &hashes).await?;

        // Distinct misses, in first-seen order. `miss_slot[hash]` is the index
        // into `miss_texts`/`fresh` so duplicates within a batch score once.
        let mut miss_slot: HashMap<&str, usize> = HashMap::new();
        let mut miss_texts: Vec<String> = Vec::new();
        for (text, hash) in texts.iter().zip(&hashes) {
            if !cached.contains_key(hash) && !miss_slot.contains_key(hash.as_str()) {
                miss_slot.insert(hash.as_str(), miss_texts.len());
                miss_texts.push(text.clone());
            }
        }

        let fresh = if miss_texts.is_empty() {
            Vec::new()
        } else {
            self.inner.score_batch(&miss_texts).await?
        };
        if fresh.len() != miss_texts.len() {
            bail!(
                "inner scorer returned {} results for {} texts",
                fresh.len(),
                miss_texts.len()
            );
        }

        let rows: Vec<OnnxScoreRow> = miss_slot
            .iter()
            .map(|(hash, &slot)| OnnxScoreRow {
                text_sha256: (*hash).to_string(),
                score: fresh[slot].toxicity,
            })
            .collect();
        self.db.upsert_onnx_scores(self.model_id, &rows).await?;

        let mut hits = 0u64;
        let mut out = Vec::with_capacity(texts.len());
        for hash in &hashes {
            if let Some(&score) = cached.get(hash) {
                hits += 1;
                out.push(ToxicityResult {
                    toxicity: score,
                    attributes: ToxicityAttributes::default(),
                });
            } else {
                out.push(fresh[miss_slot[hash.as_str()]].clone());
            }
        }
        self.stats.hit(hits);
        self.stats.miss(texts.len() as u64 - hits);
        Ok(out)
    }
}
