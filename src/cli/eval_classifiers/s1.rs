//! Zentropi's hosted CoPE-B-S1 multilabel API, for the eval only (#407).
//!
//! One request per post asks several yes/no policy questions at once. The
//! `master` question (Bryan's whole policy, unchanged) is the verdict; in
//! bundle mode six reason questions ride along to explain it. The hosted API
//! returns a 0/1 label plus a confidence per question — not the raw
//! probability — which is enough to compare models, not to tune thresholds.
//!
//! Parsing is strict because the API is a preview whose "specs may change":
//! a missing or odd field is an error, never a quiet "no".

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::content_hash;
use super::reasons::{ReasonAnswer, ReasonSet, ReasonSource, REASON_KEYS};
use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};

pub const S1_API_URL: &str = "https://api.zentropi.ai/v1/multilabel";
pub const S1_MODEL_ID: &str = "cope-b-s1";
const MASTER_KEY: &str = "master";
/// Seven questions take longer than one; 10 s (the /v1/label client's
/// timeout) would turn slow-but-fine answers into errors.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Compiled in, so the policy a verdict was made under is exactly the one
/// its version hash describes. Editing a file takes effect on the next build.
const MASTER_POLICY: &str = include_str!("../../../gpu/cope-b-runpod/policy.txt");
const REASON_POLICIES: [(&str, &str); 6] = [
    (
        "personal_attack",
        include_str!("../../../evals/classifiers/reasons/personal_attack.txt"),
    ),
    (
        "identity_hate",
        include_str!("../../../evals/classifiers/reasons/identity_hate.txt"),
    ),
    (
        "mockery",
        include_str!("../../../evals/classifiers/reasons/mockery.txt"),
    ),
    (
        "threat",
        include_str!("../../../evals/classifiers/reasons/threat.txt"),
    ),
    (
        "pile_on",
        include_str!("../../../evals/classifiers/reasons/pile_on.txt"),
    ),
    (
        "bad_faith",
        include_str!("../../../evals/classifiers/reasons/bad_faith.txt"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum S1Mode {
    /// The master policy alone: the model card's most accurate mode.
    Master,
    /// Master plus the six reasons in one request.
    Bundle,
}

/// A stable version for a set of (key, policy) pairs. Any change to any
/// text or key changes it, which is what makes the verdict cache re-judge.
fn version_of(labelers: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (k, t) in labelers {
        s.push_str(k);
        s.push('\0');
        s.push_str(t);
        s.push('\0');
    }
    format!("s1-{}", content_hash(&s))
}

static MASTER_VERSION: LazyLock<String> = LazyLock::new(|| version_of(&S1Mode::Master.labelers()));
static BUNDLE_VERSION: LazyLock<String> = LazyLock::new(|| version_of(&S1Mode::Bundle.labelers()));

impl S1Mode {
    /// (key, policy text) for every question this mode asks, master first.
    pub fn labelers(self) -> Vec<(&'static str, &'static str)> {
        let mut v = vec![(MASTER_KEY, MASTER_POLICY)];
        if self == S1Mode::Bundle {
            v.extend(REASON_POLICIES);
        }
        v
    }

    /// `&'static` because `ToxicityClassifier::policy_version` requires it;
    /// a `LazyLock` static computes each once instead of leaking per client.
    pub fn policy_version(self) -> &'static str {
        match self {
            S1Mode::Master => MASTER_VERSION.as_str(),
            S1Mode::Bundle => BUNDLE_VERSION.as_str(),
        }
    }

    pub fn eval_name(self) -> &'static str {
        match self {
            S1Mode::Master => "s1-master",
            S1Mode::Bundle => "s1-bundle",
        }
    }
}

pub fn build_request(mode: S1Mode, content: &str) -> serde_json::Value {
    let labelers: Vec<serde_json::Value> = mode
        .labelers()
        .into_iter()
        .map(|(key, text)| serde_json::json!({"key": key, "criteria_text": text}))
        .collect();
    serde_json::json!({
        "model": S1_MODEL_ID,
        "content_text": content,
        "labelers": labelers,
    })
}

/// One item of the response array. Fields are Options so a missing one
/// is reported by us, per key, instead of failing the whole array decode.
#[derive(Deserialize)]
struct Item {
    key: Option<String>,
    label: Option<String>,
    confidence: Option<f64>,
}

/// The decoded answer to one post.
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    pub toxic: bool,
    pub confidence: f32,
    /// `None` in master mode.
    pub reasons: Option<ReasonSet>,
}

