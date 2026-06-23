// Scan-phase staging types — the three-phase scan pipeline work queue.
//
// Phase A (Gather): collect posts, compute behavioral signals, enqueue classifier work.
// Phase B (Burst): classifier verdict for each queued post.
// Phase C (Score): read back staged data and compute final AccountScore.

pub mod burst;
pub mod finalize;
pub mod gather;
pub mod staging;
