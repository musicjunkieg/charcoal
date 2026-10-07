//! Backoff and circuit breaking for the refresh job (#387).
//!
//! The cost incident behind #385: an unset policy-version variable made every
//! refresh refuse, and `REFRESH_RETRY_HOURS = 1` retried each of nine users
//! every hour for sixteen days — 216 identical, paid-for failures a day. The
//! refusal was right; repeating it at full price was not. Two caps, one per
//! scale of fault:
//!
//! - **Per user — backoff.** Consecutive failures with the same cause stretch
//!   the retry: 1 h, 2 h, 4 h … capped at 24 h. Anything that is not a failure
//!   ends the streak, as does a new cause or a new deployment (an operator who
//!   fixed the problem and redeployed must not wait out yesterday's backoff).
//!   Stored in `scan_state`, so it survives restarts without a migration.
//!
//! - **Per deployment — circuit breaker.** When different users fail with the
//!   SAME cause, the fault is the deployment's, not theirs, and running every
//!   remaining user only pays to rediscover it. The breaker opens and further
//!   refreshes stop before any Bluesky or GPU work, saying why. After a
//!   cool-down one refresh is let through as a probe; success closes the
//!   breaker, the same failure re-opens it for twice as long (capped at 24 h).
//!   Process-local on purpose: a deploy — which is how a config change lands
//!   on Railway — starts closed.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The `scan_state` key holding a user's [`FailureStreak`] as JSON.
pub const STREAK_KEY: &str = "refresh_failure_streak";

/// The longest a failing user waits between attempts. The nightly cadence:
/// a fault nobody has fixed costs one attempt a day per user, not 24.
pub const BACKOFF_CAP_HOURS: u64 = 24;

/// Distinct users failing with one cause before the breaker opens.
///
/// Two, not "every user in the tick": refresh claims run independently, so
/// there is no tick-wide view to wait for, and production has three users —
/// a threshold of three would only ever open after the money was spent.
pub const BREAKER_TRIP_USERS: usize = 2;

/// How long the breaker stays open the first time it trips.
pub const BREAKER_COOLDOWN: Duration = Duration::from_secs(3600);

/// The causes a signature keeps, in characters. Long enough to tell causes
/// apart, short enough that a pasted response body cannot bloat `scan_state`.
const SIGNATURE_MAX_CHARS: usize = 240;

/// The retry delay after the `streak`-th consecutive identical failure:
/// 1 h, 2 h, 4 h … capped at [`BACKOFF_CAP_HOURS`].
pub fn backoff_delay(streak: u32) -> Duration {
    // `streak - 1` doublings. Past 2^5 = 32 h the cap has long applied, so
    // clamping the exponent keeps the shift from ever overflowing.
    let doublings = streak.saturating_sub(1).min(16);
    let hours = (1u64 << doublings).min(BACKOFF_CAP_HOURS);
    Duration::from_secs(hours * 3600)
}

/// A failure message reduced to its cause: the parts that vary per attempt —
/// job ids, DIDs, counts, durations — are masked, so two attempts that failed
/// for the same reason compare equal.
///
/// Word by word: a long word containing a digit is an identifier (a RunPod
/// job id, a DID's key, a UUID) and becomes `<id>`; in any other word, digit
/// runs become `#` (`180s` → `#s`), so a changed timeout or count does not
/// read as a new cause. The words that say WHAT failed are kept as they are.
pub fn failure_signature(message: &str) -> String {
    // A word is what a person would select with a double-click: letters,
    // digits, `-` and `_`. Separators (`:`, spaces, `/`) are copied through.
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '-' || c == '_'
    }
    fn mask(word: &str, out: &mut String) {
        if !word.chars().any(|c| c.is_ascii_digit()) {
            out.push_str(word);
        } else if word.chars().count() >= 8 {
            out.push_str("<id>");
        } else {
            let mut in_digits = false;
            for c in word.chars() {
                if c.is_ascii_digit() {
                    if !in_digits {
                        out.push('#');
                    }
                    in_digits = true;
                } else {
                    out.push(c);
                    in_digits = false;
                }
            }
        }
    }

    let mut out = String::with_capacity(message.len().min(SIGNATURE_MAX_CHARS * 2));
    let mut word = String::new();
    for c in message.chars() {
        if is_word(c) {
            word.push(c);
        } else {
            mask(&word, &mut out);
            word.clear();
            out.push(c);
        }
    }
    mask(&word, &mut out);
    crate::output::truncate_chars(out.trim(), SIGNATURE_MAX_CHARS)
}

