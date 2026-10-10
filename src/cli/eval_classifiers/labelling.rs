//! Choosing what a human labels next, in two streams kept strictly apart:
//!
//! - **Holdout** — a fixed, hash-chosen third of the pool, labelled whatever
//!   the models say. Only these labels feed the headline rates.
//! - **Diagnostic** — disagreements, plus a stable ~10 % of agreed posts.
//!   Useful for understanding models; never mixed into the headline numbers.
//!
//! The stream a label belongs to is DERIVED from the post id at merge time,
//! never read from the (human-edited) file (Codex review, round 2).

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

use super::cache::VerdictRecord;
use super::data::{load_current_labels, Answer, FixtureRow, Label, Source, Truth};
use super::{content_hash, fnv1a};

/// Salted so holdout membership is independent of the diagnostic sample.
pub fn in_holdout(id: &str) -> bool {
    fnv1a(format!("holdout:{id}").as_bytes()).is_multiple_of(3)
}

pub fn sampled(id: &str) -> bool {
    fnv1a(id.as_bytes()).is_multiple_of(10)
}

pub fn source_for(id: &str) -> Source {
    if in_holdout(id) {
        Source::Holdout
    } else {
        Source::Diagnostic
    }
}

pub fn pick_for_labelling(
    pool: &[FixtureRow],
    per_model: &[HashMap<String, VerdictRecord>],
    labels: &HashMap<String, Answer>,
) -> Vec<(FixtureRow, Source)> {
    let mut out = Vec::new();
    for row in pool.iter().filter(|r| !labels.contains_key(&r.id)) {
        if in_holdout(&row.id) {
            out.push((row.clone(), Source::Holdout));
            continue;
        }
        let calls: Vec<bool> = per_model
            .iter()
            .filter_map(|m| m.get(&row.id).and_then(|r| r.called_toxic))
            .collect();
        if calls.len() < 2 {
            continue;
        }
        let disagree = calls.iter().any(|c| *c != calls[0]);
        if disagree || sampled(&row.id) {
            out.push((row.clone(), Source::Diagnostic));
        }
    }
    out
}

#[derive(Serialize)]
struct ToLabel<'a> {
    id: &'a str,
    content: &'a str,
    source: Source,
    label: &'a str,
    note: &'a str,
}

/// Refuse to overwrite `path` while it holds a typed answer that has not been
/// merged into labels.jsonl yet: overwriting would silently lose the human's
/// work. Answers already in `merged` are safe to drop — they are kept in
/// labels.jsonl — so the normal label → answer → merge → label cycle works.
fn ensure_no_unmerged_answers(path: &Path, merged: &HashMap<String, Answer>) -> Result<()> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        // A line we cannot read might hold an answer: fail closed, never
        // overwrite what we could not check.
        let row: serde_json::Value = serde_json::from_str(l).with_context(|| {
            format!(
                "{} line {} is not valid JSON — fix it or run `charcoal eval-classifiers label --merge` first",
                path.display(),
                i + 1
            )
        })?;
        let typed = row["label"].as_str().unwrap_or("").trim();
        let answered = !typed.is_empty();
        let merged_truth = row["id"].as_str().and_then(|id| merged.get(id));
        if answered && merged_truth.is_none() {
            bail!(
                "{} has answers you have not saved yet (line {}). Run `charcoal eval-classifiers label --merge` first, then run `label` again.",
                path.display(),
                i + 1
            );
        }
        // An answer that DIFFERS from the saved label is an edit, and `merge`
        // skips ids already saved, so it would be lost without a word.
        if let Some(a) = merged_truth {
            let saved = match a.truth {
                Truth::Toxic => "toxic",
                Truth::Clean => "clean",
            };
            if answered && !typed.eq_ignore_ascii_case(saved) {
                bail!(
                    "{} line {}: that post is already in labels.jsonl as {saved}; to change it, edit labels.jsonl directly.",
                    path.display(),
                    i + 1
                );
            }
        }
    }
    Ok(())
}

/// Blind on purpose: no model votes, so they cannot anchor the answer. The
/// `source` is shown for the labeller's information only. `merged` is the
/// current labels.jsonl: the file is refused if it holds answers not in it.
pub fn write_to_label(
    path: &Path,
    rows: &[(FixtureRow, Source)],
    merged: &HashMap<String, Answer>,
) -> Result<()> {
    ensure_no_unmerged_answers(path, merged)?;
    let mut f =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    for (r, source) in rows {
        let line = ToLabel {
            id: &r.id,
            content: &r.content,
            source: *source,
            label: "",
            note: "",
        };
        writeln!(f, "{}", serde_json::to_string(&line)?)?;
    }
    Ok(())
}

