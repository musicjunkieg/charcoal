//! Every verdict ever paid for, one JSONL file per model, plus the identity
//! (model id + policy version) of the last run. A verdict is reused, reported
//! or labelled from only while it is CURRENT: same post text, same model,
//! same policy (Codex review, rounds 1 and 2).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use super::content_hash;
use super::data::FixtureRow;
use crate::toxicity::classifier::ClassifierVerdict;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictRecord {
    pub id: String,
    /// The eval's name for the model (file name), e.g. "cope-b".
    pub model: String,
    pub content_hash: String,
    /// What the backend reported serving.
    pub model_id: String,
    pub policy_version: String,
    /// The model's OWN binary answer. `None` = failed, timed out, or the
    /// backend reported an unexpected identity. Never derived from confidence.
    pub called_toxic: Option<bool>,
    pub error: Option<String>,
    pub latency_ms: u32,
    pub input_chars: usize,
    /// Estimated spend for this post, from pricing.json.
    pub est_usd: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct Identity<'a> {
    pub model_id: &'a str,
    pub policy_version: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityRecord {
    pub model_id: String,
    pub policy_version: String,
}

impl VerdictRecord {
    pub fn from_verdict(
        id: &str,
        model: &str,
        content: &str,
        v: &ClassifierVerdict,
        est_usd: f64,
    ) -> Self {
        Self {
            id: id.to_string(),
            model: model.to_string(),
            content_hash: content_hash(content),
            model_id: v.model_id.clone(),
            policy_version: v.policy_version.clone(),
            called_toxic: Some(v.toxic_token),
            error: None,
            latency_ms: v.latency_ms,
            input_chars: content.chars().count(),
            est_usd,
        }
    }

    pub fn from_error(
        id: &str,
        model: &str,
        content: &str,
        who: Identity<'_>,
        detail: &str,
        est_usd: f64,
    ) -> Self {
        Self {
            id: id.to_string(),
            model: model.to_string(),
            content_hash: content_hash(content),
            model_id: who.model_id.to_string(),
            policy_version: who.policy_version.to_string(),
            called_toxic: None,
            error: Some(detail.to_string()),
            latency_ms: 0,
            input_chars: content.chars().count(),
            est_usd,
        }
    }

    fn matches(&self, content: &str, model_id: &str, policy_version: &str) -> bool {
        self.content_hash == content_hash(content)
            && self.model_id == model_id
            && self.policy_version == policy_version
    }

    /// A successful, current verdict: nothing to re-judge.
    pub fn reusable(&self, content: &str, who: Identity<'_>) -> bool {
        self.called_toxic.is_some() && self.matches(content, who.model_id, who.policy_version)
    }
}

fn path_for(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{model}.jsonl"))
}

fn identity_path(dir: &Path, model: &str) -> PathBuf {
    dir.join(format!("{model}.identity.json"))
}

pub fn append(dir: &Path, rec: &VerdictRecord) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let path = path_for(dir, &rec.model);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(rec)?)?;
    Ok(())
}

pub fn load(dir: &Path, model: &str) -> Result<HashMap<String, VerdictRecord>> {
    let path = path_for(dir, model);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let rec: VerdictRecord = serde_json::from_str(l)
            .with_context(|| format!("{} line {}", path.display(), i + 1))?;
        out.insert(rec.id.clone(), rec);
    }
    Ok(out)
}

/// The identity the last run verified, so report and label — which build no
/// backend — know which verdicts are current.
pub fn save_identity(dir: &Path, model: &str, who: Identity<'_>) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let rec = IdentityRecord {
        model_id: who.model_id.to_string(),
        policy_version: who.policy_version.to_string(),
    };
    let path = identity_path(dir, model);
    std::fs::write(&path, serde_json::to_string(&rec)?)
        .with_context(|| format!("writing {}", path.display()))
}