/// This process's deployment, for resetting backoff on a deploy.
///
/// `RAILWAY_DEPLOYMENT_ID` changes on every deploy — including the redeploy a
/// variable change triggers — and survives a crash-restart of the same
/// deployment. Anywhere else, one id per process.
pub fn deploy_id() -> String {
    deploy_id_from(std::env::var("RAILWAY_DEPLOYMENT_ID").ok())
}

fn deploy_id_from(railway: Option<String>) -> String {
    static PROCESS: OnceLock<String> = OnceLock::new();
    match railway.map(|s| s.trim().to_string()) {
        Some(id) if !id.is_empty() => id,
        _ => PROCESS
            .get_or_init(|| {
                format!(
                    "process-{}-{}",
                    std::process::id(),
                    Utc::now().timestamp_nanos_opt().unwrap_or_default()
                )
            })
            .clone(),
    }
}

/// A user's run of consecutive failures with one cause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureStreak {
    pub count: u32,
    pub cause: String,
    /// The deployment the streak was counted under.
    pub deploy: String,
}

impl FailureStreak {
    /// The streak after one more failure with `cause` under `deploy`.
    pub fn after_failure(prev: Option<&FailureStreak>, cause: &str, deploy: &str) -> Self {
        let count = match prev {
            Some(p) if p.cause == cause && p.deploy == deploy => p.count.saturating_add(1),
            _ => 1,
        };
        Self {
            count,
            cause: cause.to_string(),
            deploy: deploy.to_string(),
        }
    }
}

/// The breaker refused to run a refresh.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "refresh skipped: a deployment-wide fault — {users} users failed with the same cause, \
     so this one was not attempted: {cause}"
)]
pub struct CircuitOpen {
    pub cause: String,
    pub users: usize,
}

#[derive(Debug)]
enum BreakerState {
    /// Counting: for each cause, the distinct users that failed with it since
    /// the last success.
    Closed(HashMap<String, HashSet<String>>),
    /// Refusing until `until`; then one probe is let through and `until` is
    /// pushed out a further `cooldown`, so concurrent refreshes keep waiting
    /// for the probe's answer rather than all becoming probes at once.
    Open {
        cause: String,
        users: usize,
        until: DateTime<Utc>,
        cooldown: Duration,
    },
}

/// The process-wide refresh circuit breaker. See the module docs.
pub struct RefreshBreaker {
    deploy: String,
    state: Mutex<BreakerState>,
}

impl RefreshBreaker {
    pub fn new(deploy: impl Into<String>) -> Self {
        Self {
            deploy: deploy.into(),
            state: Mutex::new(BreakerState::Closed(HashMap::new())),
        }
    }

    /// The breaker every production refresh shares.
    pub fn global() -> &'static RefreshBreaker {
        static GLOBAL: OnceLock<RefreshBreaker> = OnceLock::new();
        GLOBAL.get_or_init(|| RefreshBreaker::new(deploy_id()))
    }

    /// The deployment this breaker — and this process — belongs to.
    pub fn deploy(&self) -> &str {
        &self.deploy
    }

    /// The state, surviving a poisoned lock: a panic elsewhere while holding
    /// it cannot leave a half-written breaker (every write is one assignment),
    /// and refusing all refreshes forever would be the worse failure.
    fn state(&self) -> std::sync::MutexGuard<'_, BreakerState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Before a refresh does any work. `Err` = do not run it.
    pub fn admit(&self, now: DateTime<Utc>) -> Result<(), CircuitOpen> {
        let mut state = self.state();
        match &mut *state {
            BreakerState::Closed(_) => Ok(()),
            BreakerState::Open {
                cause,
                users,
                until,
                cooldown,
            } => {
                if now < *until {
                    return Err(CircuitOpen {
                        cause: cause.clone(),
                        users: *users,
                    });
                }
                // This caller is the probe. Re-arm so the next caller waits
                // for its answer; a probe that never reports (a panic) gets
                // another chance one cool-down later.
                *until = now + chrono_dur(*cooldown);
                tracing::warn!(
                    cause = %cause,
                    "refresh circuit breaker: cool-down over, letting one refresh through as a probe"
                );
                Ok(())
            }
        }
    }

    /// A refresh failed with `cause` (a [`failure_signature`]).
    pub fn record_failure(&self, user_did: &str, cause: &str, now: DateTime<Utc>) {
        let mut state = self.state();
        let next = match &mut *state {
            BreakerState::Closed(failures) => {
                let users = failures.entry(cause.to_string()).or_default();
                users.insert(user_did.to_string());
                if users.len() < BREAKER_TRIP_USERS {
                    return;
                }
                tracing::error!(
                    cause,
                    users = users.len(),
                    cooldown_secs = BREAKER_COOLDOWN.as_secs(),
                    "refresh circuit breaker OPEN: different users are failing with the same \
                     cause, so the fault is the deployment's — no further refresh will run \
                     until the cool-down ends"
                );
                BreakerState::Open {
                    cause: cause.to_string(),
                    users: users.len(),
                    until: now + chrono_dur(BREAKER_COOLDOWN),
                    cooldown: BREAKER_COOLDOWN,
                }
            }
            BreakerState::Open {
                cause: open_cause,
                users,
                cooldown,
                ..
            } => {
                if open_cause.as_str() == cause {
                    // The probe failed the same way: still broken. Wait longer.
                    let cooldown =
                        (*cooldown * 2).min(Duration::from_secs(BACKOFF_CAP_HOURS * 3600));
                    tracing::error!(
                        cause,
                        cooldown_secs = cooldown.as_secs(),
                        "refresh circuit breaker: the probe failed the same way — staying open"
                    );
                    BreakerState::Open {
                        cause: cause.to_string(),
                        users: *users,
                        until: now + chrono_dur(cooldown),
                        cooldown,
                    }
                } else {
                    // The deployment-wide fault is gone; this is a new one,
                    // counted from scratch like any other.
                    tracing::warn!(
                        previous = %open_cause,
                        cause,
                        "refresh circuit breaker closed: the probe got past the original fault"
                    );
                    let mut failures = HashMap::new();
                    failures.insert(cause.to_string(), HashSet::from([user_did.to_string()]));
                    BreakerState::Closed(failures)
                }
            }
        };
        *state = next;
    }

    /// A refresh got past whatever the breaker was guarding against.
    pub fn record_success(&self) {
        let mut state = self.state();
        if matches!(&*state, BreakerState::Open { .. }) {
            tracing::info!("refresh circuit breaker closed: the probe succeeded");
        }
        *state = BreakerState::Closed(HashMap::new());
    }
}