pub fn merge_labels(to_label: &Path, labels_path: &Path, pool: &[FixtureRow]) -> Result<usize> {
    // Only labels that still describe the pool's text count as "already
    // labelled": an answer for an edited post must be merged, not skipped
    // (Codex fix 2). The new row is appended and, being newest, wins.
    let existing = load_current_labels(labels_path, pool)?.answers;
    let pool_text: HashMap<&str, &str> = pool
        .iter()
        .map(|r| (r.id.as_str(), r.content.as_str()))
        .collect();
    let text = std::fs::read_to_string(to_label)
        .with_context(|| format!("reading {}", to_label.display()))?;
    // Validate-then-append: every row is checked into `pending` first, so a
    // bad row N cannot leave rows 1..N-1 already written to labels.jsonl.
    let mut pending: Vec<Label> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = || format!("{} line {}", to_label.display(), i + 1);
        let row: serde_json::Value = serde_json::from_str(l).with_context(at)?;
        let id = row["id"]
            .as_str()
            .with_context(|| format!("{}: row without id", at()))?;
        let answer = row["label"].as_str().unwrap_or("").trim();
        if answer.is_empty() || existing.contains_key(id) {
            continue;
        }
        let Some(pool_content) = pool_text.get(id) else {
            bail!("{}: id {id:?} is not in the pool", at());
        };
        if let Some(content) = row["content"].as_str() {
            if content != *pool_content {
                bail!(
                    "{}: content for {id:?} differs from the pool — re-run `label`",
                    at()
                );
            }
        }
        let truth = match answer {
            "toxic" => Truth::Toxic,
            "clean" => Truth::Clean,
            other => bail!("{}: label {other:?} must be toxic or clean", at()),
        };
        let note = row["note"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        // Derived, not read: a hand-edited "source" cannot promote a
        // disagreement-picked post into the headline rates.
        let label = Label {
            id: id.to_string(),
            label: truth,
            source: source_for(id),
            note,
            // The pool's text — checked equal to the shown text above when
            // the row carries it — is what this answer describes.
            content_hash: content_hash(pool_content),
        };
        // Even two agreeing answers are an error: the human should fix the
        // file rather than have the last one silently win on reload.
        if !seen.insert(id.to_string()) {
            bail!("{}: id {id:?} is answered more than once", at());
        }
        pending.push(label);
    }
    if pending.is_empty() {
        return Ok(0);
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(labels_path)
        .with_context(|| format!("opening {}", labels_path.display()))?;
    for label in &pending {
        writeln!(out, "{}", serde_json::to_string(label)?)?;
    }
    Ok(pending.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::eval_classifiers::cache::VerdictRecord;
    use crate::cli::eval_classifiers::data::load_labels;

    fn row(id: &str) -> FixtureRow {
        serde_json::from_value(serde_json::json!({"id": id, "content": format!("post {id}")}))
            .unwrap()
    }
    fn rec(id: &str, call: Option<bool>) -> VerdictRecord {
        VerdictRecord {
            id: id.into(),
            model: "m".into(),
            content_hash: String::new(),
            model_id: "m".into(),
            policy_version: "p".into(),
            called_toxic: call,
            error: call.is_none().then(|| "e".into()),
            latency_ms: 1,
            input_chars: 1,
            est_usd: 0.0,
        }
    }
    fn by_id(recs: Vec<VerdictRecord>) -> HashMap<String, VerdictRecord> {
        recs.into_iter().map(|r| (r.id.clone(), r)).collect()
    }
    fn holdout_id() -> String {
        (0..100)
            .map(|i| format!("h{i}"))
            .find(|id| in_holdout(id))
            .unwrap()
    }
    fn other_id() -> String {
        (0..100)
            .map(|i| format!("o{i}"))
            .find(|id| !in_holdout(id))
            .unwrap()
    }

    #[test]
    fn holdout_and_sample_fractions_are_stable() {
        let holdout = (0..3000)
            .filter(|i| in_holdout(&format!("post-{i}")))
            .count();
        assert!((900..=1100).contains(&holdout), "{holdout} of 3000 ≈ 1/3");
        let sample = (0..2000).filter(|i| sampled(&format!("post-{i}"))).count();
        assert!((140..=260).contains(&sample), "{sample} of 2000 ≈ 1/10");
    }

    #[test]
    fn holdout_posts_are_picked_whatever_the_models_say() {
        let picked = pick_for_labelling(&[row(&holdout_id())], &[], &HashMap::new());
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].1, Source::Holdout);
    }

    #[test]
    fn diagnostics_are_disagreements_plus_the_agreed_sample() {
        let ids: Vec<String> = (0..400)
            .map(|i| format!("d{i}"))
            .filter(|id| !in_holdout(id))
            .collect();
        let dis = ids[0].clone();
        let labelled = ids[1].clone();
        let agree = ids.iter().skip(2).find(|id| !sampled(id)).unwrap().clone();
        let agree_sampled = ids.iter().skip(2).find(|id| sampled(id)).unwrap().clone();
        let pool: Vec<FixtureRow> = [&dis, &agree, &agree_sampled, &labelled]
            .iter()
            .map(|id| row(id))
            .collect();
        let m1 = by_id(vec![
            rec(&dis, Some(true)),
            rec(&agree, Some(true)),
            rec(&agree_sampled, Some(true)),
            rec(&labelled, Some(true)),
        ]);
        let m2 = by_id(vec![
            rec(&dis, Some(false)),
            rec(&agree, Some(true)),
            rec(&agree_sampled, Some(true)),
            rec(&labelled, Some(false)),
        ]);
        let labels: HashMap<String, Answer> = [(
            labelled.clone(),
            Answer {
                truth: Truth::Toxic,
                source: Source::Diagnostic,
            },
        )]
        .into();
        let picked: Vec<(String, Source)> = pick_for_labelling(&pool, &[m1, m2], &labels)
            .into_iter()
            .map(|(r, s)| (r.id, s))
            .collect();
        assert!(picked.contains(&(dis.clone(), Source::Diagnostic)));
        assert!(picked.contains(&(agree_sampled.clone(), Source::Diagnostic)));
        assert!(!picked.iter().any(|(id, _)| *id == agree));
        assert!(!picked.iter().any(|(id, _)| *id == labelled));
    }

    /// Codex review, round 2: the source is derived, never trusted — a
    /// diagnostic post relabelled "holdout" in the file stays diagnostic.
    #[test]
    fn merge_derives_the_source_and_validates_against_the_pool() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let (h, o) = (holdout_id(), other_id());
        let pool = vec![row(&h), row(&o)];
        write_to_label(
            &to_label,
            &[(row(&h), Source::Holdout), (row(&o), Source::Diagnostic)],
            &HashMap::new(),
        )
        .unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(
            !text.contains("called_toxic") && !text.contains("model"),
            "no votes shown: {text}"
        );
        // Answer both — and "promote" the diagnostic one to holdout by hand.
        let edited = text
            .replace(r#""label":"""#, r#""label":"toxic""#)
            .replace(r#""source":"diagnostic""#, r#""source":"holdout""#);
        std::fs::write(&to_label, edited).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap(), 2);
        let got = load_labels(&labels).unwrap();
        assert_eq!(got[&h].source, Source::Holdout);
        assert_eq!(
            got[&o].source,
            Source::Diagnostic,
            "the hand edit is ignored"
        );
        assert_eq!(
            merge_labels(&to_label, &labels, &pool).unwrap(),
            0,
            "already merged ids are skipped"
        );

        std::fs::write(
            &to_label,
            r#"{"id":"nope","content":"x","source":"holdout","label":"toxic","note":""}"#,
        )
        .unwrap();
        assert!(
            merge_labels(&to_label, &labels, &pool).is_err(),
            "unknown id"
        );
        std::fs::write(&to_label, format!(r#"{{"id":"{h}x","content":"post {h}x","source":"holdout","label":"maybe","note":""}}"#)).unwrap();
        assert!(
            merge_labels(&to_label, &labels, &[row(&format!("{h}x"))]).is_err(),
            "bad label"
        );
        let pool2 = vec![row(&format!("{o}y"))];
        std::fs::write(&to_label, format!(r#"{{"id":"{o}y","content":"different text","source":"diagnostic","label":"clean","note":""}}"#)).unwrap();
        assert!(
            merge_labels(&to_label, &labels, &pool2).is_err(),
            "content differs from the pool"
        );
    }

    /// Codex fix 2: merge records WHICH text the answer was given for.
    #[test]
    fn merge_records_the_hash_of_the_pool_text() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        std::fs::write(
            &to_label,
            format!(r#"{{"id":"{h}","content":"post {h}","label":"toxic","note":""}}"#),
        )
        .unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &[row(&h)]).unwrap(), 1);
        let saved: serde_json::Value =
            serde_json::from_str(std::fs::read_to_string(&labels).unwrap().trim()).unwrap();
        assert_eq!(saved["content_hash"], content_hash(&format!("post {h}")));
    }

    /// Codex fix 2: once a labelled post's text changes, its old label is
    /// not truth any more: the picker offers the post again, and merging the
    /// new answer is not skipped as "already labelled".
    #[test]
    fn an_edited_post_is_offered_again_and_its_new_answer_merged() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        std::fs::write(
            &labels,
            format!(
                "{{\"id\":\"{h}\",\"label\":\"toxic\",\"source\":\"holdout\",\"content_hash\":\"{}\"}}\n",
                content_hash("the old text")
            ),
        )
        .unwrap();
        let pool = vec![row(&h)]; // text is now "post {h}"
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 1);
        let picked = pick_for_labelling(&pool, &[], &cur.answers);
        assert_eq!(picked.len(), 1, "offered again");
        write_to_label(&to_label, &picked, &cur.answers).unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, answered).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap(), 1);
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 0);
        assert_eq!(cur.answers[&h].truth, Truth::Clean);
    }

    /// Review fix: a bad row must not leave the earlier good rows appended.
    #[test]
    fn a_failed_merge_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        let pool = vec![row(&h)];
        let good = format!(r#"{{"id":"{h}","content":"post {h}","label":"toxic","note":""}}"#);
        let bad = r#"{"id":"nope","content":"x","label":"toxic","note":""}"#;
        std::fs::write(&to_label, format!("{good}\n{bad}\n")).unwrap();
        assert!(merge_labels(&to_label, &labels, &pool).is_err());
        assert!(
            !labels.exists() || std::fs::read_to_string(&labels).unwrap().is_empty(),
            "no partial merge"
        );
    }

    /// Final review fix 2: re-running `label` before `--merge` must not
    /// throw away answers the human already typed.
    #[test]
    fn label_refuses_to_overwrite_unmerged_answers() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let h = holdout_id();
        let typed = format!(
            "{{\"id\":\"{h}\",\"content\":\"post {h}\",\"source\":\"holdout\",\"label\":\"toxic\",\"note\":\"\"}}\n"
        );
        std::fs::write(&to_label, &typed).unwrap();
        let err = write_to_label(
            &to_label,
            &[(row(&other_id()), Source::Diagnostic)],
            &HashMap::new(),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("label --merge"),
            "tells the user what to run: {err:#}"
        );
        assert_eq!(
            std::fs::read_to_string(&to_label).unwrap(),
            typed,
            "the typed answer is untouched"
        );
    }

    /// Final review fix 2: a file of blank answers holds nothing to lose.
    #[test]
    fn label_may_overwrite_a_file_with_only_blank_answers() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let (h, o) = (holdout_id(), other_id());
        write_to_label(&to_label, &[(row(&h), Source::Holdout)], &HashMap::new()).unwrap();
        // A whitespace-only answer is still blank (merge treats it so too).
        let spaced = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"  ""#);
        std::fs::write(&to_label, spaced).unwrap();
        write_to_label(&to_label, &[(row(&o), Source::Diagnostic)], &HashMap::new()).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(
            text.contains(&o) && !text.contains(&format!("\"{h}\"")),
            "{text}"
        );
    }

    /// Final review fix 2: after `--merge` the answers live in labels.jsonl,
    /// so the next `label` may replace the file (no deadlock after a merge).
    #[test]
    fn label_may_overwrite_answers_that_are_already_merged() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let (h, o) = (holdout_id(), other_id());
        let pool = vec![row(&h), row(&o)];
        write_to_label(&to_label, &[(row(&h), Source::Holdout)], &HashMap::new()).unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, answered).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap(), 1);
        let merged = load_labels(&labels).unwrap();
        write_to_label(&to_label, &[(row(&o), Source::Diagnostic)], &merged).unwrap();
        assert!(std::fs::read_to_string(&to_label).unwrap().contains(&o));
    }

    /// Final round 2, item 3: an edited answer for an already-merged id would
    /// be silently overwritten (and skipped by merge), so refuse it.
    #[test]
    fn label_refuses_an_edited_answer_for_an_already_merged_id() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let h = holdout_id();
        let merged = HashMap::from([(
            h.clone(),
            Answer {
                truth: Truth::Toxic,
                source: Source::Holdout,
            },
        )]);
        let typed = format!(
            "{{\"id\":\"{h}\",\"content\":\"post {h}\",\"source\":\"holdout\",\"label\":\"clean\",\"note\":\"\"}}\n"
        );
        std::fs::write(&to_label, &typed).unwrap();
        let err = write_to_label(
            &to_label,
            &[(row(&other_id()), Source::Diagnostic)],
            &merged,
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already in labels.jsonl as toxic") && msg.contains("edit labels.jsonl"),
            "{msg}"
        );
        assert_eq!(std::fs::read_to_string(&to_label).unwrap(), typed);
    }

    /// Review fix: the same id answered twice is a file error, not two labels.
    #[test]
    fn a_duplicate_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        let pool = vec![row(&h)];
        let line = format!(r#"{{"id":"{h}","content":"post {h}","label":"toxic","note":""}}"#);
        std::fs::write(&to_label, format!("{line}\n{line}\n")).unwrap();
        let err = merge_labels(&to_label, &labels, &pool).unwrap_err();
        assert!(format!("{err:#}").contains(&h), "names the id: {err:#}");
        assert!(!labels.exists() || std::fs::read_to_string(&labels).unwrap().is_empty());
    }
}
