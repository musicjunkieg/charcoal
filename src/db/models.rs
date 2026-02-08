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
    pub fn from_score(score: f64) -> Self {
        match score {
            s if s >= 76.0 => ThreatTier::High,
            s if s >= 51.0 => ThreatTier::Elevated,
            s if s >= 26.0 => ThreatTier::Watch,
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
