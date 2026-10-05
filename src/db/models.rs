// Data models — Rust structs that map to database rows.
//
// These are the types that flow through the application. They're separate
// from the database queries so other modules can use them without depending
// on rusqlite directly.

use serde::{Deserialize, Serialize};

/// A scored account in the threat list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccountScore {
    pub did: String,
    pub handle: String,
    pub toxicity_score: Option<f64>,
    pub topic_overlap: Option<f64>,
    /// Shadow-compare value (#297): what overlap WOULD have been under the
    /// pre-#297 single-mean-centroid formula. Recorded so #135 can
    /// recalibrate gates/tiers from real paired data. None on the keyword
    /// fallback path (no embeddings involved).
    pub overlap_legacy: Option<f64>,
    pub threat_score: Option<f64>,
    pub threat_tier: Option<String>,
    pub posts_analyzed: u32,
    /// The most toxic posts as evidence (JSON-encoded in the DB)
    pub top_toxic_posts: Vec<ToxicPost>,
    pub scored_at: String,
    /// Behavioral signals (JSON-serialized), present when behavioral analysis ran
    pub behavioral_signals: Option<String>,
    /// NLI-derived contextual hostility score (max across all interaction pairs)
    pub context_score: Option<f64>,
    /// Social graph distance to the protected user (None if not classified)
    pub graph_distance: Option<String>,
    /// Quality of the topic fingerprint used for overlap scoring
    pub fingerprint_quality: Option<String>,
    /// Confidence level of this scoring result
    pub scoring_confidence: Option<String>,
}

impl AccountScore {
    /// Construct a minimal valid `AccountScore` for unit tests, mirroring
    /// `AccountInput::new_for_test`.
    ///
    /// Everything optional is `None`, collections are empty, and `scored_at`
    /// is now — the shape a test needs when it only cares about one or two
    /// fields (a tier, a score) and must not hand-write the other thirteen.
    /// Deliberately not `#[cfg(test)]`: integration tests in `tests/` compile
    /// against the library, where a `cfg(test)` item does not exist.
    pub fn default_for_test(did: &str) -> Self {
        AccountScore {
            did: did.to_string(),
            handle: format!("{}.test", did.trim_start_matches("did:plc:")),
            toxicity_score: None,
            topic_overlap: None,
            overlap_legacy: None,
            threat_score: None,
            threat_tier: None,
            posts_analyzed: 0,
            top_toxic_posts: vec![],
            scored_at: chrono::Utc::now().to_rfc3339(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        }
    }
}

/// One stored topic centroid. Label/keywords/weight live in the fingerprint
/// JSON (clusters[i] ↔ cluster_index i); this is only what scoring needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterCentroid {
    pub centroid: Vec<f64>,
    pub post_count: u32,
}

/// Confidence level of a scoring result.
///
/// Set by `scoring::profile`, not by a post count: `Low` when Stage 1 exits
/// early, otherwise `High` or `Standard` by the topic fingerprint's quality
/// (`FingerprintQuality`). Drives expiry: Low confidence scores expire sooner
/// (3 days) than High confidence scores (14 days).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScoringConfidence {
    /// Stage 1 early exit: every sampled post was clean and the topic overlap
    /// was below the gate, so the account was never fully scored.
    Low,
    /// Fully scored, but the fingerprint was `Degraded` or `Unreliable`
    /// (fewer than 15 original posts).
    Standard,
    /// Fully scored with a `Normal` fingerprint (at least 15 original posts).
    High,
}

impl ScoringConfidence {
    /// Number of days before this score is considered stale.
    pub fn staleness_days(&self) -> i64 {
        match self {
            ScoringConfidence::Low => 3,
            ScoringConfidence::Standard => 7,
            ScoringConfidence::High => 14,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ScoringConfidence::Low => "low",
            ScoringConfidence::Standard => "standard",
            ScoringConfidence::High => "high",
        }
    }

    /// Inverse of [`as_str`](Self::as_str). `None` for anything the enum
    /// never produced.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "low" => Some(ScoringConfidence::Low),
            "standard" => Some(ScoringConfidence::Standard),
            "high" => Some(ScoringConfidence::High),
            _ => None,
        }
    }

    /// Staleness window for a stored `scoring_confidence` label (#344).
    /// `None` (NotAssessed / insufficient-data rows) and unknown labels fall
    /// back to Standard (7 days) — never 0, never forever.
    pub fn staleness_days_for_label(label: Option<&str>) -> i64 {
        label
            .and_then(Self::from_label)
            .unwrap_or(ScoringConfidence::Standard)
            .staleness_days()
    }
}

/// A single post with its toxicity score, kept as evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToxicPost {
    pub text: String,
    pub toxicity: f64,
    pub uri: String,
}

