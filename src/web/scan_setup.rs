// Scan setup — model loading, fingerprint staleness/rebuild decisions, and the
// scoring-stack bundle a scan runs on.
//
// Extracted from `src/web/scan_job.rs` (#344) so the next task's control-flow
// changes to the scan pipeline itself can be reviewed without also reviewing
// relocated lines. Pure move: no logic changed. `scan_job.rs` re-exports the
// items call sites there still need; new code should import
// `crate::web::scan_setup` directly instead of going through that bridge.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::db::Database;
use crate::observability::cache_stats::CacheStats;
use crate::scoring::behavioral::detect_pile_on_participants;
use crate::topics::embeddings::EMBEDDING_MODEL_ID;
use crate::toxicity::ensemble::TwoStageToxicityScorer;
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

/// The three ONNX models a scan needs, loaded once and shared.
///
/// These used to load per scan so they were not held while idle. Concurrency
/// (#257) makes that cost linear in concurrent scans — ~500MB each since #231
/// moved NLI to the fp32 export — so they move to `AppState` and stay resident.
/// Accepted trade: production idles ~0.776GB today and ~1.28GB after, which
/// raises the metered RAM floor (#188) even on days nobody scans.
pub struct ScanModels {
    pub toxicity: Arc<OnnxToxicityScorer>,
    pub embedder: Arc<crate::topics::embeddings::SentenceEmbedder>,
    pub nli: Arc<crate::scoring::nli::NliScorer>,
}

impl ScanModels {
    /// Load all three models from `model_dir`.
    ///
    /// Fails hard rather than degrading: a server that cannot score is not
    /// usefully up, and production auto-downloads missing models before this
    /// point. The per-scan path used to degrade when a model was absent; at
    /// boot that would hide a broken deploy behind a green healthcheck.
    pub fn load(model_dir: &std::path::Path) -> anyhow::Result<Self> {
        let toxicity = OnnxToxicityScorer::load(model_dir)
            .context("failed to load the toxicity model at boot")?;
        let embedder = crate::topics::embeddings::SentenceEmbedder::load(
            &crate::toxicity::download::embedding_model_dir(model_dir),
        )
        .context("failed to load the embedding model at boot")?;
        let nli = crate::scoring::nli::NliScorer::load(model_dir)
            .context("failed to load the NLI model at boot")?;

        Ok(Self {
            toxicity: Arc::new(toxicity),
            embedder: Arc::new(embedder),
            nli: Arc::new(nli),
        })
    }
}

/// Rebuild the protected user's fingerprint when it's older than this.
/// Matches the scan-staleness tiering cadence (Normal = 14 days). (#296,
/// spike #295 defect 10 — updated_at was recorded but never consulted.)
const FINGERPRINT_MAX_AGE_DAYS: i64 = 14;

/// True when a fingerprint's `updated_at` (both backends emit
/// `YYYY-MM-DD HH:MM:SS` UTC) is more than FINGERPRINT_MAX_AGE_DAYS old.
/// Unparseable timestamps count as STALE: a successful rebuild rewrites
/// `updated_at` via the DB clock, so a one-off corrupt row self-heals after
/// one rebuild, while treating it as fresh would keep an old fingerprint on
/// the fresh path forever. A rebuild-per-scan only persists under systemic
/// timestamp-format drift, which the warn below makes loud — and the
/// stale-rebuild-failure path already falls back gracefully. (#303)
pub fn fingerprint_is_stale(updated_at: &str, now: chrono::NaiveDateTime) -> bool {
    match chrono::NaiveDateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S") {
        Ok(built) => {
            now.signed_duration_since(built) > chrono::Duration::days(FINGERPRINT_MAX_AGE_DAYS)
        }
        Err(_) => {
            warn!(
                updated_at,
                "Unparseable fingerprint timestamp; treating as stale and rebuilding"
            );
            true
        }
    }
}

