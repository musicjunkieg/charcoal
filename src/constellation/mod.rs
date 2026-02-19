// Constellation — primary backlink index for AT Protocol amplification detection.
//
// Constellation (constellation.microcosm.blue) indexes all quote-posts and
// reposts across the AT Protocol network. It catches amplification events
// including those from blocked/muted accounts and has 1+ years of indexed data.
//
// Constellation is the primary amplification detection source — it replaced
// notification polling (which required authentication).

pub mod client;
