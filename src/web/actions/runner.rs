//! Background batch runner for tier-based mute/block actions (#315).
//!
//! Task 8 fills in the body: resuming queued/running batches left behind by
//! the previous deploy, then draining the wake channel as new batches are
//! inserted. For now this only wires the channel so `AppState.action_wake`
//! has something to send into.

use super::super::AppState;

/// Spawn the runner and return the sender half of its wake channel.
///
/// The receiver is intentionally dropped here — there is no runner loop yet.
/// `_state` is unused until Task 8 gives the loop something to act on.
pub fn spawn_runner(_state: AppState) -> tokio::sync::mpsc::Sender<i64> {
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    tx
}
