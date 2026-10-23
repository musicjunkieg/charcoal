//! Loading the eval's posts (the pool) and the human answers (labels).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::content_hash;
pub use crate::cli::classify_compare::FixtureRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Truth {
    Toxic,
    Clean,
}

/// Why a post was labelled. Only `Holdout` labels — chosen independently of
/// any model's answer — may feed the headline rates; the others were picked
/// BECAUSE of model behaviour (or by hand) and would bias them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Seed,
    Holdout,
    Diagnostic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    pub truth: Truth,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: String,
    pub label: Truth,
    pub source: Source,
    #[serde(default)]
    pub note: Option<String>,
    /// `content_hash` of the post text this answer was given for. Required:
    /// a label keyed only by id would survive an edit of the post and be
    /// scored against text the human never saw (Codex fix 2).
    pub content_hash: String,
    /// #407: the reason families the human ticked. Toxic labels only;
    /// absent on lines written before #407.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

pub fn load_pool(path: &Path) -> Result<Vec<FixtureRow>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut rows: Vec<FixtureRow> = Vec::new();
    // id -> the file line it first appeared on (1-based, blank lines counted,
    // so the numbers match what an editor shows).
    let mut first_seen: HashMap<String, usize> = HashMap::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        let line = i + 1;
        let row: FixtureRow =
            serde_json::from_str(l).with_context(|| format!("{} line {line}", path.display()))?;
        // The cache, text lookup and labels are all keyed by id: a duplicate
        // would be paid for twice and a label could bind to the wrong text.
        if let Some(first) = first_seen.insert(row.id.clone(), line) {
            bail!(
                "{}: post id {:?} appears twice (line {first} and line {line}) — every post needs its own id; remove or rename one",
                path.display(),
                row.id
            );
        }
        rows.push(row);
    }
    Ok(rows)
}

/// A labels file that does not exist yet is an empty set, not an error.
/// Every label, whatever text it was given for; the newest row per id wins.
/// Consumers that score or pick should use [`load_current_labels`].
pub fn load_labels(path: &Path) -> Result<HashMap<String, Answer>> {
    Ok(load_labels_with_hash(path)?
        .into_iter()
        .map(|(id, (answer, _))| (id, answer))
        .collect())
}

fn load_labels_with_hash(path: &Path) -> Result<HashMap<String, (Answer, String)>> {
    // Later rows replace earlier ones: relabelling an edited post appends.
    Ok(load_label_rows(path)?
        .into_iter()
        .map(|(id, answer, hash)| (id, (answer, hash)))
        .collect())
}

/// Every row of labels.jsonl, oldest first, as (id, answer, content_hash).
fn load_label_rows(path: &Path) -> Result<Vec<(String, Answer, String)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = Vec::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let label: Label = serde_json::from_str(l).with_context(|| {
            format!(
                "{} line {} (label must be toxic|clean, source seed|holdout|diagnostic, plus the content_hash of the text it describes)",
                path.display(),
                i + 1
            )
        })?;
        out.push((
            label.id,
            Answer {
                truth: label.label,
                source: label.source,
            },
            label.content_hash,
        ));
    }
    Ok(out)
}

/// Ids that were EVER labelled as `Source::Seed`, whatever text the label
/// describes. Seed provenance lives only in labels.jsonl (pool rows carry no
/// source), so this is where a relabel looks it up: a curated seed whose text
/// changed must stay a seed when relabelled, or a hand-picked post could land
/// in the hash-chosen holdout and bias the headline (Codex review #2, item 3).
/// Any row counts, not just the newest: once a seed, always a seed.
pub fn seed_ids(path: &Path) -> Result<std::collections::HashSet<String>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = std::collections::HashSet::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let label: Label = serde_json::from_str(l)
            .with_context(|| format!("{} line {}", path.display(), i + 1))?;
        if label.source == Source::Seed {
            out.insert(label.id);
        }
    }
    Ok(out)
}

/// The labels that are still truth for the pool as it is now.
#[derive(Debug)]
pub struct CurrentLabels {
    pub answers: HashMap<String, Answer>,
    /// Labels dropped because their post's text changed since labelling.
    /// Those posts count as unlabelled (the picker offers them again).
    pub stale: usize,
    /// Labels dropped because their post is no longer in pool.jsonl. The
    /// rows stay in labels.jsonl (the post may come back); they just are not
    /// truth while the post is gone.
    pub removed: usize,
}