pub fn load_identity(dir: &Path, model: &str) -> Result<Option<IdentityRecord>> {
    let path = identity_path(dir, model);
    match std::fs::read_to_string(&path) {
        Ok(t) => Ok(Some(
            serde_json::from_str(&t).with_context(|| format!("parsing {}", path.display()))?,
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// The ONE gate every consumer goes through: only verdicts for posts still
/// in the pool, with the pool's present text, from the identity last
/// verified. Errors that pass are kept — they count against coverage.
pub fn current(
    recs: HashMap<String, VerdictRecord>,
    pool: &[FixtureRow],
    who: Option<&IdentityRecord>,
) -> HashMap<String, VerdictRecord> {
    let Some(who) = who else {
        return HashMap::new();
    };
    let text: HashMap<&str, &str> = pool
        .iter()
        .map(|r| (r.id.as_str(), r.content.as_str()))
        .collect();
    recs.into_iter()
        .filter(|(id, r)| {
            text.get(id.as_str())
                .is_some_and(|content| r.matches(content, &who.model_id, &who.policy_version))
        })
        .collect()
}

/// `model`'s current verdicts, read from disk: load + last verified identity +
/// [`current`]. The label and report commands both call this, and the
/// end-to-end test calls it too, so a consumer that skipped the gate would be
/// a visible change to one function rather than a hand-built closure.
pub fn current_for(
    dir: &Path,
    model: &str,
    pool: &[FixtureRow],
) -> Result<HashMap<String, VerdictRecord>> {
    Ok(current(
        load(dir, model)?,
        pool,
        load_identity(dir, model)?.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(toxic: bool, conf: f32) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: toxic,
            confidence: conf,
            latency_ms: 12,
            model_id: "m-1".into(),
            policy_version: "p-1".into(),
        }
    }
    const WHO: Identity<'static> = Identity {
        model_id: "m-1",
        policy_version: "p-1",
    };
    fn row(id: &str, content: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": content})).unwrap()
    }

    /// Codex review, critical: CoPE-B reports confidence as exp(logprob) of the
    /// emitted token over the WHOLE vocabulary. A "clean" answer with 0.1 must
    /// stay clean — the repository's own RunPod fixture shape.
    #[test]
    fn the_models_own_answer_is_kept_whatever_its_confidence() {
        assert_eq!(
            VerdictRecord::from_verdict("a", "cope-b", "x", &verdict(false, 0.1), 0.0).called_toxic,
            Some(false)
        );
        assert_eq!(
            VerdictRecord::from_verdict("b", "cope-b", "x", &verdict(true, 0.2), 0.0).called_toxic,
            Some(true)
        );
        assert_eq!(
            VerdictRecord::from_error("c", "cope-b", "x", WHO, "timed out", 0.0).called_toxic,
            None
        );
    }

    #[test]
    fn reuse_requires_same_content_model_and_policy() {
        let r = VerdictRecord::from_verdict("a", "cope-b", "post text", &verdict(true, 0.9), 0.0);
        assert!(r.reusable("post text", WHO));
        assert!(!r.reusable("post text, edited", WHO));
        assert!(!r.reusable(
            "post text",
            Identity {
                model_id: "m-2",
                policy_version: "p-1"
            }
        ));
        assert!(!r.reusable(
            "post text",
            Identity {
                model_id: "m-1",
                policy_version: "p-2"
            }
        ));
        assert!(
            !VerdictRecord::from_error("a", "cope-b", "post text", WHO, "boom", 0.0)
                .reusable("post text", WHO)
        );
    }

    #[test]
    fn cache_and_identity_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        append(
            dir.path(),
            &VerdictRecord::from_error("a", "m", "x", WHO, "timeout", 0.0),
        )
        .unwrap();
        append(
            dir.path(),
            &VerdictRecord::from_verdict("a", "m", "x", &verdict(true, 0.9), 0.01),
        )
        .unwrap();
        let got = load(dir.path(), "m").unwrap();
        assert_eq!(got["a"].called_toxic, Some(true), "the last record wins");
        assert!(load(dir.path(), "other").unwrap().is_empty());
        assert_eq!(load_identity(dir.path(), "m").unwrap(), None);
        save_identity(dir.path(), "m", WHO).unwrap();
        assert_eq!(
            load_identity(dir.path(), "m").unwrap().unwrap().model_id,
            "m-1"
        );
    }

    /// Codex review, round 2: report and label must not score stale or
    /// foreign verdicts.
    #[test]
    fn only_current_verdicts_survive() {
        let pool = vec![
            row("kept", "same"),
            row("edited", "new text"),
            row("err", "e"),
        ];
        let mut recs = HashMap::new();
        recs.insert(
            "kept".into(),
            VerdictRecord::from_verdict("kept", "m", "same", &verdict(true, 0.9), 0.0),
        );
        recs.insert(
            "edited".into(),
            VerdictRecord::from_verdict("edited", "m", "old text", &verdict(true, 0.9), 0.0),
        );
        recs.insert(
            "gone".into(),
            VerdictRecord::from_verdict("gone", "m", "x", &verdict(true, 0.9), 0.0),
        );
        recs.insert(
            "err".into(),
            VerdictRecord::from_error("err", "m", "e", WHO, "timed out", 0.0),
        );
        let who = IdentityRecord {
            model_id: "m-1".into(),
            policy_version: "p-1".into(),
        };
        let cur = current(recs.clone(), &pool, Some(&who));
        let mut keys: Vec<&str> = cur.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["err", "kept"],
            "errors still count toward coverage"
        );
        let newer = IdentityRecord {
            model_id: "m-1".into(),
            policy_version: "p-2".into(),
        };
        assert!(
            current(recs.clone(), &pool, Some(&newer)).is_empty(),
            "redeployed: nothing current"
        );
        assert!(
            current(recs, &pool, None).is_empty(),
            "no identity recorded: nothing current"
        );
    }
}
