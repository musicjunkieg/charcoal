//! The scoring revision stamp (#343 §4.4, #344).
//!
//! Every `account_scores` row records the *revision* it was scored under:
//! the human-bumped [`SCORING_GENERATION`] composed with every in-binary model
//! identity. A row is *fresh* only if its stamp equals [`scoring_revision()`]
//! **and** its `valid_until` is in the future; anything else is hidden from
//! tier lists and re-scored by the refresh job (High/Elevated) or on the
//! account's next re-engagement.
//!
//! # Why a composite
//!
//! Model swaps change scores: a different toxicity, embedding or NLI model
//! makes yesterday's numbers incomparable with today's even when the formula
//! is untouched. Composing the identities in means a swap expires stored
//! scores by itself — nobody has to remember (V2-01). The caches are NOT
//! affected: `onnx_scores` and `classifier_verdicts` are keyed by their own
//! model ids and stay reusable across a revision change.
//!
//! # When to bump [`SCORING_GENERATION`] by hand
//!
//! - the threat formula or its weights (`src/scoring/threat.rs`)
//! - the topic-overlap math or the fingerprint JSON format (`src/topics/`)
//! - a scoring-policy change (tier thresholds, abstention rules)
//! - a **classifier** model or policy change (CoPE-B / Zentropi): the
//!   classifier lives outside this binary, so its identity is not composed
//!   in; the runbook's deploy checklist covers it. Staged verdicts from the
//!   old classifier are rejected at finalize regardless (model_id + policy).
//!
//! Bumping is a code change, not a config knob: two replicas disagreeing on
//! the revision would hide each other's scores. The value is opaque; the
//! date form of the generation is for humans reading
//! `SELECT scoring_generation, COUNT(*) …`.
//!
//! # Rolling deploys
//!
//! A revision change ships as a single-replica deploy. During the seconds
//! both binaries run, the old one may still stamp its in-flight scan's rows
//! with the old revision; the new binary hides those rows and the refresh
//! job re-scores the High/Elevated ones. Staged work the old binary left
//! behind carries the old revision in `scan_state.scan_run_generation` and
//! is discarded on the next run (`pipeline::scan_phases::RunIdentity`).
use std::sync::LazyLock;

pub const SCORING_GENERATION: &str = "2026-09-13";

/// The stamp migration v18 writes onto rows scored before revisions
/// existed. Never equal to [`scoring_revision()`].
pub const LEGACY_GENERATION: &str = "legacy";

/// Pure composition, so tests can build alternative revisions. `|` is the
/// delimiter; no identity contains it (asserted by the unit test).
pub fn compose_revision(generation: &str, onnx: &str, embedding: &str, nli: &str) -> String {
    debug_assert!(![generation, onnx, embedding, nli]
        .iter()
        .any(|s| s.contains('|')));
    format!("{generation}|onnx={onnx}|emb={embedding}|nli={nli}")
}

static SCORING_REVISION: LazyLock<String> = LazyLock::new(|| {
    compose_revision(
        SCORING_GENERATION,
        crate::toxicity::onnx::ONNX_MODEL_ID,
        crate::topics::embeddings::EMBEDDING_MODEL_ID,
        crate::scoring::nli::NLI_MODEL_ID,
    )
});

/// The revision this binary scores under. Bound into every freshness query
/// and written onto every score row, staged blob and run marker.
pub fn scoring_revision() -> &'static str {
    SCORING_REVISION.as_str()
}
