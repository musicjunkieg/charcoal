// Staging value types for the three-phase scan pipeline.
//
// The pipeline stages work-queue pattern:
//   Phase A (Gather)   — fetch posts, compute behavioral signals, enqueue classifier work
//   Phase B (Burst)    — run classifier verdicts for each queued post
//   Phase C (Score/Finalize) — read back staged AccountInput and compute final AccountScore
//
// This module defines the wire types that travel between phases via the DB-staged
// work queue. No DB logic lives here — only the structs and their serialisation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bluesky::posts::PostSample;

// ── Schema version ────────────────────────────────────────────────────────────

/// Bumped whenever the shape of `AccountInput` changes in a breaking way.
/// Stored alongside every serialised blob so Phase C can reject stale rows
/// rather than silently misinterpret them.
pub const ACCOUNT_INPUT_SCHEMA_VERSION: u32 = 3;

// ── ScanPhase ─────────────────────────────────────────────────────────────────

/// Current phase of a scan run, stored as the `scan_phase` key inside the
/// `scan_state` k/v store.  This is a typed wrapper over a plain string value —
/// it is NOT a DB column of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanPhase {
    /// Phase A: collecting posts and enqueueing classifier work.
    Gather,
    /// Phase B: running classifier verdicts (the "burst" of GPU/API calls).
    Burst,
    /// Phase C: reading back staged data and writing final scores.
    Finalize,
    /// All phases complete for this scan run.
    Done,
}

impl ScanPhase {
    /// Canonical string representation stored in `scan_state`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanPhase::Gather => "gather",
            ScanPhase::Burst => "burst",
            ScanPhase::Finalize => "finalize",
            ScanPhase::Done => "done",
        }
    }

    /// Parse from the string value stored in `scan_state`.  Returns `None` for
    /// unrecognised values (e.g. values written by a future schema version).
    pub fn from_value(s: &str) -> Option<Self> {
        match s {
            "gather" => Some(ScanPhase::Gather),
            "burst" => Some(ScanPhase::Burst),
            "finalize" => Some(ScanPhase::Finalize),
            "done" => Some(ScanPhase::Done),
            _ => None,
        }
    }
}

// ── QueueRow ──────────────────────────────────────────────────────────────────

/// One row in the per-scan classifier work queue.
///
/// Phase A writes one `QueueRow` per post that needs a verdict.
/// Phase B reads pending rows, calls the classifier, and fills in the
/// `toxic_token`/`confidence`/`model_id`/`policy_version` fields.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueRow {
    /// DID of the account this post belongs to.
    pub account_did: String,
    /// AT URI of the post (primary key within a scan).
    pub post_uri: String,
    /// Post text sent to the classifier.
    pub text: String,
    /// Optional parent post text for reply-pair classification.
    pub context_text: Option<String>,
    /// Post kind tag: `"original"`, `"reply"`, or `"quote"`.
    pub post_kind: String,
    /// Raw ONNX toxicity score (clean-pass filter output).
    pub onnx_score: f64,
    /// Work-queue status: `"pending"` or `"done"` (the v9 schema CHECK
    /// constraint allows only these two values).
    pub status: String,
    // ── filled in by Phase B ──
    /// Binary toxicity verdict from the classifier (`None` while pending).
    pub toxic_token: Option<bool>,
    /// Classifier confidence score (`None` while pending).
    pub confidence: Option<f32>,
    /// Classifier model identifier (`None` while pending).
    pub model_id: Option<String>,
    /// Policy version used for the verdict (`None` while pending).
    pub policy_version: Option<String>,
}

// ── VerdictRow ────────────────────────────────────────────────────────────────

/// A completed classifier verdict, produced by Phase B.
///
/// Returned from the queue after Phase B fills in all fields.  Phase C
/// aggregates these to compute the reply-weighted toxicity rate.
#[derive(Debug, Clone, PartialEq)]
pub struct VerdictRow {
    /// DID of the account this verdict belongs to.
    pub account_did: String,
    /// AT URI of the post.
    pub post_uri: String,
    /// Binary toxicity verdict.
    pub toxic_token: bool,
    /// Classifier confidence score.
    pub confidence: f32,
    /// Classifier model identifier.
    pub model_id: String,
    /// Policy version used for the verdict.
    pub policy_version: String,
}

// ── AccountInput ──────────────────────────────────────────────────────────────

