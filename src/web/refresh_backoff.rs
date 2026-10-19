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
pub const STREAK_KEY: &str = crate::db::REFRESH_FAILURE_STREAK_KEY;

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
/// and so is an HTTP status code the message names as one (`HTTP 404`,
/// `503 Service Unavailable`) — see [`is_http_status`].
pub fn failure_signature(message: &str) -> String {
    // A word is what a person would select with a double-click: letters,
    // digits, `-` and `_`. Separators (`:`, spaces, `/`) are copied through.
    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '-' || c == '_'
    }

    // A URL's query string carries the account (`?actor=alice.bsky.social`)
    // and paging values; the scheme, host and path — WHICH endpoint failed —
    // are the cause (Codex review). Mask the query, keep the rest.
    static URL_QUERY: OnceLock<regex_lite::Regex> = OnceLock::new();
    let url_query = URL_QUERY.get_or_init(|| {
        regex_lite::Regex::new(r"(https?://[^\s?#()]+)\?[^\s()]*").expect("valid pattern")
    });
    let message = url_query.replace_all(message, "${1}?<query>");

    // Split into alternating words and separators, keeping both, so each
    // word can be judged with the word before it and the text after it.
    let mut parts: Vec<(bool, String)> = Vec::new();
    for c in message.chars() {
        match parts.last_mut() {
            Some((w, text)) if *w == is_word(c) => text.push(c),
            _ => parts.push((is_word(c), c.to_string())),
        }
    }

    let mut out = String::with_capacity(message.len().min(SIGNATURE_MAX_CHARS * 2));
    let mut prev_word: Option<&str> = None;
    let mut i = 0;
    while i < parts.len() {
        let (word, text) = (&parts[i].0, parts[i].1.as_str());
        if !*word {
            out.push_str(text);
            // `@alice.bsky.social` names a user, not a cause (#387 review):
            // an AppView outage would otherwise give every user a different
            // signature, and the breaker could never see it as one fault.
            if text.ends_with('@') {
                if let Some(end) = dotted_name_end(&parts, i + 1) {
                    out.push_str("<handle>");
                    i = end;
                    continue;
                }
            }
            i += 1;
            continue;
        }
        // `did:plc:…` / `did:web:host` — the method is kept, the id is not.
        // A did:web id can contain no digit at all, so the digit rule alone
        // would leave it in.
        if text == "did"
            && parts.get(i + 1).is_some_and(|(_, t)| t == ":")
            && parts.get(i + 2).is_some_and(|(w, _)| *w)
            && parts.get(i + 3).is_some_and(|(_, t)| t == ":")
            && parts.get(i + 4).is_some_and(|(w, _)| *w)
        {
            out.push_str("did:");
            out.push_str(&parts[i + 2].1);
            out.push_str(":<id>");
            i = dotted_name_end(&parts, i + 4).unwrap_or(i + 5);
            prev_word = None;
            continue;
        }
        if !text.chars().any(|c| c.is_ascii_digit())
            || is_http_status(text, prev_word, &parts[i + 1..])
        {
            out.push_str(text);
        } else if text.chars().count() >= 8 {
            out.push_str("<id>");
        } else {
            let mut in_digits = false;
            for c in text.chars() {
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
        prev_word = Some(text);
        i += 1;
    }
    crate::output::truncate_chars(out.trim(), SIGNATURE_MAX_CHARS)
}

/// If `parts[start..]` begins with a dotted name — `word(.word)*` — return
/// the index just past it. A bare word with no dot is not a hostname or a
/// handle, so it returns `None`.
fn dotted_name_end(parts: &[(bool, String)], start: usize) -> Option<usize> {
    if !parts.get(start).is_some_and(|(w, _)| *w) {
        return None;
    }
    let mut end = start + 1;
    let mut dotted = false;
    while parts.get(end).is_some_and(|(_, t)| t == ".")
        && parts.get(end + 1).is_some_and(|(w, _)| *w)
    {
        end += 2;
        dotted = true;
    }
    dotted.then_some(end)
}

/// Is `word` an HTTP status code — one the MESSAGE identifies as such?
///
/// A status is part of the cause (a 404 for one user and a 503 for another
/// are different faults), but a three-digit COUNT is not (CodeRabbit,
/// PR #143): "after 200 rows" and "after 300 rows" are one cause. So the
/// number must be a real status AND be named as one, either by the word
/// before it (`HTTP 404`, `status 503`) or the way `reqwest::StatusCode`
/// prints itself, the number followed by its own reason (`502 Bad Gateway`).
fn is_http_status(word: &str, prev_word: Option<&str>, after: &[(bool, String)]) -> bool {
    // Cheap rejections first: this runs for every word containing a digit.
    if word.len() != 3 || !word.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let Ok(code) = word.parse::<u16>() else {
        return false;
    };
    let Ok(status) = reqwest::StatusCode::from_u16(code) else {
        return false;
    };
    let named_before = prev_word
        .is_some_and(|p| p.eq_ignore_ascii_case("http") || p.eq_ignore_ascii_case("status"));
    // Only as much of the following text as a reason phrase can span: the
    // longest, "Request Header Fields Too Large", is 5 words + 4 spaces.
    let rest: String = after.iter().take(12).map(|(_, t)| t.as_str()).collect();
    let reason_after = status.canonical_reason().is_some_and(|reason| {
        rest.strip_prefix(' ')
            .is_some_and(|r| r.starts_with(reason))
    });
    named_before || reason_after
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
    /// Did some real work but was ALSO stopped by a transient classifier
    /// failure: the fault may still be live. Never closes the breaker, and —
    /// unlike `Neutral` — never frees the probe slot early either; an open
    /// breaker waits out another cool-down (CodeRabbit, PR #143).
    Inconclusive,
}

#[derive(Debug)]
enum BreakerState {
    /// Counting: for each cause, the distinct users that failed with it since
    /// the last success, each with when it last did (only those inside
    /// [`BREAKER_WINDOW`] count).
    Closed(Failures),
    /// Refusing until `until`; then one probe is let through, tagged `probe`,
    /// and `until` is pushed out a further `cooldown`, so concurrent
    /// refreshes keep waiting for the probe's answer.
    Open {
        cause: String,
        users: usize,
        until: DateTime<Utc>,
        cooldown: Duration,
        probe: Option<u64>,
        /// Probes that failed with some OTHER cause, counted the way the
        /// closed state counts: if a new cause fails two different probes it
        /// is deployment-wide too, and takes over the breaker.
        rivals: Failures,
    },
}

/// For each cause, the distinct users that failed with it, and when.
type Failures = HashMap<String, HashMap<String, DateTime<Utc>>>;

/// Record `user` failing with `cause` at `now` and return how many distinct
/// users failed with it inside [`BREAKER_WINDOW`].
fn count_failure(failures: &mut Failures, cause: &str, user: &str, now: DateTime<Utc>) -> usize {
    let users = failures.entry(cause.to_string()).or_default();
    let window = chrono_dur(BREAKER_WINDOW);
    users.retain(|_, at| now - *at < window);
    users.insert(user.to_string(), now);
    users.len()
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
                ..
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
                BreakerReport::Neutral | BreakerReport::Inconclusive => return,
                BreakerReport::Success => BreakerState::Closed(HashMap::new()),
                BreakerReport::Failure(cause) => {
                    let users = count_failure(failures, cause, user_did, now);
                    if users < BREAKER_TRIP_USERS {
                        return;
                    }
                    tracing::error!(
                        cause,
                        users,
                        cooldown_secs = BREAKER_COOLDOWN.as_secs(),
                        "refresh circuit breaker OPEN: different users are failing with the \
                         same cause, so the fault is the deployment's — no further refresh \
                         will run until the cool-down ends"
                    );
                    BreakerState::Open {
                        cause: cause.to_string(),
                        users,
                        until: now + chrono_dur(BREAKER_COOLDOWN),
                        cooldown: BREAKER_COOLDOWN,
                        probe: None,
                        rivals: HashMap::new(),
                    }
                }
            },
            BreakerState::Open {
                cause: open_cause,
                users,
                until,
                cooldown,
                probe,
                rivals,
            } => {
                // Only the current probe speaks for an open breaker.
                if admission.probe.is_none() || admission.probe != *probe {
                    return;
                }
                match report {
                    BreakerReport::Inconclusive => {
                        // Possibly still broken: re-arm the same cool-down.
                        // Not doubled — the probe did get some work through.
                        *probe = None;
                        *until = now + chrono_dur(*cooldown);
                        return;
                    }
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
                            rivals: std::mem::take(rivals),
                        }
                    }
                    BreakerReport::Failure(cause) => {
                        // A different failure is NOT evidence the original
                        // fault is gone: the probe may have failed before it
                        // ever reached the faulty part — a Bluesky hiccup, a
                        // database blip in setup (#387 review). Stay open and
                        // free the slot so the next refresh probes now. But
                        // count it: a new cause that fails two different
                        // probes is deployment-wide in its own right, and
                        // takes over the breaker with a fresh cool-down
                        // rather than handing out free probes forever.
                        let users = count_failure(rivals, cause, user_did, now);
                        if users < BREAKER_TRIP_USERS {
                            tracing::warn!(
                                open_cause = %open_cause,
                                cause,
                                "refresh circuit breaker: the probe failed differently — \
                                 staying open; the next refresh probes"
                            );
                            *probe = None;
                            *until = now;
                            return;
                        }
                        tracing::error!(
                            previous = %open_cause,
                            cause,
                            users,
                            "refresh circuit breaker: a new cause is failing every probe — \
                             it is the deployment's fault now"
                        );
                        BreakerState::Open {
                            cause: cause.to_string(),
                            users,
                            until: now + chrono_dur(BREAKER_COOLDOWN),
                            cooldown: BREAKER_COOLDOWN,
                            probe: None,
                            rivals: HashMap::new(),
                        }
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
        // After the word HTTP or status.
        assert_ne!(
            failure_signature("RunPod /status HTTP 404 for job sync-1a2b3c4d9"),
            failure_signature("RunPod /status HTTP 503 for job sync-1a2b3c4d9"),
        );
        // As reqwest's StatusCode prints itself: the number, then its reason.
        assert_ne!(
            failure_signature("PLC directory returned 404 Not Found for did:plc:abc"),
            failure_signature("PLC directory returned 503 Service Unavailable for did:plc:abc"),
        );
    }

    /// CodeRabbit (PR #143): only a number the message identifies as a status
    /// is kept. A count that happens to be three digits is masked like any
    /// other, or a changing row total would restart the backoff.
    #[test]
    fn signature_masks_three_digit_counts() {
        assert_eq!(
            failure_signature("gave up after 200 rows"),
            failure_signature("gave up after 300 rows")
        );
        assert_eq!(
            failure_signature("within 180s"),
            failure_signature("within 240s")
        );
        // A number followed by the WRONG reason is not a status line.
        assert_eq!(
            failure_signature("got 404 Bad things"),
            failure_signature("got 405 Bad things")
        );
    }

    /// #387 review: errors name the user (`@handle`, a DID). Unmasked, an
    /// AppView outage gives every user a different cause and the breaker can
    /// never see it as one fault.
    #[test]
    fn signature_masks_handles_and_dids() {
        assert_eq!(
            failure_signature("Failed to fetch feed for @alice.bsky.social: 502 Bad Gateway"),
            failure_signature("Failed to fetch feed for @bob.example.com: 502 Bad Gateway"),
        );
        assert_eq!(
            failure_signature("Failed to fetch DID document for did:web:alice.example"),
            failure_signature("Failed to fetch DID document for did:web:bob.example.org"),
        );
        assert_eq!(
            failure_signature("PLC directory returned 404 Not Found for did:plc:abcdefgh"),
            failure_signature("PLC directory returned 404 Not Found for did:plc:zyxwvuts"),
        );
        // The status still tells them apart, and so does what was being done.
        assert_ne!(
            failure_signature("Failed to fetch feed for @alice.bsky.social: 502 Bad Gateway"),
            failure_signature("Failed to fetch feed for @alice.bsky.social: 404 Not Found"),
        );
        assert_ne!(
            failure_signature("Failed to fetch feed for @a.bsky.social"),
            failure_signature("Failed to resolve handle @a.bsky.social"),
        );
        // Codex review: the same handle also rides in request URLs. Query
        // values are masked; the endpoint — what was being fetched — is kept.
        let a = failure_signature(
            "error sending request for url (https://public.api.bsky.app/xrpc/app.bsky.feed.getAuthorFeed?actor=alice.bsky.social&limit=100)",
        );
        let b = failure_signature(
            "error sending request for url (https://public.api.bsky.app/xrpc/app.bsky.feed.getAuthorFeed?actor=bob.example.com&limit=100)",
        );
        assert_eq!(a, b);
        assert!(a.contains("getAuthorFeed"), "{a}");
        assert_ne!(
            a,
            failure_signature(
                "error sending request for url (https://public.api.bsky.app/xrpc/app.bsky.actor.getProfile?actor=alice.bsky.social)",
            )
        );
        // An address-like word with no handle shape after `@` is left alone.
        assert!(failure_signature("x @ y").contains("@ y"));
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

    /// #387 review: a probe that fails for a DIFFERENT reason may never have
    /// reached the faulty component (a Bluesky hiccup, a database blip during
    /// setup), so it is no evidence the original fault is gone. The breaker
    /// stays open and the slot is freed for the next refresh to probe at once.
    #[test]
    fn a_probe_failing_differently_does_not_close_it() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let p = t0() + mins(61);
        let probe = b.admit(p).unwrap();
        b.report(probe, "did:a", BreakerReport::Failure("other"), p);
        let next = b.admit(p).expect("the slot is free again");
        assert!(next.is_probe(), "still open: the next refresh is a probe");
        b.report(next, "did:b", BreakerReport::Success, p);
        assert!(!b.admit(p).unwrap().is_probe(), "closed by a real success");
    }

    /// …but a NEW cause that fails two different probes is itself
    /// deployment-wide: the breaker switches to it, with a fresh cool-down,
    /// rather than handing out probe after probe for free.
    #[test]
    fn a_new_cause_failing_two_probes_takes_over_the_breaker() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let p = t0() + mins(61);
        let first = b.admit(p).unwrap();
        b.report(first, "did:a", BreakerReport::Failure("other"), p);
        let second = b.admit(p).unwrap();
        b.report(second, "did:b", BreakerReport::Failure("other"), p);
        let refused = b.admit(p).unwrap_err();
        assert_eq!(refused.cause, "other");
        assert!(b.admit(p + mins(59)).is_err());
        assert!(b.admit(p + mins(61)).unwrap().is_probe());
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

    /// CodeRabbit (PR #143): a probe that scored something but was ALSO
    /// interrupted by the classifier proves the fault may still be live. It
    /// must not close the breaker, and must not free the slot for an instant
    /// re-probe either: the breaker stays open for another (undoubled)
    /// cool-down.
    #[test]
    fn an_inconclusive_probe_keeps_it_open_for_another_cooldown() {
        let b = RefreshBreaker::new("d");
        trip(&b, "c");
        let p = t0() + mins(61);
        let probe = b.admit(p).unwrap();
        b.report(probe, "did:x", BreakerReport::Inconclusive, p);
        assert!(b.admit(p).is_err(), "not closed, slot not freed");
        assert!(b.admit(p + mins(59)).is_err(), "a full cool-down");
        assert!(
            b.admit(p + mins(61)).unwrap().is_probe(),
            "and not a doubled one"
        );
    }

    /// While closed, an inconclusive run neither counts nor resets anything.
    #[test]
    fn an_inconclusive_run_is_ignored_while_closed() {
        let b = RefreshBreaker::new("d");
        fail(&b, "did:a", "c", t0());
        b.report(
            Admission::normal(),
            "did:x",
            BreakerReport::Inconclusive,
            t0(),
        );
        fail(&b, "did:b", "c", t0());
        assert!(b.admit(t0()).is_err(), "the count survived");
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
