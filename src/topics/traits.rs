// Topic extractor trait — swap-ready abstraction.
//
// Like the ToxicityScorer trait, this lets us swap out the topic extraction
// approach without changing the rest of the pipeline. The default implementation
// uses TF-IDF, but this could be replaced with embeddings-based clustering later.

use super::fingerprint::TopicFingerprint;
use anyhow::Result;

/// Trait for extracting a topic fingerprint from a collection of posts.
pub trait TopicExtractor {
    /// Analyze a set of post texts and produce a topic fingerprint.
    fn extract(&self, posts: &[String]) -> Result<TopicFingerprint>;
}