fn decode(item: &Item) -> Result<(bool, f32)> {
    let fired = match item.label.as_deref() {
        Some("1") => true,
        Some("0") => false,
        other => bail!("label {other:?} is not \"0\" or \"1\""),
    };
    let c = item.confidence.context("confidence missing")?;
    if !(c.is_finite() && (0.0..=1.0).contains(&c)) {
        bail!("confidence {c} is outside 0–1");
    }
    Ok((fired, c as f32))
}

pub fn parse_response(mode: S1Mode, body: &str) -> Result<Parsed> {
    let items: Vec<Item> =
        serde_json::from_str(body).context("S1 response is not the documented array")?;
    let allowed: Vec<&str> = mode.labelers().into_iter().map(|(k, _)| k).collect();
    let mut by_key: HashMap<String, Item> = HashMap::new();
    for item in items {
        let key = item.key.clone().context("S1 response item without a key")?;
        if !allowed.contains(&key.as_str()) {
            bail!("S1 response has unexpected key {key:?}");
        }
        if by_key.insert(key.clone(), item).is_some() {
            bail!("S1 response repeats key {key:?}");
        }
    }
    let master = by_key
        .get(MASTER_KEY)
        .context("S1 response has no master answer")?;
    let (toxic, confidence) = decode(master).context("S1 master answer")?;
    let reasons = (mode == S1Mode::Bundle).then(|| {
        let mut answers = BTreeMap::new();
        let mut complete = true;
        for k in REASON_KEYS {
            // A bad reason never sinks the verdict: keep the master, and let
            // the report leave this post out of its reason rates.
            match by_key.get(k).map(decode) {
                Some(Ok((fired, confidence))) => {
                    answers.insert(k.to_string(), ReasonAnswer { fired, confidence });
                }
                _ => complete = false,
            }
        }
        ReasonSet { answers, complete }
    });
    Ok(Parsed {
        toxic,
        confidence,
        reasons,
    })
}

/// Whether a failed attempt is worth repeating.
enum Attempt {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

pub struct S1Client {
    http: reqwest::Client,
    api_key: String,
    api_url: String,
    mode: S1Mode,
    initial_backoff: Duration,
    /// Reasons from each answered post, keyed by its exact text, until the
    /// runner takes them (see [`ReasonSource`]).
    reasons: Mutex<HashMap<String, ReasonSet>>,
}

impl S1Client {
    pub fn new(api_key: String, mode: S1Mode) -> Result<Self> {
        Self::with_api_url(api_key, mode, S1_API_URL.to_string())
    }

    /// The endpoint injected, so tests can point at a local mock server.
    pub fn with_api_url(api_key: String, mode: S1Mode, api_url: String) -> Result<Self> {
        if api_key.trim().is_empty() {
            bail!("ZENTROPI_API_KEY is empty");
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("building the S1 HTTP client")?;
        Ok(Self {
            http,
            api_key,
            api_url,
            mode,
            initial_backoff: INITIAL_BACKOFF,
            reasons: Mutex::new(HashMap::new()),
        })
    }

    /// Shorter waits for tests; production keeps 500 ms doubling.
    pub fn with_backoff(mut self, initial: Duration) -> Self {
        self.initial_backoff = initial;
        self
    }

    async fn send_once(&self, content: &str) -> std::result::Result<String, Attempt> {
        let resp = self
            .http
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&build_request(self.mode, content))
            .send()
            .await
            .map_err(|e| Attempt::Retry(anyhow::Error::new(e).context("reaching the S1 API")))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }
        let err = anyhow::anyhow!("S1 API error {status}: {body}");
        if status.as_u16() == 429 || status.is_server_error() {
            Err(Attempt::Retry(err))
        } else {
            Err(Attempt::Fatal(err))
        }
    }

