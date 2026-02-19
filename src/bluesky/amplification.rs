// Amplification event types — shared between Constellation and the pipeline.
//
// The AmplificationNotification struct represents a quote or repost of the
// protected user's content. It's produced by the Constellation backlink client
// and consumed by the amplification pipeline.

/// An amplification event detected from Constellation backlinks.
#[derive(Debug, Clone)]
pub struct AmplificationNotification {
    pub event_type: String, // "quote" or "repost"
    pub amplifier_did: String,
    pub amplifier_handle: String,
    /// The protected user's post that was amplified
    pub original_post_uri: Option<String>,
    /// The amplifier's post URI (for quotes — the quote-post itself)
    pub amplifier_post_uri: String,
    pub indexed_at: String,
}