/// One `account_scores` row with its provenance, for lossless export/import
/// (#344 R01). The presentation reads (`get_ranked_threats`, counts) hide
/// expired rows; this does not, and `import_score` writes it back verbatim.
/// How a row's expiry left its backend (V2-06). Postgres rows are always
/// `At`; SQLite can hold NULL or text `datetime()` cannot parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedExpiry {
    /// RFC3339 UTC, fractional seconds as the source had them.
    At(String),
    Missing,
    /// The raw SQLite text, for the log line the importer writes.
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredScore {
    pub score: AccountScore,
    /// RFC3339 UTC.
    pub scored_at: String,
    pub scoring_generation: String,
    pub valid_until: ExportedExpiry,
}

/// An amplification event — someone quoted or reposted the protected user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AmplificationEvent {
    pub id: i64,
    pub event_type: String,
    pub amplifier_did: String,
    pub amplifier_handle: String,
    pub original_post_uri: String,
    pub amplifier_post_uri: Option<String>,
    pub amplifier_text: Option<String>,
    pub detected_at: String,
    pub followers_fetched: bool,
    pub followers_scored: bool,
    /// The protected user's original post text (for pair display and NLI scoring)
    pub original_post_text: Option<String>,
    /// NLI contextual hostility score for this interaction pair
    pub context_score: Option<f64>,
}

/// A user-provided label for an account (ground truth for scoring accuracy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserLabel {
    pub user_did: String,
    pub target_did: String,
    /// One of: "high", "elevated", "watch", "safe"
    pub label: String,
    pub labeled_at: String,
    pub notes: Option<String>,
}

/// A topic-matched post pair for NLI scoring (second-degree accounts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredPair {
    pub id: i64,
    pub user_did: String,
    pub target_did: String,
    pub target_post_text: String,
    pub target_post_uri: String,
    pub user_post_text: String,
    pub user_post_uri: String,
    pub similarity: f64,
    pub context_score: Option<f64>,
    pub created_at: String,
}

/// A row from the users table, used by admin endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
    pub did: String,
    pub handle: String,
    pub created_at: String,
    pub last_login_at: Option<String>,
}

/// Accuracy metrics comparing predicted tiers to user labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccuracyMetrics {
    pub total_labeled: i64,
    pub exact_matches: i64,
    pub overscored: i64,
    pub underscored: i64,
    pub accuracy: f64,
}

/// Threat tier thresholds — these are configurable constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThreatTier {
    Low,
    Watch,
    Elevated,
    High,
    /// Outside the ordered Low→High scale. The account's posts were in a
    /// language our English-only models cannot assess, so no score was produced
    /// (#222). Constructed only at the coverage gate, never from a score.
    NotAssessed,
}

impl ThreatTier {
    /// Floor of the Elevated tier (#344 Task 7). Named so the refresh
    /// candidate query (`threat_score >= ELEVATED_MIN`) and `from_score`
    /// cannot drift apart — the candidate set is defined as "High or
    /// Elevated by score", which only holds if both read the same constant.
    pub const ELEVATED_MIN: f64 = 15.0;

    /// Determine the tier from a threat score (0-100).
    ///
    /// Thresholds are tuned for the multiplicative scoring formula where
    /// overlap amplifies toxicity. A score of 35+ requires meaningful
    /// toxicity combined with topic proximity — the core threat signal.
    /// Low-toxicity accounts stay low regardless of topic overlap.
    ///
    /// Never returns `NotAssessed`; that tier is set at the coverage gate, not
    /// derived from a score.
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 35.0 => ThreatTier::High,
            s if s >= Self::ELEVATED_MIN => ThreatTier::Elevated,
            s if s >= 8.0 => ThreatTier::Watch,
            _ => ThreatTier::Low,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ThreatTier::Low => "Low",
            ThreatTier::Watch => "Watch",
            ThreatTier::Elevated => "Elevated",
            ThreatTier::High => "High",
            ThreatTier::NotAssessed => "NotAssessed",
        }
    }
}

impl std::fmt::Display for ThreatTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// An amplification event that has not been written to the database yet.
///
/// Owned rather than borrowed because the amplification pipeline builds these
/// across `.await` points while iterating borrowed events — owned fields keep
/// the payload `'static` and sidestep the borrow tangle. `detected_at` is
/// deliberately absent: the database default stamps it, matching the
/// single-row `insert_amplification_event` path.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAmplificationEvent {
    pub event_type: String,
    pub amplifier_did: String,
    pub amplifier_handle: String,
    pub original_post_uri: String,
    pub amplifier_post_uri: Option<String>,
    pub amplifier_text: Option<String>,
    pub original_post_text: Option<String>,
    pub context_score: Option<f64>,
}