    async fn call(&self, content: &str) -> Result<Parsed> {
        let mut backoff = self.initial_backoff;
        let mut attempt = 0;
        let body = loop {
            match self.send_once(content).await {
                Ok(body) => break body,
                Err(Attempt::Retry(e)) if attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::debug!(attempt, error = %e, "S1 retry");
                    tokio::time::sleep(backoff).await;
                    backoff *= 2;
                }
                Err(Attempt::Retry(e)) | Err(Attempt::Fatal(e)) => return Err(e),
            }
        };
        // A shape error is not retried: the same request gets the same shape.
        parse_response(self.mode, &body)
    }
}

#[async_trait::async_trait]
impl ToxicityClassifier for S1Client {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let start = Instant::now();
        let parsed = self.call(content).await?;
        if let Some(r) = parsed.reasons {
            self.reasons
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(content.to_string(), r);
        }
        Ok(ClassifierVerdict {
            toxic_token: parsed.toxic,
            confidence: parsed.confidence,
            latency_ms: start.elapsed().as_millis().try_into().unwrap_or(u32::MAX),
            model_id: S1_MODEL_ID.to_string(),
            policy_version: self.policy_version().to_string(),
        })
    }
    fn name(&self) -> &'static str {
        self.mode.eval_name()
    }
    fn model_id(&self) -> &'static str {
        S1_MODEL_ID
    }
    fn policy_version(&self) -> &'static str {
        self.mode.policy_version()
    }
    /// The hosted API already decided 0/1; take its answer as given.
    fn threshold(&self) -> f32 {
        0.0
    }
}

