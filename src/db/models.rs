// Data models — Rust structs that map to database rows.
//
// These are the types that flow through the application. They're separate
// from the database queries so other modules can use them without depending
// on rusqlite directly.

use serde::{Deserialize, Serialize};

/// A scored account in the threat list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountScore {
    pub did: String,
    pub handle: String,
    pub toxicity_score: Option<f64>,
    pub topic_overlap: Option<f64>,
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
}

/// A single post with its toxicity score, kept as evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToxicPost {
    pub text: String,
    pub toxicity: f64,
    pub uri: String,
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
}

impl ThreatTier {
    /// Determine the tier from a threat score (0-100).
    ///
    /// Thresholds are tuned for the multiplicative scoring formula where
    /// overlap amplifies toxicity. A score of 35+ requires meaningful
    /// toxicity combined with topic proximity — the core threat signal.
    /// Low-toxicity accounts stay low regardless of topic overlap.
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 35.0 => ThreatTier::High,
            s if s >= 15.0 => ThreatTier::Elevated,
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
        }
    }
}

impl std::fmt::Display for ThreatTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}