/// All per-account data that Phase A collects and Phase C needs to compute
/// the final `AccountScore` — everything except the classifier verdicts
/// (which travel via `VerdictRow`).
///
/// Serialised as JSON and stored in the DB work queue.  `schema_version` lets
/// Phase C reject stale blobs without panicking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountInput {
    /// Schema version — must equal `ACCOUNT_INPUT_SCHEMA_VERSION` for Phase C
    /// to process this blob.
    pub schema_version: u32,

    /// The `scoring_revision()` this blob was gathered under (schema v3,
    /// #344 R03). Phase C refuses to finalize a blob from another revision:
    /// its target embedding, sample selection and fingerprint quality were
    /// computed against inputs the current formula may not accept.
    pub scoring_generation: String,

    /// Handle of the account being scored. Phase C needs this for the
    /// `AccountScore.handle` field — the orchestrator only has the DID
    /// (`list_scan_accounts` is DID-only), so the handle is stashed here.
    pub account_handle: String,

    /// The 50-post sample fetched in Stage 2, partitioned into originals,
    /// replies, and quotes.  Carries reply_ratio, quote_ratio, total_posts.
    pub sample: PostSample,

    /// Parent post texts keyed by AT URI, fetched for reply-pair context.
    /// Used to reconstruct the `(parent_text, reply_text)` pairs for the
    /// context-score step in Phase C.
    pub parent_texts: HashMap<String, String>,

    /// Median engagement across all accounts in the current scan run.
    /// Used by `behavioral::apply_behavioral_modifier_contextual` as the
    /// normalisation baseline.
    pub median_engagement: f64,

    /// Whether this account participated in a pile-on against the protected
    /// user within the 24-hour sliding window.  Precomputed by Phase A as
    /// `pile_on_dids.contains(account_did)` — the scan-global set is not
    /// stored here.
    pub is_pile_on: bool,

    /// Direct (amplifier) text pairs for NLI context scoring.
    /// Each tuple is `(protected_user_post_text, amplifier_post_text)`.
    /// `None` when the account has no direct amplification events.
    pub direct_pairs: Option<Vec<(String, String)>>,

    /// Social graph distance from the protected user, serialised as the
    /// `GraphDistance::as_str()` value (`"Mutual follow"`, `"Follows you"`,
    /// `"You follow"`, `"Stranger"`).  `None` when the relationship API
    /// call was skipped or failed.
    pub graph_distance: Option<String>,

    /// Fingerprint quality tier computed from the sample's originals count:
    /// `"normal"`, `"degraded"`, or `"unreliable"`.
    pub fingerprint_quality: String,

    /// Target mean sentence-embedding, precomputed in Phase A (gather) over
    /// `select_fingerprint_posts(&sample)`. Moving this ONNX work out of the
    /// serial Phase-C finalize loop is the whole point of #213 — in Phase A it
    /// overlaps network I/O instead of serializing on the global model mutex.
    /// `None` when no embedder was loaded at gather time; Phase C then embeds
    /// at score time (old behavior) or falls back to TF-IDF.
    #[serde(default)]
    pub target_embedding: Option<Vec<f64>>,
}

impl AccountInput {
    /// Construct a minimal valid `AccountInput` for unit tests.
    ///
    /// All numeric fields are zero/false, collections are empty, and
    /// `schema_version` is set to `ACCOUNT_INPUT_SCHEMA_VERSION`.
    pub fn new_for_test() -> Self {
        use crate::bluesky::posts::PostSample;
        AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            scoring_generation: crate::scoring::generation::scoring_revision().to_string(),
            account_handle: "test.handle".to_string(),
            sample: PostSample {
                originals: vec![],
                replies: vec![],
                quotes: vec![],
                reply_ratio: 0.0,
                quote_ratio: 0.0,
                total_posts: 0,
            },
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "normal".to_string(),
            target_embedding: None,
        }
    }
}

// ── Evidence provenance (#344 R03, V2-01, V3-01) ───────────────────────────────

/// The policy label the Stage-1 clean pass writes onto the rows it settles.
///
/// A clean-pass row is evidence produced by the ONNX model, not by the
/// classifier, so it is validated against the ONNX identity rather than the
/// classifier's. Before v18 those rows carried `None`/`None`, which finalize
/// could not tell apart from a corrupt row — hence the explicit label.
pub const CLEAN_PASS_POLICY: &str = "onnx-clean-pass";

/// The Stage-2 classifier's identity, as it advertises it and as it writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifierIdentity<'a> {
    pub model_id: &'a str,
    pub policy_version: &'a str,
}

/// What finalize accepts as evidence for THIS run (R03, V2-01).
///
/// A current-revision score may only be published from verdict rows whose
/// recorded producer is one this binary actually runs. Anything else — an old
/// binary's unlabelled clean row, another classifier policy, the decode-error
/// sentinel — is incomplete evidence and costs one bounded re-gather.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceContract<'a> {
    pub onnx_model_id: &'a str,
    pub classifier: ClassifierIdentity<'a>,
}

/// Who produced a done row, decided from its recorded provenance alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Settled by the Stage-1 ONNX clean pass of this binary.
    CleanPass,
    /// Settled by the Stage-2 classifier this binary runs.
    Classifier,
    /// The `decode-error` benign sentinel a failed burst decode writes (#355).
    DecodeErrorSentinel,
    /// No producer recorded: a pre-v18 clean row, or a corrupt one.
    Missing,
    /// A producer this binary does not run — another model or another policy.
    Foreign,
}

impl EvidenceContract<'_> {
    /// Classify a row's recorded `(model_id, policy_version)` pair.
    ///
    /// Order matters: the clean-pass arm is checked first so an ONNX model id
    /// paired with the *classifier's* policy cannot masquerade as a clean row,
    /// and the sentinel arm sits after both so a real producer named
    /// `decode-error` (there is none) could never be mistaken for it.
    pub fn provenance(&self, model_id: Option<&str>, policy_version: Option<&str>) -> Provenance {
        match (model_id, policy_version) {
            (Some(m), Some(p)) if m == self.onnx_model_id && p == CLEAN_PASS_POLICY => {
                Provenance::CleanPass
            }
            (Some(m), Some(p))
                if m == self.classifier.model_id && p == self.classifier.policy_version =>
            {
                Provenance::Classifier
            }
            (Some("decode-error"), _) => Provenance::DecodeErrorSentinel,
            (None, _) | (_, None) => Provenance::Missing,
            _ => Provenance::Foreign,
        }
    }

    /// Only evidence from a known, current producer counts.
    pub fn accepts(&self, model_id: Option<&str>, policy_version: Option<&str>) -> bool {
        matches!(
            self.provenance(model_id, policy_version),
            Provenance::CleanPass | Provenance::Classifier
        )
    }
}