impl ReasonSource for S1Client {
    fn take_reasons(&self, content: &str) -> Option<ReasonSet> {
        self.reasons
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::classifier::ToxicityClassifier;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn item(key: &str, label: &str, conf: f64) -> serde_json::Value {
        json!({"key": key, "label": label, "confidence": conf, "compute_time": 0.3})
    }
    fn bundle_body(master: &str) -> serde_json::Value {
        let mut v = vec![item("master", master, 0.9)];
        for k in REASON_KEYS {
            v.push(item(k, if k == "mockery" { "1" } else { "0" }, 0.8));
        }
        serde_json::Value::Array(v)
    }

    #[test]
    fn master_mode_sends_one_labeler_bundle_sends_seven() {
        let m = build_request(S1Mode::Master, "hi");
        assert_eq!(m["model"], "cope-b-s1");
        assert_eq!(m["content_text"], "hi");
        assert_eq!(m["labelers"].as_array().unwrap().len(), 1);
        assert_eq!(m["labelers"][0]["key"], "master");
        assert!(m["labelers"][0]["criteria_text"]
            .as_str()
            .unwrap()
            .contains("toxic toward a PARTICIPANT"));
        let b = build_request(S1Mode::Bundle, "hi");
        let keys: Vec<&str> = b["labelers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys[0], "master");
        assert_eq!(&keys[1..], &REASON_KEYS);
    }

    #[test]
    fn a_good_bundle_response_parses_master_and_reasons() {
        let p = parse_response(S1Mode::Bundle, &bundle_body("1").to_string()).unwrap();
        assert!(p.toxic);
        assert!((p.confidence - 0.9).abs() < 1e-6);
        let r = p.reasons.unwrap();
        assert!(r.complete);
        assert_eq!(r.fired(), vec!["mockery"]);
    }

    #[test]
    fn master_mode_has_no_reasons() {
        let body = json!([item("master", "0", 0.7)]).to_string();
        let p = parse_response(S1Mode::Master, &body).unwrap();
        assert!(!p.toxic);
        assert!(p.reasons.is_none());
    }

    #[test]
    fn a_missing_or_garbled_master_is_an_error_never_a_no() {
        let no_master = json!([item("threat", "0", 0.7)]).to_string();
        assert!(parse_response(S1Mode::Bundle, &no_master).is_err());
        for bad in [
            item("master", "maybe", 0.9),
            item("master", "1", 1.5),
            json!({"key": "master", "label": "1"}),
        ] {
            let body = serde_json::Value::Array(vec![bad]).to_string();
            assert!(parse_response(S1Mode::Master, &body).is_err(), "{body}");
        }
        assert!(
            parse_response(S1Mode::Master, r#"{"label":"1"}"#).is_err(),
            "not an array"
        );
    }

    #[test]
    fn a_missing_or_garbled_reason_keeps_the_master_and_marks_reasons_incomplete() {
        let mut v = bundle_body("1");
        let arr = v.as_array_mut().unwrap();
        arr.retain(|i| i["key"] != "pile_on");
        arr.iter_mut().find(|i| i["key"] == "threat").unwrap()["label"] = json!("yes");
        let p = parse_response(S1Mode::Bundle, &v.to_string()).unwrap();
        assert!(p.toxic);
        let r = p.reasons.unwrap();
        assert!(!r.complete);
        assert!(!r.answers.contains_key("pile_on") && !r.answers.contains_key("threat"));
        assert_eq!(r.fired(), vec!["mockery"]);
    }

    #[test]
    fn an_unknown_or_repeated_key_means_the_shape_changed() {
        let unknown = json!([item("master", "1", 0.9), item("surprise", "1", 0.9)]).to_string();
        assert!(parse_response(S1Mode::Master, &unknown).is_err());
        let reason_in_master_mode =
            json!([item("master", "1", 0.9), item("threat", "1", 0.9)]).to_string();
        assert!(parse_response(S1Mode::Master, &reason_in_master_mode).is_err());
        let twice = json!([item("master", "1", 0.9), item("master", "0", 0.9)]).to_string();
        assert!(parse_response(S1Mode::Master, &twice).is_err());
    }

    /// Spec §4.6: editing a reason re-judges the bundle only.
    #[test]
    fn a_reason_edit_changes_the_bundle_version_not_the_master_version() {
        let master = [("master", "M")];
        let bundle = [("master", "M"), ("threat", "T")];
        let edited = [("master", "M"), ("threat", "T2")];
        assert_ne!(version_of(&bundle), version_of(&edited));
        assert_eq!(version_of(&master), version_of(&[("master", "M")]));
        assert_ne!(
            S1Mode::Master.policy_version(),
            S1Mode::Bundle.policy_version()
        );
        assert!(S1Mode::Bundle.policy_version().starts_with("s1-"));
    }

    fn client(server: &MockServer, mode: S1Mode) -> S1Client {
        S1Client::with_api_url("k".into(), mode, format!("{}/v1/multilabel", server.uri()))
            .unwrap()
            .with_backoff(std::time::Duration::from_millis(1))
    }

    #[tokio::test]
    async fn classify_returns_the_master_verdict_and_stashes_the_reasons() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/multilabel"))
            .and(header("authorization", "Bearer k"))
            .respond_with(ResponseTemplate::new(200).set_body_json(bundle_body("1")))
            .mount(&server)
            .await;
        let c = client(&server, S1Mode::Bundle);
        let v = c.classify("post").await.unwrap();
        assert!(v.toxic_token);
        assert_eq!(v.model_id, S1_MODEL_ID);
        assert_eq!(v.policy_version, c.policy_version());
        assert_eq!(c.take_reasons("post").unwrap().fired(), vec!["mockery"]);
        assert!(c.take_reasons("post").is_none(), "taken once");
    }

    #[tokio::test]
    async fn a_rate_limit_is_retried_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([item("master", "0", 0.8)])),
            )
            .with_priority(2)
            .mount(&server)
            .await;
        let v = client(&server, S1Mode::Master).classify("p").await.unwrap();
        assert!(!v.toxic_token);
    }

    #[tokio::test]
    async fn exhausted_retries_are_an_error_not_clean() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(4) // 1 try + 3 retries
            .mount(&server)
            .await;
        let err = client(&server, S1Mode::Master)
            .classify("p")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("503"), "{err:#}");
    }

    #[tokio::test]
    async fn a_client_error_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .expect(1)
            .mount(&server)
            .await;
        assert!(client(&server, S1Mode::Master).classify("p").await.is_err());
    }

    #[test]
    fn an_empty_key_is_refused() {
        let err = S1Client::new(String::new(), S1Mode::Master).err().unwrap();
        assert!(format!("{err}").contains("ZENTROPI_API_KEY"));
    }
}