/// Each post's ticked reasons, from the newest label row whose hash matches
/// the post's present text — the same rule [`load_current_labels`] uses, so
/// reasons never outlive the answer they belong to. Only posts with at least
/// one reason are returned. A hand-edited bad reason is an error naming the
/// line: silently skipping it would score a family on missing data.
pub fn load_current_reasons(
    path: &Path,
    pool: &[FixtureRow],
) -> Result<HashMap<String, Vec<String>>> {
    let present: HashMap<&str, String> = pool
        .iter()
        .map(|r| (r.id.as_str(), content_hash(&r.content)))
        .collect();
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = format!("{} line {}", path.display(), i + 1);
        let label: Label = serde_json::from_str(l).with_context(|| at.clone())?;
        if !label.reasons.is_empty() && label.label != Truth::Toxic {
            bail!("{at}: reasons are only for toxic labels");
        }
        super::reasons::check_reason_keys(&label.reasons).with_context(|| at.clone())?;
        if present.get(label.id.as_str()) == Some(&label.content_hash) {
            // Newest matching row wins, as for the answer itself.
            if label.reasons.is_empty() {
                out.remove(&label.id);
            } else {
                out.insert(label.id, label.reasons);
            }
        }
    }
    Ok(out)
}

/// Labels whose recorded `content_hash` matches the pool's present text for
/// that post (Codex fix 2). A label for edited text would score a verdict on
/// the new text against the old text's truth, so it is dropped and counted.
/// A label for an id no longer in the pool is dropped and counted too: it
/// has no verdict to match, yet left in it would still count in the class
/// and coverage totals, pulling every model toward the 90 % coverage gate
/// (Codex review #2, item 2).
pub fn load_current_labels(path: &Path, pool: &[FixtureRow]) -> Result<CurrentLabels> {
    let present: HashMap<&str, String> = pool
        .iter()
        .map(|r| (r.id.as_str(), content_hash(&r.content)))
        .collect();
    // Pick, per post, the NEWEST row whose hash matches the text it has now,
    // not the newest row overall: a post labelled on text A, relabelled on B
    // and then restored to A has a valid A label again (Codex review #4).
    // Rows are oldest first, so a later match simply replaces an earlier one.
    let mut answers = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut seeds = std::collections::HashSet::new();
    for (id, answer, hash) in load_label_rows(path)? {
        if answer.source == Source::Seed {
            seeds.insert(id.clone());
        }
        if present.get(id.as_str()) == Some(&hash) {
            answers.insert(id.clone(), answer);
        }
        seen.insert(id);
    }
    // Once a seed, always a seed (as in [`seed_ids`]): a restored older row
    // may say holdout, but a hand-picked post must stay out of the headline.
    for (id, answer) in answers.iter_mut() {
        if seeds.contains(id) {
            answer.source = Source::Seed;
        }
    }
    // Counted once per post, however many rows it has.
    let (mut stale, mut removed) = (0, 0);
    for id in &seen {
        if !present.contains_key(id.as_str()) {
            removed += 1;
        } else if !answers.contains_key(id) {
            stale += 1;
        }
    }
    Ok(CurrentLabels {
        answers,
        stale,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn file(lines: &[&str]) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f
    }

    #[test]
    fn loads_pool_rows_and_skips_blank_lines() {
        let f = file(&[
            r#"{"id":"a","content":"x"}"#,
            "",
            r#"{"id":"b","content":"y","label":"toxic"}"#,
        ]);
        let rows = load_pool(f.path()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].label, "toxic");
    }

    /// Final review fix 4: everything downstream is keyed by id, so a
    /// duplicated id would be paid for twice and labelled against the wrong
    /// text. The error names the id and both (file) line numbers.
    #[test]
    fn a_duplicated_pool_id_is_an_error_naming_both_lines() {
        let f = file(&[
            r#"{"id":"a","content":"x"}"#,
            "",
            r#"{"id":"b","content":"y"}"#,
            r#"{"id":"a","content":"z"}"#,
        ]);
        let err = format!("{:#}", load_pool(f.path()).unwrap_err());
        assert!(
            err.contains("\"a\"") && err.contains("line 1") && err.contains("line 4"),
            "{err}"
        );
    }

    /// The committed pool must still load: its ids are unique. `>=` because
    /// the README tells the owner to grow it (~600), which must not fail this.
    #[test]
    fn the_committed_pool_has_unique_ids() {
        let pool = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/classifiers/pool.jsonl");
        assert!(load_pool(&pool).unwrap().len() >= 110);
    }

    #[test]
    fn labels_carry_truth_and_source() {
        let f = file(&[
            r#"{"id":"a","label":"toxic","source":"holdout","content_hash":"0"}"#,
            r#"{"id":"b","label":"clean","source":"seed","note":"ok","content_hash":"0"}"#,
        ]);
        let labels = load_labels(f.path()).unwrap();
        assert_eq!(
            labels["a"],
            Answer {
                truth: Truth::Toxic,
                source: Source::Holdout
            }
        );
        assert_eq!(labels["b"].source, Source::Seed);
    }

    #[test]
    fn bad_label_rows_are_errors() {
        assert!(
            load_labels(file(&[r#"{"id":"c","label":"maybe","source":"holdout"}"#]).path())
                .is_err()
        );
        assert!(
            load_labels(file(&[r#"{"id":"c","label":"toxic","content_hash":"0"}"#]).path())
                .is_err(),
            "source is required"
        );
    }

    fn pool_of(rows: &[(&str, &str)]) -> Vec<FixtureRow> {
        rows.iter()
            .map(|(id, c)| serde_json::from_value(serde_json::json!({"id": id, "content": c})))
            .collect::<std::result::Result<_, _>>()
            .unwrap()
    }

    fn label_line(id: &str, label: &str, text: &str) -> String {
        format!(
            r#"{{"id":"{id}","label":"{label}","source":"holdout","content_hash":"{}"}}"#,
            content_hash(text)
        )
    }

    fn reasons_line(id: &str, label: &str, text: &str, reasons: &str) -> String {
        format!(
            r#"{{"id":"{id}","label":"{label}","source":"holdout","content_hash":"{}","reasons":{reasons}}}"#,
            content_hash(text)
        )
    }

    /// #407: older label lines have no reasons and still load.
    #[test]
    fn labels_without_reasons_still_load() {
        let f = file(&[&label_line("a", "toxic", "x")]);
        assert!(load_current_reasons(f.path(), &pool_of(&[("a", "x")]))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reasons_follow_the_label_for_the_present_text() {
        let f = file(&[
            &reasons_line("a", "toxic", "old", r#"["threat"]"#),
            &reasons_line("a", "toxic", "new", r#"["mockery","personal_attack"]"#),
            // A stale row AFTER the matching one: taking "the last row for
            // the id" instead of "the last row whose hash matches" would
            // pick this one up.
            &reasons_line("a", "toxic", "older-edit", r#"["threat"]"#),
            &reasons_line("gone", "toxic", "z", r#"["threat"]"#),
        ]);
        let got = load_current_reasons(f.path(), &pool_of(&[("a", "new")])).unwrap();
        assert_eq!(got["a"], vec!["mockery", "personal_attack"]);
        assert!(!got.contains_key("gone"));
    }

    /// A newer matching row with no reasons is the human clearing them.
    #[test]
    fn a_newer_matching_row_without_reasons_clears_older_ones() {
        let f = file(&[
            &reasons_line("a", "toxic", "x", r#"["threat"]"#),
            &label_line("a", "toxic", "x"),
        ]);
        let got = load_current_reasons(f.path(), &pool_of(&[("a", "x")])).unwrap();
        assert!(!got.contains_key("a"), "{got:?}");
    }

    #[test]
    fn a_bad_reason_in_labels_jsonl_is_an_error_naming_the_line() {
        let unknown = file(&[&reasons_line("a", "toxic", "x", r#"["rude"]"#)]);
        let err = format!(
            "{:#}",
            load_current_reasons(unknown.path(), &pool_of(&[("a", "x")])).unwrap_err()
        );
        assert!(err.contains("line 1") && err.contains("rude"), "{err}");
        let on_clean = file(&[&reasons_line("a", "clean", "x", r#"["threat"]"#)]);
        assert!(load_current_reasons(on_clean.path(), &pool_of(&[("a", "x")])).is_err());
    }

    /// Codex fix 2: a label describes the text it was given for. Without the
    /// hash it would silently survive an edit of the post.
    #[test]
    fn a_label_must_say_which_text_it_describes() {
        let err = load_labels(file(&[r#"{"id":"a","label":"toxic","source":"holdout"}"#]).path())
            .unwrap_err();
        assert!(format!("{err:#}").contains("content_hash"), "{err:#}");
    }

    /// Codex fix 2: a label for text the post no longer has is not truth —
    /// the post counts as unlabelled, and the number dropped is reported.
    #[test]
    fn a_label_for_changed_text_is_not_used() {
        let f = file(&[
            &label_line("kept", "toxic", "same"),
            &label_line("edited", "toxic", "old text"),
        ]);
        let pool = pool_of(&[("kept", "same"), ("edited", "new text")]);
        let cur = load_current_labels(f.path(), &pool).unwrap();
        assert_eq!(cur.stale, 1);
        assert!(cur.answers.contains_key("kept"));
        assert!(!cur.answers.contains_key("edited"));
    }

    /// Codex review #2, item 2: a label for a post no longer in the pool is
    /// not truth. Left in, it would sit in the class and coverage
    /// denominators with no verdict to match, dragging every model toward the
    /// 90 % coverage gate. It is ignored and counted, never deleted.
    #[test]
    fn a_label_for_a_post_no_longer_in_the_pool_is_not_used() {
        let f = file(&[
            &label_line("kept", "toxic", "same"),
            &label_line("gone", "toxic", "whatever"),
        ]);
        let pool = pool_of(&[("kept", "same")]);
        let cur = load_current_labels(f.path(), &pool).unwrap();
        assert_eq!(cur.removed, 1);
        assert_eq!(cur.stale, 0, "removed is not the same as edited");
        assert!(cur.answers.contains_key("kept"));
        assert!(!cur.answers.contains_key("gone"));
        assert_eq!(load_labels(f.path()).unwrap().len(), 2, "the row is kept");
        // And the denominators: a model that answered every pool post has
        // full coverage over one labelled post, not 1 of 2.
        let rec = crate::cli::eval_classifiers::cache::VerdictRecord {
            id: "kept".into(),
            model: "m".into(),
            content_hash: content_hash("same"),
            model_id: "m".into(),
            policy_version: "p".into(),
            called_toxic: Some(true),
            error: None,
            latency_ms: 1,
            input_chars: 4,
            est_usd: 0.0,
            reasons: None,
        };
        let recs: HashMap<String, _> = [("kept".to_string(), rec)].into();
        let r = &crate::cli::eval_classifiers::report::build(
            &[("m".to_string(), recs)],
            &cur.answers,
            &HashMap::new(),
        )[0];
        assert_eq!(r.holdout.labelled, 1);
        assert_eq!(r.holdout.coverage(), Some(1.0));
    }

    /// Codex fix 2: relabelling an edited post appends a newer row; the
    /// newest row is the one that counts.
    #[test]
    fn a_relabel_of_the_new_text_replaces_the_stale_label() {
        let f = file(&[
            &label_line("a", "toxic", "old text"),
            &label_line("a", "clean", "new text"),
        ]);
        let cur = load_current_labels(f.path(), &pool_of(&[("a", "new text")])).unwrap();
        assert_eq!(cur.stale, 0);
        assert_eq!(cur.answers["a"].truth, Truth::Clean);
    }

    /// Codex review #4: labelled on text A, relabelled on B, then the post
    /// goes back to A. The A label is valid again, so it must not be lost
    /// because a newer row describes other text.
    #[test]
    fn a_label_for_restored_text_counts_again() {
        let f = file(&[
            &label_line("a", "toxic", "text A"),
            &label_line("a", "clean", "text B"),
        ]);
        let cur = load_current_labels(f.path(), &pool_of(&[("a", "text A")])).unwrap();
        assert_eq!(cur.stale, 0, "a matching label exists");
        assert_eq!(cur.answers["a"].truth, Truth::Toxic);
    }

    /// Codex review #4: several rows for the same text — the newest wins
    /// (a hand edit appended after a merge, say).
    #[test]
    fn the_newest_label_for_the_present_text_wins() {
        let f = file(&[
            &label_line("a", "toxic", "text A"),
            &label_line("a", "clean", "text A"),
            &label_line("a", "toxic", "text B"),
        ]);
        let cur = load_current_labels(f.path(), &pool_of(&[("a", "text A")])).unwrap();
        assert_eq!(cur.answers["a"].truth, Truth::Clean);
    }

    /// Stale and removed are counted per post, however many rows it has.
    #[test]
    fn stale_and_removed_count_posts_not_rows() {
        let f = file(&[
            &label_line("edited", "toxic", "v1"),
            &label_line("edited", "clean", "v2"),
            &label_line("gone", "toxic", "x"),
            &label_line("gone", "clean", "y"),
        ]);
        let cur = load_current_labels(f.path(), &pool_of(&[("edited", "v3")])).unwrap();
        assert_eq!((cur.stale, cur.removed), (1, 1));
    }

    /// Review of Codex #4 fix, M1: once a seed, always a seed. A post
    /// labelled holdout on text A, hand-labelled seed on text B, then
    /// restored to A must not come back as holdout and enter the headline.
    #[test]
    fn a_restored_label_stays_a_seed_if_the_post_was_ever_one() {
        let f = file(&[
            &label_line("a", "toxic", "text A"),
            &label_line("a", "toxic", "text B").replace("holdout", "seed"),
        ]);
        let cur = load_current_labels(f.path(), &pool_of(&[("a", "text A")])).unwrap();
        assert_eq!(cur.answers["a"].source, Source::Seed);
    }

    /// Codex fix 2: the committed seed labels were backfilled with hashes of
    /// the committed pool text — every one must still match.
    #[test]
    fn the_committed_labels_match_the_committed_pool() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/classifiers");
        let pool = load_pool(&dir.join("pool.jsonl")).unwrap();
        let labels = dir.join("labels.jsonl");
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 0, "a committed label no longer matches its post");
        assert_eq!(cur.answers.len(), load_labels(&labels).unwrap().len());
    }

    #[test]
    fn a_missing_labels_file_is_an_empty_set() {
        assert!(load_labels(Path::new("/nonexistent/labels.jsonl"))
            .unwrap()
            .is_empty());
    }
}