/// A stored generation needs a format rebuild when the embedding path ran
/// (mean embedding exists) but the per-topic centroid rows don't match the
/// fingerprint JSON: zero rows = pre-#297 legacy, count mismatch = pre-#302
/// divergence. Keyword-only fingerprints (no embedding) are NOT legacy —
/// treating them as such would rebuild-loop every scan on model-less
/// deployments. (#297)
pub fn fingerprint_needs_rebuild_for_format(
    has_embedding: bool,
    centroid_rows: usize,
    json_clusters: usize,
) -> bool {
    has_embedding && centroid_rows != json_clusters
}

/// Why the protected fingerprint must be rebuilt before this scan (#344 V2-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason {
    /// No stored fingerprint at all.
    Absent,
    /// A stored fingerprint whose JSON would not parse.
    Unreadable,
    /// Older than `FINGERPRINT_MAX_AGE_DAYS` (#296).
    Stale,
    /// Embedding present but the centroid rows do not match the JSON (#297/#302).
    Format,
    /// The stored vectors were produced by a different embedding model — or by
    /// one that was never recorded, which is the same thing for our purposes.
    IncompatibleModel,
}

impl RebuildReason {
    /// Only age/format rebuilds may fall back to the stored fingerprint when
    /// the rebuild fails; incompatible vectors must never be scored against.
    pub fn fallback_allowed(&self) -> bool {
        matches!(self, RebuildReason::Stale | RebuildReason::Format)
    }
}

/// Pure decision: `None` = the stored fingerprint is usable as-is.
///
/// `stored` is `(fingerprint_json, updated_at)`; `parsed_ok` says whether that
/// JSON deserialized. Reasons are ordered by severity, so the returned reason
/// is always the strongest one that applies — which matters because
/// [`RebuildReason::fallback_allowed`] is read off it: an incompatible
/// fingerprint that is ALSO stale must report `IncompatibleModel`, or a failed
/// rebuild would be allowed to fall back onto vectors this binary cannot score
/// against (R03/V2-04).
pub fn rebuild_decision(
    stored: Option<&(String, String)>,
    parsed_ok: bool,
    has_embedding: bool,
    stored_model_id: Option<&str>,
    centroid_rows: usize,
    json_clusters: usize,
    now: chrono::NaiveDateTime,
) -> Option<RebuildReason> {
    // No row at all: nothing to salvage, and nothing to fall back to.
    let Some((_, updated_at)) = stored else {
        return Some(RebuildReason::Absent);
    };
    if !parsed_ok {
        return Some(RebuildReason::Unreadable);
    }
    // A keyword-only fingerprint (no embedding) has no vectors to be
    // incompatible — model-less deployments must not rebuild-loop (#297).
    if has_embedding && stored_model_id != Some(EMBEDDING_MODEL_ID) {
        return Some(RebuildReason::IncompatibleModel);
    }
    if fingerprint_is_stale(updated_at, now) {
        return Some(RebuildReason::Stale);
    }
    if fingerprint_needs_rebuild_for_format(has_embedding, centroid_rows, json_clusters) {
        return Some(RebuildReason::Format);
    }
    None
}

/// The scoring stack a scan runs on, plus the cache counters it must persist.
///
/// Extracted from `run_scan` (#344 Task 8) because the nightly refresh needs
/// exactly the same bundle; the counters travel with the scorers so nothing
/// can build one and forget the other.
pub(crate) struct ScanScorers {
    pub scorer: TwoStageToxicityScorer,
    pub onnx_stats: Arc<CacheStats>,
    pub classifier_stats: Arc<CacheStats>,
}

