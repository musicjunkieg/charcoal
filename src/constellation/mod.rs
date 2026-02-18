// Constellation — supplementary backlink index for AT Protocol.
//
// Constellation (constellation.microcosm.blue) indexes all quote-posts and
// reposts across the AT Protocol network. This catches amplification events
// that notification polling misses — for example, engagement from blocked/muted
// accounts, or events that fall between polling intervals.
//
// Constellation is supplementary, not a replacement. It runs on a Raspberry Pi
// with ~6 days of indexed data, so availability and coverage are limited.

pub mod client;