fn chrono_dur(d: Duration) -> chrono::Duration {
    // Every duration here is at most BACKOFF_CAP_HOURS — far inside range.
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::hours(BACKOFF_CAP_HOURS as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(n: u64) -> Duration {
        Duration::from_secs(n * 3600)
    }

    #[test]
    fn backoff_doubles_from_one_hour_and_caps_at_a_day() {
        // 0 is not a real streak; it must not mean "retry immediately".
        assert_eq!(backoff_delay(0), h(1));
        assert_eq!(backoff_delay(1), h(1));
        assert_eq!(backoff_delay(2), h(2));
        assert_eq!(backoff_delay(3), h(4));
        assert_eq!(backoff_delay(4), h(8));
        assert_eq!(backoff_delay(5), h(16));
        assert_eq!(backoff_delay(6), h(24));
        // No overflow however long the fault lasts.
        assert_eq!(backoff_delay(64), h(24));
        assert_eq!(backoff_delay(u32::MAX), h(24));
    }

    #[test]
    fn signature_masks_what_varies_between_attempts() {
        let a = failure_signature(
            "RunPod job sync-419fcecd-1b2c-4d5e-8f90-123456789abc did not complete within 180s",
        );
        let b = failure_signature(
            "RunPod job sync-77aa0b3e-9c8d-4e7f-a1b2-cdef01234567 did not complete within 240s",
        );
        assert_eq!(a, b);
        // The words that say WHAT failed survive.
        assert!(a.contains("did not complete within"), "{a}");
        let did_a = failure_signature("policy mismatch for did:plc:h3wpawnrlptr4534chevddo6");
        let did_b = failure_signature("policy mismatch for did:plc:zz9ab8cd7ef6gh5ij4kl3mn2");
        assert_eq!(did_a, did_b);
    }

    #[test]
    fn signature_keeps_different_causes_apart() {
        assert_ne!(
            failure_signature("CoPE-B policy version mismatch: expected policy-ddac468"),
            failure_signature("getAuthorFeed: 502 from the AppView"),
        );
    }

    #[test]
    fn signature_is_bounded_and_utf8_safe() {
        let long = "é".repeat(5_000);
        let s = failure_signature(&long);
        assert!(s.chars().count() <= SIGNATURE_MAX_CHARS + 3, "{}", s.len());
    }

    #[test]
    fn streak_counts_identical_failures_and_restarts_on_change() {
        let one = FailureStreak::after_failure(None, "policy mismatch", "d1");
        assert_eq!(one.count, 1);
        let two = FailureStreak::after_failure(Some(&one), "policy mismatch", "d1");
        assert_eq!(two.count, 2);
        // A different cause is a different problem: start over.
        let other = FailureStreak::after_failure(Some(&two), "appview 502", "d1");
        assert_eq!((other.count, other.cause.as_str()), (1, "appview 502"));
        // A new deployment may have fixed it: start over.
        let redeployed = FailureStreak::after_failure(Some(&two), "policy mismatch", "d2");
        assert_eq!((redeployed.count, redeployed.deploy.as_str()), (1, "d2"));
    }

    #[test]
    fn streak_round_trips_through_scan_state_json() {
        let s = FailureStreak {
            count: 3,
            cause: "x".into(),
            deploy: "d".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<FailureStreak>(&json).unwrap(), s);
    }

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-06T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn mins(n: i64) -> chrono::Duration {
        chrono::Duration::minutes(n)
    }

    #[test]
    fn breaker_starts_closed() {
        assert_eq!(RefreshBreaker::new("d").admit(t0()), Ok(()));
    }

    #[test]
    fn one_user_failing_repeatedly_does_not_trip_it() {
        let b = RefreshBreaker::new("d");
        for _ in 0..5 {
            b.record_failure("did:a", "policy mismatch", t0());
        }
        assert_eq!(
            b.admit(t0()),
            Ok(()),
            "a per-user fault is the backoff's job"
        );
    }

    #[test]
    fn different_causes_do_not_trip_it() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "policy mismatch", t0());
        b.record_failure("did:b", "appview 502", t0());
        assert_eq!(b.admit(t0()), Ok(()));
    }

    #[test]
    fn two_users_with_the_same_cause_open_it() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "policy mismatch", t0());
        b.record_failure("did:b", "policy mismatch", t0());
        let refused = b.admit(t0() + mins(1)).unwrap_err();
        assert_eq!(refused.cause, "policy mismatch");
        assert_eq!(refused.users, 2);
        // And it says so in words an operator can act on.
        let msg = refused.to_string();
        assert!(
            msg.contains("deployment-wide") && msg.contains("policy mismatch"),
            "{msg}"
        );
    }

    #[test]
    fn a_success_in_between_resets_the_count() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "policy mismatch", t0());
        b.record_success();
        b.record_failure("did:b", "policy mismatch", t0());
        assert_eq!(b.admit(t0()), Ok(()));
    }

    #[test]
    fn after_the_cooldown_exactly_one_probe_is_let_through() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "c", t0());
        b.record_failure("did:b", "c", t0());
        assert!(b.admit(t0() + mins(59)).is_err(), "still cooling down");
        let later = t0() + mins(61);
        assert_eq!(b.admit(later), Ok(()), "the probe");
        assert!(
            b.admit(later).is_err(),
            "concurrent refreshes wait for the probe's answer"
        );
    }

    #[test]
    fn a_successful_probe_closes_it() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "c", t0());
        b.record_failure("did:b", "c", t0());
        let later = t0() + mins(61);
        assert_eq!(b.admit(later), Ok(()));
        b.record_success();
        assert_eq!(b.admit(later), Ok(()));
        assert_eq!(b.admit(later), Ok(()));
    }

    #[test]
    fn a_failed_probe_reopens_it_for_twice_as_long_capped_at_a_day() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "c", t0());
        b.record_failure("did:b", "c", t0());
        // Probe 1 at +61 min fails → open 2 h.
        let p1 = t0() + mins(61);
        assert_eq!(b.admit(p1), Ok(()));
        b.record_failure("did:c", "c", p1);
        assert!(b.admit(p1 + mins(119)).is_err());
        let p2 = p1 + mins(121);
        assert_eq!(b.admit(p2), Ok(()), "probe 2 after 2 h");
        // Keep failing: the cool-down never exceeds a day.
        let mut at = p2;
        for _ in 0..10 {
            b.record_failure("did:c", "c", at);
            at += chrono::Duration::hours(24) + mins(1);
            assert_eq!(b.admit(at), Ok(()), "a probe at least daily");
        }
    }

    #[test]
    fn a_probe_failing_differently_closes_it_and_counts_afresh() {
        let b = RefreshBreaker::new("d");
        b.record_failure("did:a", "c", t0());
        b.record_failure("did:b", "c", t0());
        let p = t0() + mins(61);
        assert_eq!(b.admit(p), Ok(()));
        b.record_failure("did:a", "other", p);
        assert_eq!(b.admit(p), Ok(()), "the original fault is gone");
        b.record_failure("did:b", "other", p);
        assert_eq!(b.admit(p).unwrap_err().cause, "other");
    }

    #[test]
    fn deploy_identity_prefers_railway_and_falls_back_per_process() {
        assert_eq!(deploy_id_from(Some("abc-123".into())), "abc-123");
        assert_eq!(deploy_id_from(Some("  ".into())), deploy_id_from(None));
        // Stable within a process, so a crash-restart is the only fallback reset.
        assert_eq!(deploy_id_from(None), deploy_id_from(None));
        assert!(deploy_id_from(None).starts_with("process-"));
    }
}