/// Build the two-stage scorer over the shared ONNX models and the caches.
///
/// The classifier is required — `build_from_env` errors (and the caller fails
/// loudly) if unconfigured; there is no silent ONNX-only fallback.
pub(crate) fn build_scan_scorers(
    models: &ScanModels,
    db: &Arc<dyn Database>,
) -> anyhow::Result<ScanScorers> {
    // #343 Phase 1: stage-1 / clean-pass ONNX scores are cached by text hash
    // across users. Hit/miss counts are persisted at the end of the scan.
    let onnx_stats = Arc::new(CacheStats::default());
    let primary_scorer: Box<dyn ToxicityScorer> =
        Box::new(crate::toxicity::cached::CachedToxicityScorer::new(
            Box::new(Arc::clone(&models.toxicity)),
            Arc::clone(db),
            crate::toxicity::onnx::ONNX_MODEL_ID,
            Arc::clone(&onnx_stats),
        ));

    // Wrap in the two-stage scorer. ONNX runs as a clean-pass filter
    // (< 0.10 = cleared); posts at or above the threshold are sent to the
    // configured Stage-2 classifier (CHARCOAL_CLASSIFIER) for a binary verdict.
    // #343 Phase 1: stage-2 verdicts are cached by (text hash, model, policy).
    let classifier_stats = Arc::new(CacheStats::default());
    let classifier: Arc<dyn crate::toxicity::classifier::ToxicityClassifier> =
        Arc::new(crate::toxicity::cached_classifier::CachedClassifier::new(
            crate::toxicity::classifier::build_from_env()?,
            Arc::clone(db),
            Arc::clone(&classifier_stats),
        ));
    info!(
        backend = classifier.name(),
        "Stage-2 toxicity classifier loaded — two-stage scoring enabled"
    );
    // Scan-start banner metric so log aggregation can attribute which backend
    // produced this scan's verdicts.
    crate::observability::classifier_metrics::record_backend_selected(classifier.name());

    // Concrete scorer (not boxed as `dyn`): the phased scan pipeline (#208)
    // needs the `TwoStageToxicityScorer`'s inherent `classifier()` accessor and
    // its `CleanPassScorer` impl, both of which a `dyn ToxicityScorer` erases.
    Ok(ScanScorers {
        scorer: TwoStageToxicityScorer::new(primary_scorer, classifier),
        onnx_stats,
        classifier_stats,
    })
}

/// Persist both cache counters. Best-effort by construction: telemetry must
/// never fail a run that has already done its work.
pub(crate) async fn record_scan_cache_stats(db: &dyn Database, user_did: &str, s: &ScanScorers) {
    if let Err(e) =
        crate::observability::cache_stats::record_cache_stats(db, user_did, "onnx", &s.onnx_stats)
            .await
    {
        warn!(error = %e, "could not record onnx cache stats");
    }

    if let Err(e) = crate::observability::cache_stats::record_cache_stats(
        db,
        user_did,
        "classifier",
        &s.classifier_stats,
    )
    .await
    {
        warn!(error = %e, "could not record classifier cache stats");
    }
}

/// The protected user's recent posts embedded for follower NLI pairing.
///
/// `Err` on fetch or embedding failure; `Ok(empty)` when the user has no
/// posts. The full scan degrades on `Err` at its call site; the refresh does
/// not (R05).
pub(crate) async fn embed_protected_posts(
    client: &PublicAtpClient,
    embedder: &crate::topics::embeddings::SentenceEmbedder,
    actor_handle: &str,
) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
    let pp_texts: Vec<String> = crate::bluesky::posts::fetch_recent_posts(client, actor_handle, 50)
        .await
        .context("fetching the protected user's recent posts for NLI pairing")?
        .iter()
        .map(|p| p.text.clone())
        .collect();
    if pp_texts.is_empty() {
        return Ok(Vec::new());
    }
    let embeddings = embedder
        .embed_batch(&pp_texts)
        .await
        .context("embedding the protected user's posts for NLI pairing")?;
    Ok(pp_texts.into_iter().zip(embeddings).collect())
}

/// The accounts that participated in a pile-on against this user.
pub(crate) async fn pile_on_dids(
    db: &dyn Database,
    user_did: &str,
) -> anyhow::Result<HashSet<String>> {
    let pile_on_refs = db.get_events_for_pile_on(user_did).await?;
    Ok(detect_pile_on_participants(
        &pile_on_refs
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    ))
}

#[cfg(test)]
mod fingerprint_staleness_tests {
    use super::*;

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str("2026-08-20 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn fresh_fingerprint_is_not_stale() {
        assert!(!fingerprint_is_stale("2026-08-19 12:00:00", now()));
    }

