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

use std::collections::HashMap;
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

/// How far apart two failures can be and still count toward tripping the
/// breaker. A deployment-wide fault fails every refresh it touches within
/// hours; two identical failures days apart, with quiet nights between, are
/// two per-user faults (#387 review). A day: the nightly cadence.
pub const BREAKER_WINDOW: Duration = Duration::from_secs(BACKOFF_CAP_HOURS * 3600);

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
/// read as a new cause. The words that say WHAT failed are kept as they are,
/// and so is a bare HTTP status code (`404` vs `503`).
pub fn failure_signature(message: &str) -> String {
    // A word is what a person would select with a double-click: letters,
    // digits, `-` and `_`. Separators (`:`, spaces, `/`) are copied through.
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '-' || c == '_'
    }
    // A bare 100–599 is almost always an HTTP status, and a status is part of
    // the cause: a 404 for one user and a 503 for another are different
    // faults, and merging them would open the breaker on a per-user error.
    fn is_http_status(word: &str) -> bool {
        word.len() == 3
            && word.bytes().all(|b| b.is_ascii_digit())
            && (b'1'..=b'5').contains(&word.as_bytes()[0])
    }
    fn mask(word: &str, out: &mut String) {
        if !word.chars().any(|c| c.is_ascii_digit()) || is_http_status(word) {
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

/// What [`RefreshBreaker::admit`] hands a refresh it lets run.
///
/// Carries whether this refresh is the probe. While the breaker is open only
/// the probe's report may change it: a refresh that started before the
/// breaker opened and finishes after has not tested anything, and letting it
/// close the breaker — or stretch the cool-down — would act on stale evidence
/// (#387 review; CodeRabbit, PR #143).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Admission {
    probe: Option<u64>,
}

impl Admission {
    /// An ordinary admission — the breaker was closed.
    pub fn normal() -> Self {
        Self::default()
    }

    pub fn is_probe(&self) -> bool {
        self.probe.is_some()
    }
}

/// What a finished refresh tells the breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerReport<'a> {
    /// Failed with this cause (a [`failure_signature`]).
    Failure(&'a str),
    /// Did real work end to end — scored accounts — so whatever the breaker
    /// guards against is not stopping refreshes.
    Success,
    /// Proves nothing either way: deferred, nothing due, or paused resumably.
    Neutral,
}

#[derive(Debug)]
enum BreakerState {
    /// Counting: for each cause, the distinct users that failed with it since
    /// the last success, each with when it last did (only those inside
    /// [`BREAKER_WINDOW`] count).
    Closed(HashMap<String, HashMap<String, DateTime<Utc>>>),
    /// Refusing until `until`; then one probe is let through, tagged `probe`,
    /// and `until` is pushed out a further `cooldown`, so concurrent
    /// refreshes keep waiting for the probe's answer.
    Open {
        cause: String,
        users: usize,
        until: DateTime<Utc>,
        cooldown: Duration,
        probe: Option<u64>,
    },
}

/// The process-wide refresh circuit breaker. See the module docs.
pub struct RefreshBreaker {
    deploy: String,
    state: Mutex<(BreakerState, u64)>,
}

impl RefreshBreaker {
    pub fn new(deploy: impl Into<String>) -> Self {
        Self {
            deploy: deploy.into(),
            state: Mutex::new((BreakerState::Closed(HashMap::new()), 0)),
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

    /// The state and the probe counter, surviving a poisoned lock: every write
    /// is one assignment, so a panic elsewhere cannot leave it half-written,
    /// and refusing all refreshes forever would be the worse failure.
    fn state(&self) -> std::sync::MutexGuard<'_, (BreakerState, u64)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Before a refresh does any work. `Err` = do not run it.
    pub fn admit(&self, now: DateTime<Utc>) -> Result<Admission, CircuitOpen> {
        let mut guard = self.state();
        let (state, next_probe) = &mut *guard;
        match state {
            BreakerState::Closed(_) => Ok(Admission::normal()),
            BreakerState::Open {
                cause,
                users,
                until,
                cooldown,
                probe,
            } => {
                if now < *until {
                    return Err(CircuitOpen {
                        cause: cause.clone(),
                        users: *users,
                    });
                }
                // This caller is the probe. Re-arm so the next caller waits
                // for its answer; a probe that never reports (a panic) is
                // superseded by the next one, one cool-down later.
                *next_probe += 1;
                *probe = Some(*next_probe);
                *until = now + chrono_dur(*cooldown);
                tracing::warn!(
                    cause = %cause,
                    "refresh circuit breaker: cool-down over, letting one refresh through as a probe"
                );
                Ok(Admission {
                    probe: Some(*next_probe),
                })
            }
        }
    }

    /// A refresh admitted with `admission` finished.
    pub fn report(
        &self,
        admission: Admission,
        user_did: &str,
        report: BreakerReport<'_>,
        now: DateTime<Utc>,
    ) {
        let mut guard = self.state();
        let (state, _) = &mut *guard;
        let next = match state {
            BreakerState::Closed(failures) => match report {
                BreakerReport::Neutral => return,
                BreakerReport::Success => BreakerState::Closed(HashMap::new()),
                BreakerReport::Failure(cause) => {
                    let users = failures.entry(cause.to_string()).or_default();
                    let window = chrono_dur(BREAKER_WINDOW);
                    users.retain(|_, at| now - *at < window);
                    users.insert(user_did.to_string(), now);
                    if users.len() < BREAKER_TRIP_USERS {
                        return;
                    }
                    tracing::error!(
                        cause,
                        users = users.len(),
                        cooldown_secs = BREAKER_COOLDOWN.as_secs(),
                        "refresh circuit breaker OPEN: different users are failing with the \
                         same cause, so the fault is the deployment's — no further refresh \
                         will run until the cool-down ends"
                    );
                    BreakerState::Open {
                        cause: cause.to_string(),
                        users: users.len(),
                        until: now + chrono_dur(BREAKER_COOLDOWN),
                        cooldown: BREAKER_COOLDOWN,
                        probe: None,
                    }
                }
            },
            BreakerState::Open {
                cause: open_cause,
                users,
                until,
                cooldown,
                probe,
            } => {
                // Only the current probe speaks for an open breaker.
                if admission.probe.is_none() || admission.probe != *probe {
                    return;
                }
                match report {
                    BreakerReport::Neutral => {
                        // The probe tested nothing. Free the slot so the next
                        // refresh can probe now, rather than a cool-down later.
                        *probe = None;
                        *until = now;
                        return;
                    }
                    BreakerReport::Success => {
                        tracing::info!("refresh circuit breaker closed: the probe succeeded");
                        BreakerState::Closed(HashMap::new())
                    }
                    BreakerReport::Failure(cause) if open_cause.as_str() == cause => {
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
                            probe: None,
                        }
                    }
                    BreakerReport::Failure(cause) => {
                        // The deployment-wide fault is gone; this is a new
                        // one, counted from scratch like any other.
                        tracing::warn!(
                            previous = %open_cause,
                            cause,
                            "refresh circuit breaker closed: the probe got past the original fault"
                        );
                        let mut failures = HashMap::new();
                        failures.insert(
                            cause.to_string(),
                            HashMap::from([(user_did.to_string(), now)]),
                        );
                        BreakerState::Closed(failures)
                    }
                }
            }
        };
        *state = next;
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

    /// #387 review: a status code is part of the cause. One user's 404 and
    /// another's 503 must not read as one deployment-wide fault.
    #[test]
    fn signature_keeps_http_status_codes() {
        assert_ne!(
            failure_signature("RunPod /status HTTP 404 for job sync-1a2b3c4d9"),
            failure_signature("RunPod /status HTTP 503 for job sync-1a2b3c4d9"),
        );
        assert_ne!(
            failure_signature("getAuthorFeed: 400"),
            failure_signature("getAuthorFeed: 502"),
        );
        // A duration is still masked, and so is a number out of status range.
        assert_eq!(
            failure_signature("within 180s"),
            failure_signature("within 240s")
        );
        assert_eq!(
            failure_signature("after 900 rows"),
            failure_signature("after 750 rows")
        );
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

    /// A failure reported by an ordinary (non-probe) refresh.
    fn fail(b: &RefreshBreaker, user: &str, cause: &str, at: DateTime<Utc>) {
        b.report(Admission::normal(), user, BreakerReport::Failure(cause), at);
    }

    fn trip(b: &RefreshBreaker, cause: &str) {
        fail(b, "did:a", cause, t0());
        fail(b, "did:b", cause, t0());
        assert!(b.admit(t0()).is_err(), "tripped");
    }

    #[test]
    fn breaker_starts_closed() {
        let a = RefreshBreaker::new("d").admit(t0()).unwrap();
        assert!(!a.is_probe());
    }

    #[test]
    fn one_user_failing_repeatedly_does_not_trip_it() {
        let b = RefreshBreaker::new("d");
        for _ in 0..5 {
            fail(&b, "did:a", "policy mismatch", t0());
        }
        assert!(
            b.admit(t0()).is_ok(),
            "a per-user fault is the backoff's job"
        );
    }

    #[test]
    fn different_causes_do_not_trip_it() {
        let b = RefreshBreaker::new("d");
        fail(&b, "did:a", "policy mismatch", t0());
        fail(&b, "did:b", "appview 502", t0());
        assert!(b.admit(t0()).is_ok());
    }

    #[test]
    fn two_users_with_the_same_cause_open_it() {
        let b = RefreshBreaker::new("d");
        fail(&b, "did:a", "policy mismatch", t0());
        fail(&b, "did:b", "policy mismatch", t0());
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
        fail(&b, "did:a", "policy mismatch", t0());
        b.report(Admission::normal(), "did:x", BreakerReport::Success, t0());
        fail(&b, "did:b", "policy mismatch", t0());
        assert!(b.admit(t0()).is_ok());
    }

    /// #387 review: nothing due, a deferral or a resumable pause tested
    /// nothing, so it must not wipe the count of identical failures.
    #[test]
    fn a_neutral_outcome_in_between_does_not_reset_the_count() {
        let b = RefreshBreaker::new("d");
        fail(&b, "did:a", "policy mismatch", t0());
        b.report(Admission::normal(), "did:x", BreakerReport::Neutral, t0());
        fail(&b, "did:b", "policy mismatch", t0());
        assert!(b.admit(t0()).is_err());
    }

    /// #387 review: two same-cause failures a day or more apart, with
    /// nothing but quiet nights between, are two per-user faults, not one
    /// deployment-wide one.
    #[test]
    fn failures_more_than_a_window_apart_do_not_trip_it() {
        let b = RefreshBreaker::new("d");
        fail(&b, "did:a", "c", t0());
        fail(&b, "did:b", "c", t0() + chrono::Duration::hours(25));
        assert!(b.admit(t0() + chrono::Duration::hours(25)).is_ok());
        // Within the window it still trips.
        fail(&b, "did:c", "c", t0() + chrono::Duration::hours(26));
        assert!(b.admit(t0() + chrono::Duration::hours(26)).is_err());
    }

    #[test]
    fn after_the_cooldown_exactly_one_probe_is_let_through() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        assert!(b.admit(t0() + mins(59)).is_err(), "still cooling down");
        let later = t0() + mins(61);
        assert!(b.admit(later).unwrap().is_probe(), "the probe");
        assert!(
            b.admit(later).is_err(),
            "concurrent refreshes wait for the probe's answer"
        );
    }

    #[test]
    fn a_successful_probe_closes_it() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let later = t0() + mins(61);
        let probe = b.admit(later).unwrap();
        b.report(probe, "did:c", BreakerReport::Success, later);
        assert!(!b.admit(later).unwrap().is_probe());
        assert!(b.admit(later).is_ok());
    }

    #[test]
    fn a_failed_probe_reopens_it_for_twice_as_long_capped_at_a_day() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        // Probe 1 at +61 min fails → open 2 h.
        let p1 = t0() + mins(61);
        let probe = b.admit(p1).unwrap();
        b.report(probe, "did:c", BreakerReport::Failure("c"), p1);
        assert!(b.admit(p1 + mins(119)).is_err());
        let mut at = p1 + mins(121);
        let mut probe = b.admit(at).unwrap();
        assert!(probe.is_probe(), "probe 2 after 2 h");
        // Keep failing: the cool-down never exceeds a day.
        for _ in 0..10 {
            b.report(probe, "did:c", BreakerReport::Failure("c"), at);
            at += chrono::Duration::hours(24) + mins(1);
            probe = b.admit(at).expect("a probe at least daily");
            assert!(probe.is_probe());
        }
    }

    #[test]
    fn a_probe_failing_differently_closes_it_and_counts_afresh() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let p = t0() + mins(61);
        let probe = b.admit(p).unwrap();
        b.report(probe, "did:a", BreakerReport::Failure("other"), p);
        assert!(b.admit(p).is_ok(), "the original fault is gone");
        fail(&b, "did:b", "other", p);
        assert_eq!(b.admit(p).unwrap_err().cause, "other");
    }

    /// #387 review + CodeRabbit (PR #143): a refresh that started before the
    /// breaker opened and reports after has tested nothing. None of its
    /// reports — success, a new cause, or the same cause — may change an open
    /// breaker.
    #[test]
    fn only_the_probe_can_change_an_open_breaker() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let stale = Admission::normal();
        b.report(stale, "did:x", BreakerReport::Success, t0());
        b.report(stale, "did:y", BreakerReport::Failure("other"), t0());
        b.report(stale, "did:z", BreakerReport::Failure("c"), t0());
        assert!(b.admit(t0() + mins(59)).is_err(), "still open");
        assert!(
            b.admit(t0() + mins(61)).unwrap().is_probe(),
            "and the cool-down was not stretched"
        );
    }

    /// A superseded probe (one that never reported, then a cool-down passed
    /// and another was admitted) cannot speak for the new one either.
    #[test]
    fn a_superseded_probe_cannot_close_it() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let first = b.admit(t0() + mins(61)).unwrap();
        let second = b.admit(t0() + mins(122)).unwrap();
        assert!(first.is_probe() && second.is_probe() && first != second);
        b.report(first, "did:x", BreakerReport::Success, t0() + mins(123));
        assert!(b.admit(t0() + mins(123)).is_err(), "still open");
    }

    /// A probe that proved nothing frees the slot at once, rather than
    /// costing a whole extra cool-down.
    #[test]
    fn a_neutral_probe_lets_the_next_refresh_probe_immediately() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let p = t0() + mins(61);
        let probe = b.admit(p).unwrap();
        b.report(probe, "did:x", BreakerReport::Neutral, p);
        assert!(b.admit(p).unwrap().is_probe());
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