    #[test]
    fn fifteen_day_old_fingerprint_is_stale() {
        assert!(fingerprint_is_stale("2026-08-05 11:59:59", now()));
    }

    #[test]
    fn exactly_fourteen_days_is_not_stale() {
        // Boundary: staleness begins strictly AFTER 14 days.
        assert!(!fingerprint_is_stale("2026-08-06 12:00:00", now()));
    }

    #[test]
    fn unparseable_timestamp_is_treated_as_stale() {
        // A malformed timestamp must trigger the rebuild path: a successful
        // rebuild rewrites updated_at via the DB clock, so a one-off corrupt
        // row self-heals after one rebuild. Treating it as fresh would keep
        // an old fingerprint on the fresh path FOREVER, silently bypassing
        // the 14-day refresh. (#303, CodeRabbit PR #101; supersedes the
        // original #296 plan decision.)
        assert!(fingerprint_is_stale("not a date", now()));
        assert!(fingerprint_is_stale("", now()));
    }

    #[test]
    fn legacy_format_forces_rebuild() {
        // Embedding present but zero centroid rows = pre-#297 generation.
        assert!(fingerprint_needs_rebuild_for_format(true, 0, 3));
        // Keyword-only fingerprint (no embedding) is NOT legacy — no rebuild loop.
        assert!(!fingerprint_needs_rebuild_for_format(false, 0, 3));
        // Row/JSON mismatch (pre-#302 divergence) rebuilds.
        assert!(fingerprint_needs_rebuild_for_format(true, 2, 3));
        // Healthy clustered generation: no rebuild.
        assert!(!fingerprint_needs_rebuild_for_format(true, 3, 3));
    }

    #[test]
    fn rebuild_decision_orders_reasons_and_flags_model_incompatibility() {
        let now = chrono::Utc::now().naive_utc();
        let fresh = (now - chrono::Duration::days(1))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let old = (now - chrono::Duration::days(20))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string();
        let stored = |t: &str| Some(("{}".to_string(), t.to_string()));
        assert_eq!(
            rebuild_decision(None, false, false, None, 0, 0, now),
            Some(RebuildReason::Absent)
        );
        assert_eq!(
            rebuild_decision(
                stored(&fresh).as_ref(),
                false,
                true,
                Some(EMBEDDING_MODEL_ID),
                3,
                3,
                now
            ),
            Some(RebuildReason::Unreadable)
        );
        // Same dimensions, same cluster count, different model: incompatible.
        assert_eq!(
            rebuild_decision(
                stored(&fresh).as_ref(),
                true,
                true,
                Some("other-model"),
                3,
                3,
                now
            ),
            Some(RebuildReason::IncompatibleModel)
        );
        // A vector with NO recorded model is incompatible too — never assume.
        assert_eq!(
            rebuild_decision(stored(&fresh).as_ref(), true, true, None, 3, 3, now),
            Some(RebuildReason::IncompatibleModel)
        );
        assert_eq!(
            rebuild_decision(
                stored(&old).as_ref(),
                true,
                true,
                Some(EMBEDDING_MODEL_ID),
                3,
                3,
                now
            ),
            Some(RebuildReason::Stale)
        );
        assert_eq!(
            rebuild_decision(
                stored(&fresh).as_ref(),
                true,
                true,
                Some(EMBEDDING_MODEL_ID),
                0,
                3,
                now
            ),
            Some(RebuildReason::Format)
        );
        assert_eq!(
            rebuild_decision(
                stored(&fresh).as_ref(),
                true,
                true,
                Some(EMBEDDING_MODEL_ID),
                3,
                3,
                now
            ),
            None
        );
        // Keyword-only fingerprints have no vectors to be incompatible.
        assert_eq!(
            rebuild_decision(stored(&fresh).as_ref(), true, false, None, 0, 3, now),
            None
        );
        assert!(RebuildReason::Stale.fallback_allowed());
        assert!(!RebuildReason::IncompatibleModel.fallback_allowed());
        assert!(!RebuildReason::Absent.fallback_allowed());
    }
}
