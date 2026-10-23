//! Choosing what a human labels next, in two streams kept strictly apart:
//!
//! - **Holdout** — a fixed, hash-chosen third of the pool, labelled whatever
//!   the models say. Only these labels feed the headline rates.
//! - **Diagnostic** — disagreements, plus a stable ~10 % of agreed posts.
//!   Useful for understanding models; never mixed into the headline numbers.
//!
//! The stream a label belongs to is DERIVED from the post id at merge time,
//! never read from the (human-edited) file (Codex review, round 2). The one
//! exception: a post labels.jsonl already holds as a curated **seed** stays a
//! seed when relabelled (Codex review #2, item 3).

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;

use super::cache::VerdictRecord;
use super::data::{load_current_labels, seed_ids, Answer, FixtureRow, Label, Source, Truth};
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
    seeds: &HashSet<String>,
) -> Vec<(FixtureRow, Source)> {
    let mut out = Vec::new();
    for row in pool.iter().filter(|r| !labels.contains_key(&r.id)) {
        // A seed (from labels.jsonl, see `seed_ids`) that is not in the
        // current labels has gone stale. Merge keeps it a seed, so show it as
        // one: labelling it "holdout" would also inflate the holdout count
        // `label` prints. And offer it always: seeds were hand-picked, so
        // neither the holdout hash nor the disagreement rule decides whether
        // it is worth labelling, and the stale note promises it comes back.
        if seeds.contains(&row.id) {
            out.push((row.clone(), Source::Seed));
            continue;
        }
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

/// The "k holdout, s seed, d diagnostic" part of the line `label` prints.
/// Each stream is counted on its own, so a stale seed never passes for a
/// holdout post (re-review concern 2).
pub fn pick_summary(picked: &[(FixtureRow, Source)]) -> String {
    let count = |want: Source| picked.iter().filter(|(_, s)| *s == want).count();
    format!(
        "{} holdout, {} seed, {} diagnostic",
        count(Source::Holdout),
        count(Source::Seed),
        count(Source::Diagnostic)
    )
}

#[derive(Serialize)]
struct ToLabel<'a> {
    id: &'a str,
    content: &'a str,
    source: Source,
    label: &'a str,
    note: &'a str,
}

/// The pool's present text, by id — what [`classify`] checks a row against.
fn pool_text(pool: &[FixtureRow]) -> HashMap<&str, &str> {
    pool.iter()
        .map(|r| (r.id.as_str(), r.content.as_str()))
        .collect()
}

/// The text a to-label.jsonl row says it was answered on: its `content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown<'a> {
    /// No `content` key at all. Only a hand-written row lacks it; it is
    /// judged by id alone, as merge always has: there is no text to compare.
    Absent,
    Text(&'a str),
    /// `content` is there but is not a string (e.g. `null`, after a tool
    /// mangled the file). It names no text, so the answer cannot be bound
    /// to any (review 3, M1) — unlike `Absent`, which is a deliberate choice.
    NotText,
}

impl<'a> Shown<'a> {
    fn of(row: &'a serde_json::Value) -> Self {
        match row.get("content") {
            None => Shown::Absent,
            Some(v) => v.as_str().map_or(Shown::NotText, Shown::Text),
        }
    }
}

/// What to do with one ANSWERED row of to-label.jsonl. Both the `label`
/// guard and `--merge` act on this and on nothing else (re-review 3, I1).
///
/// Why one function: three review rounds in a row found the two commands
/// asking the same questions in different orders, and each order left a
/// case where `label` said "run --merge first" (or "edit labels.jsonl")
/// while `--merge` quietly did nothing — so neither could move on. With one
/// answer per row, they cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowStatus {
    /// The post left the pool, or the text it was answered on is not the
    /// pool's text now. Saving it would bind the answer to text the human
    /// never saw, so it is never saved — and so there is nothing to protect.
    NotMergeable,
    /// labels.jsonl already holds this same answer for the current text.
    AlreadyMergedSame,
    /// labels.jsonl already holds a DIFFERENT answer for the current text:
    /// an edit. The only case either command refuses.
    ConflictsWithMerged { saved: Truth },
    /// A new answer for the current text: merge saves it, `label` keeps it.
    Mergeable,
}

/// Classify one answered row. `current` must be the labels that are truth
/// for the pool as it is now ([`load_current_labels`]).
///
/// Mergeability is checked FIRST: a row answered on other text is about
/// other text, so whatever labels.jsonl says about the current text is no
/// reason to keep or refuse it. That ordering is what closes the I1 lock.
///
/// "Already merged" means a label for the CURRENT text. A stale label (same
/// id, older text) is ignored here: the row is a fresh answer about the
/// text now, exactly what relabelling an edited post is for. Counting the
/// stale label as a conflict would block every relabel.
fn classify(
    id: &str,
    shown: Shown<'_>,
    answer: Truth,
    pool_text: &HashMap<&str, &str>,
    current: &HashMap<String, Answer>,
) -> RowStatus {
    let mergeable = match (pool_text.get(id), shown) {
        (None, _) => false,
        (Some(_), Shown::Absent) => true,
        (Some(now), Shown::Text(s)) => s == *now,
        (Some(_), Shown::NotText) => false,
    };
    if !mergeable {
        return RowStatus::NotMergeable;
    }
    match current.get(id) {
        None => RowStatus::Mergeable,
        Some(a) if a.truth == answer => RowStatus::AlreadyMergedSame,
        Some(a) => RowStatus::ConflictsWithMerged { saved: a.truth },
    }
}

/// The id and answer of a row someone answered, or `None` for a blank row.
/// Shared by the guard and merge so both read (and reject) rows alike: a
/// row without an id, or an answer other than toxic/clean, is an error.
fn read_answer<'a>(row: &'a serde_json::Value, at: &str) -> Result<Option<(&'a str, Truth)>> {
    let id = row["id"]
        .as_str()
        .with_context(|| format!("{at}: row without id"))?;
    let truth = match row["label"].as_str().unwrap_or("").trim() {
        "" => return Ok(None),
        "toxic" => Truth::Toxic,
        "clean" => Truth::Clean,
        other => bail!("{at}: label {other:?} must be toxic or clean"),
    };
    Ok(Some((id, truth)))
}

/// The refusal for [`RowStatus::ConflictsWithMerged`]. Editing labels.jsonl
/// is safe advice here: the answer was given on the text that label holds.
/// The message names both halves of the way out, because someone who has
/// ALREADY edited labels.jsonl lands here too while the old answer still sits
/// in to-label.jsonl — telling them only "edit labels.jsonl" would loop.
fn conflict_error(at: &str, saved: Truth) -> anyhow::Error {
    let saved = match saved {
        Truth::Toxic => "toxic",
        Truth::Clean => "clean",
    };
    anyhow::anyhow!(
        "{at}: that post is already in labels.jsonl as {saved}, but this row answers it differently. \
         To change the saved answer, edit labels.jsonl directly; then make this row's label in \
         to-label.jsonl blank (or the same as labels.jsonl) so label and --merge can continue."
    )
}

/// Refuse to overwrite `path` while it holds a typed answer that has not been
/// merged into labels.jsonl yet: overwriting would silently lose the human's
/// work. What counts as unsaved comes only from [`classify`]: answers already
/// merged are safe to drop, and so are answers that can never be merged
/// (protecting those would only block `label` forever).
fn ensure_no_unmerged_answers(
    path: &Path,
    merged: &HashMap<String, Answer>,
    pool: &[FixtureRow],
) -> Result<()> {
    let pool_text = pool_text(pool);
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = format!("{} line {}", path.display(), i + 1);
        // A line we cannot read might hold an answer: fail closed, never
        // overwrite what we could not check.
        let row: serde_json::Value = serde_json::from_str(l).with_context(|| {
            format!("{at} is not valid JSON — fix it or run `charcoal eval-classifiers label --merge` first")
        })?;
        // A row merge would reject stops `label` too, with merge's message.
        let Some((id, answer)) = read_answer(&row, &at)? else {
            continue;
        };
        match classify(id, Shown::of(&row), answer, &pool_text, merged) {
            RowStatus::NotMergeable | RowStatus::AlreadyMergedSame => {}
            RowStatus::ConflictsWithMerged { saved } => return Err(conflict_error(&at, saved)),
            RowStatus::Mergeable => bail!(
                "{} has answers you have not saved yet (line {}). Run `charcoal eval-classifiers label --merge` first, then run `label` again.",
                path.display(),
                i + 1
            ),
        }
    }
    Ok(())
}

/// Blind on purpose: no model votes, so they cannot anchor the answer. The
/// `source` is shown for the labeller's information only. `merged` is the
/// current labels.jsonl: the file is refused if it holds answers not in it
/// that could still be merged into the `pool`.
pub fn write_to_label(
    path: &Path,
    rows: &[(FixtureRow, Source)],
    merged: &HashMap<String, Answer>,
    pool: &[FixtureRow],
) -> Result<()> {
    ensure_no_unmerged_answers(path, merged, pool)?;
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

/// What one `label --merge` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergeOutcome {
    /// Answers appended to labels.jsonl.
    pub merged: usize,
    /// Answers left out as [`RowStatus::NotMergeable`]: the post left the
    /// pool, or its text is not the text it was answered on.
    pub skipped: usize,
}

/// The line `--merge` prints about skipped answers, or "" when none were.
/// It promises a re-offer only for holdout and seed posts: a diagnostic post
/// comes back only if the models' verdicts on its current text pick it again
/// (review 3, M2).
pub fn skipped_note(skipped: usize) -> String {
    if skipped == 0 {
        return String::new();
    }
    format!(
        "{skipped} answer{} not saved — post removed from the pool or its text changed since you labelled it; holdout and seed posts still in the pool are offered again by `label`\n",
        if skipped == 1 { "" } else { "s" }
    )
}

pub fn merge_labels(
    to_label: &Path,
    labels_path: &Path,
    pool: &[FixtureRow],
) -> Result<MergeOutcome> {
    // Only labels that still describe the pool's text count as "already
    // labelled": an answer for an edited post must be merged, not skipped
    // (Codex fix 2). The new row is appended and, being newest, wins. The
    // `label` guard is handed this same set, so [`classify`] sees the same.
    let existing = load_current_labels(labels_path, pool)?.answers;
    // Read from every row, stale ones included: the stale seed label is
    // exactly the one being replaced, and its provenance must carry over.
    let seeds = seed_ids(labels_path)?;
    let pool_text = pool_text(pool);
    let mut skipped = 0;
    let text = std::fs::read_to_string(to_label)
        .with_context(|| format!("reading {}", to_label.display()))?;
    // Validate-then-append: every row is checked into `pending` first, so a
    // bad row N cannot leave rows 1..N-1 already written to labels.jsonl.
    let mut pending: Vec<Label> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let at = format!("{} line {}", to_label.display(), i + 1);
        let row: serde_json::Value = serde_json::from_str(l).with_context(|| at.clone())?;
        // Every answered row is validated here, even one about to be
        // skipped: a typo is still worth reporting.
        let Some((id, truth)) = read_answer(&row, &at)? else {
            continue;
        };
        match classify(id, Shown::of(&row), truth, &pool_text, &existing) {
            // Skip and count, never stop: `label` may replace this file, so
            // stopping here would lock the two commands against each other
            // (re-review I1). Skipping also never saves the answer against
            // text it was not given for.
            RowStatus::NotMergeable => {
                skipped += 1;
                continue;
            }
            // Already saved; nothing to report.
            RowStatus::AlreadyMergedSame => continue,
            // Refused before anything is appended, with the message `label`
            // gives: dropping the edit silently would lose it.
            RowStatus::ConflictsWithMerged { saved } => return Err(conflict_error(&at, saved)),
            RowStatus::Mergeable => {}
        }
        let pool_content = pool_text[id];
        let note = row["note"]
            .as_str()
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        // Derived, not read: a hand-edited "source" cannot promote a
        // disagreement-picked post into the headline rates. The one thing
        // carried over is seed provenance, taken from labels.jsonl itself
        // (never from to-label.jsonl, which a human edits): a relabelled
        // seed stays out of the headline (Codex review #2, item 3).
        let source = if seeds.contains(id) {
            Source::Seed
        } else {
            source_for(id)
        };
        let label = Label {
            id: id.to_string(),
            label: truth,
            source,
            note,
            // The pool's text — checked equal to the shown text above when
            // the row carries it — is what this answer describes.
            content_hash: content_hash(pool_content),
        };
        // Even two agreeing answers are an error: the human should fix the
        // file rather than have the last one silently win on reload.
        if !seen.insert(id.to_string()) {
            bail!("{at}: id {id:?} is answered more than once");
        }
        pending.push(label);
    }
    let outcome = MergeOutcome {
        merged: pending.len(),
        skipped,
    };
    if pending.is_empty() {
        return Ok(outcome);
    }
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(labels_path)
        .with_context(|| format!("opening {}", labels_path.display()))?;
    for label in &pending {
        writeln!(out, "{}", serde_json::to_string(label)?)?;
    }
    Ok(outcome)
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
            reasons: None,
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
        let picked =
            pick_for_labelling(&[row(&holdout_id())], &[], &HashMap::new(), &HashSet::new());
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
        let picked: Vec<(String, Source)> =
            pick_for_labelling(&pool, &[m1, m2], &labels, &HashSet::new())
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
            &pool,
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
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap().merged, 2);
        let got = load_labels(&labels).unwrap();
        assert_eq!(got[&h].source, Source::Holdout);
        assert_eq!(
            got[&o].source,
            Source::Diagnostic,
            "the hand edit is ignored"
        );
        assert_eq!(
            merge_labels(&to_label, &labels, &pool).unwrap().merged,
            0,
            "already merged ids are skipped"
        );

        // Re-review I1: an id not in the pool, or text that differs from the
        // pool, is skipped and counted — never saved, never a hard stop.
        let before = std::fs::read_to_string(&labels).unwrap();
        std::fs::write(
            &to_label,
            r#"{"id":"nope","content":"x","source":"holdout","label":"toxic","note":""}"#,
        )
        .unwrap();
        assert_eq!(
            merge_labels(&to_label, &labels, &pool).unwrap(),
            MergeOutcome {
                merged: 0,
                skipped: 1
            },
            "unknown id"
        );
        std::fs::write(&to_label, format!(r#"{{"id":"{h}x","content":"post {h}x","source":"holdout","label":"maybe","note":""}}"#)).unwrap();
        assert!(
            merge_labels(&to_label, &labels, &[row(&format!("{h}x"))]).is_err(),
            "bad label"
        );
        // Even a skipped row's answer is validated.
        std::fs::write(
            &to_label,
            r#"{"id":"nope","content":"x","source":"holdout","label":"maybe","note":""}"#,
        )
        .unwrap();
        assert!(merge_labels(&to_label, &labels, &pool).is_err());
        let pool2 = vec![row(&format!("{o}y"))];
        std::fs::write(&to_label, format!(r#"{{"id":"{o}y","content":"different text","source":"diagnostic","label":"clean","note":""}}"#)).unwrap();
        assert_eq!(
            merge_labels(&to_label, &labels, &pool2).unwrap().skipped,
            1,
            "content differs from the pool"
        );
        assert_eq!(std::fs::read_to_string(&labels).unwrap(), before);
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
        assert_eq!(
            merge_labels(&to_label, &labels, &[row(&h)]).unwrap().merged,
            1
        );
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
        let picked = pick_for_labelling(&pool, &[], &cur.answers, &HashSet::new());
        assert_eq!(picked.len(), 1, "offered again");
        write_to_label(&to_label, &picked, &cur.answers, &pool).unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, answered).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap().merged, 1);
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 0);
        assert_eq!(cur.answers[&h].truth, Truth::Clean);
    }

    /// Codex review #2, item 3: a curated SEED post whose text changed is
    /// relabelled through label/merge — and must stay a seed. Re-deriving its
    /// source from the id hash could drop a hand-picked post into the holdout
    /// and bias the headline rates. Uses a holdout-hashed id on purpose: that
    /// is the case where the bug would show.
    #[test]
    fn relabelling_a_stale_seed_keeps_it_a_seed() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        std::fs::write(
            &labels,
            format!(
                "{{\"id\":\"{h}\",\"label\":\"toxic\",\"source\":\"seed\",\"content_hash\":\"{}\"}}\n",
                content_hash("the old text")
            ),
        )
        .unwrap();
        let pool = vec![row(&h)]; // text is now "post {h}"
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 1);
        let picked = pick_for_labelling(&pool, &[], &cur.answers, &seed_ids(&labels).unwrap());
        write_to_label(&to_label, &picked, &cur.answers, &pool).unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, answered).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap().merged, 1);
        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.answers[&h].truth, Truth::Clean, "the new answer won");
        assert_eq!(cur.answers[&h].source, Source::Seed, "still a seed");
        assert!(
            crate::cli::eval_classifiers::report::labels_of(&cur.answers, Source::Holdout)
                .is_empty(),
            "a seed never reaches the headline"
        );
    }

    /// Re-review concern 2 / M3: a stale seed written back into
    /// to-label.jsonl is shown as a seed, not as holdout or diagnostic, so the
    /// printed holdout count is honest. And it is offered whatever the models
    /// say: a seed is hand-picked, so the holdout hash and the disagreement
    /// rule do not apply to it, and the stale note promises it comes back.
    #[test]
    fn a_stale_seed_is_offered_and_shown_as_a_seed() {
        let (h, o) = (holdout_id(), other_id());
        let pool = vec![row(&h), row(&o)];
        let seeds: HashSet<String> = [h.clone(), o.clone()].into();
        // Neither is in the current labels (both seed labels went stale),
        // and there are no model votes at all.
        let picked = pick_for_labelling(&pool, &[], &HashMap::new(), &seeds);
        let got: Vec<(String, Source)> = picked.iter().map(|(r, s)| (r.id.clone(), *s)).collect();
        assert_eq!(got, vec![(h, Source::Seed), (o, Source::Seed)]);
        assert_eq!(pick_summary(&picked), "0 holdout, 2 seed, 0 diagnostic");

        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        write_to_label(&to_label, &picked, &HashMap::new(), &pool).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(
            !text.contains("holdout") && !text.contains("diagnostic"),
            "{text}"
        );
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
        // An unknown id alone is now a skip (re-review I1), so the bad row
        // here is a bad answer, which still stops the whole merge.
        let bad = r#"{"id":"nope","content":"x","label":"maybe","note":""}"#;
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
            // h is in the pool with the text shown, so the answer can still
            // be merged: it must be protected (re-review I1, case c).
            &[row(&h), row(&other_id())],
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
        let pool = vec![row(&h), row(&o)];
        write_to_label(
            &to_label,
            &[(row(&h), Source::Holdout)],
            &HashMap::new(),
            &pool,
        )
        .unwrap();
        // A whitespace-only answer is still blank (merge treats it so too).
        let spaced = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"  ""#);
        std::fs::write(&to_label, spaced).unwrap();
        write_to_label(
            &to_label,
            &[(row(&o), Source::Diagnostic)],
            &HashMap::new(),
            &pool,
        )
        .unwrap();
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
        write_to_label(
            &to_label,
            &[(row(&h), Source::Holdout)],
            &HashMap::new(),
            &pool,
        )
        .unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, answered).unwrap();
        assert_eq!(merge_labels(&to_label, &labels, &pool).unwrap().merged, 1);
        let merged = load_labels(&labels).unwrap();
        write_to_label(&to_label, &[(row(&o), Source::Diagnostic)], &merged, &pool).unwrap();
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
            &[row(&h)],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already in labels.jsonl as toxic") && msg.contains("edit labels.jsonl"),
            "{msg}"
        );
        // Review 4: someone who already edited labels.jsonl gets this same
        // refusal, so it must also name the step that clears it — the
        // differing answer still sitting in to-label.jsonl.
        assert!(
            msg.contains("to-label.jsonl") && msg.contains("blank"),
            "the refusal must say how to clear the to-label row: {msg}"
        );
        assert_eq!(std::fs::read_to_string(&to_label).unwrap(), typed);
    }

    /// Answer post h in to-label.jsonl and merge it, as a user would.
    fn answered_and_merged(to_label: &Path, labels: &Path, pool: &[FixtureRow], h: &str) {
        write_to_label(
            to_label,
            &[(row(h), Source::Holdout)],
            &HashMap::new(),
            pool,
        )
        .unwrap();
        let answered = std::fs::read_to_string(to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"toxic""#);
        std::fs::write(to_label, answered).unwrap();
        assert_eq!(merge_labels(to_label, labels, pool).unwrap().merged, 1);
    }

    /// Re-review I1 (a): once an answered, merged post leaves the pool, its
    /// row in to-label.jsonl can never be merged, so it is not an unsaved
    /// answer to protect. Before the fix `label` said "run --merge first"
    /// while `--merge` said "not in the pool": neither could run.
    #[test]
    fn a_post_removed_from_the_pool_does_not_lock_label_or_merge() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let (h, o) = (holdout_id(), other_id());
        answered_and_merged(&to_label, &labels, &[row(&h), row(&o)], &h);
        let before = std::fs::read_to_string(&labels).unwrap();
        let pool = vec![row(&o)]; // h removed from pool.jsonl

        assert_eq!(
            merge_labels(&to_label, &labels, &pool).unwrap(),
            MergeOutcome {
                merged: 0,
                skipped: 1
            },
            "merge runs, skipping h and counting it"
        );
        assert_eq!(std::fs::read_to_string(&labels).unwrap(), before);

        let cur = load_current_labels(&labels, &pool).unwrap();
        let picked = pick_for_labelling(&pool, &[], &cur.answers, &HashSet::new());
        write_to_label(&to_label, &picked, &cur.answers, &pool).unwrap();
        assert!(!std::fs::read_to_string(&to_label)
            .unwrap()
            .contains(&format!("\"{h}\"")));
    }

    /// Re-review I1 (b): the same with h's text changed. The answer was made
    /// on the old text, so it must never be saved against the new text; the
    /// post is offered again with its new text instead.
    #[test]
    fn a_post_whose_text_changed_does_not_lock_and_is_offered_again() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        answered_and_merged(&to_label, &labels, &[row(&h)], &h);
        let before = std::fs::read_to_string(&labels).unwrap();
        let pool: Vec<FixtureRow> =
            vec![
                serde_json::from_value(serde_json::json!({"id": h, "content": "new text"}))
                    .unwrap(),
            ];

        assert_eq!(
            merge_labels(&to_label, &labels, &pool).unwrap(),
            MergeOutcome {
                merged: 0,
                skipped: 1
            }
        );
        assert_eq!(
            std::fs::read_to_string(&labels).unwrap(),
            before,
            "no label saved against text it was not made on"
        );

        let cur = load_current_labels(&labels, &pool).unwrap();
        assert_eq!(cur.stale, 1);
        let picked = pick_for_labelling(&pool, &[], &cur.answers, &HashSet::new());
        write_to_label(&to_label, &picked, &cur.answers, &pool).unwrap();
        let text = std::fs::read_to_string(&to_label).unwrap();
        assert!(text.contains("new text"), "offered again: {text}");
    }

    /// Re-review 3, I1: merge X on text A, the text changes to B, X is
    /// answered differently on B without merging, then the text goes back to
    /// A. The answer was made on B, so it can never be saved; it is not an
    /// edit of the label saved for A. Before the fix `label` refused forever
    /// ("already in labels.jsonl as toxic; edit labels.jsonl") and `--merge`
    /// printed "merged 0" with no skip note.
    #[test]
    fn an_answer_on_text_since_reverted_does_not_lock_label_or_merge() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let h = holdout_id();
        let pool_a = vec![row(&h)]; // text A = "post {h}"
        answered_and_merged(&to_label, &labels, &pool_a, &h); // toxic on A

        // The text changes to B: the label for A goes stale, h is offered
        // again, and the human answers it differently on B — not merged.
        let pool_b: Vec<FixtureRow> =
            vec![
                serde_json::from_value(serde_json::json!({"id": h, "content": "text B"})).unwrap(),
            ];
        let cur = load_current_labels(&labels, &pool_b).unwrap();
        let picked = pick_for_labelling(&pool_b, &[], &cur.answers, &HashSet::new());
        write_to_label(&to_label, &picked, &cur.answers, &pool_b).unwrap();
        let answered = std::fs::read_to_string(&to_label)
            .unwrap()
            .replace(r#""label":"""#, r#""label":"clean""#);
        std::fs::write(&to_label, &answered).unwrap();
        let before = std::fs::read_to_string(&labels).unwrap();

        // The text goes back to A.
        let cur = load_current_labels(&labels, &pool_a).unwrap();
        assert_eq!(
            cur.answers[&h].truth,
            Truth::Toxic,
            "A's label is current again"
        );

        // Merge is honest: nothing saved, and the answer is counted as skipped.
        assert_eq!(
            merge_labels(&to_label, &labels, &pool_a).unwrap(),
            MergeOutcome {
                merged: 0,
                skipped: 1
            }
        );
        assert_eq!(
            std::fs::read_to_string(&labels).unwrap(),
            before,
            "an answer about B is never saved against A"
        );

        // And `label` may replace the file: there is nothing it could save.
        let picked = pick_for_labelling(&pool_a, &[], &cur.answers, &HashSet::new());
        write_to_label(&to_label, &picked, &cur.answers, &pool_a).unwrap();
    }

    /// Re-review 3, I1: every classification, from one table. The pool has
    /// "a" with text "A" and "c" with text "C"; labels.jsonl currently holds
    /// "a" as toxic (on "A"). "c" has no current label.
    #[test]
    fn each_answered_row_is_classified_once() {
        use RowStatus::*;
        let pool: HashMap<&str, &str> = [("a", "A"), ("c", "C")].into();
        let current: HashMap<String, Answer> = [(
            "a".to_string(),
            Answer {
                truth: Truth::Toxic,
                source: Source::Holdout,
            },
        )]
        .into();
        let cases: &[(&str, Shown, Truth, RowStatus)] = &[
            // Left the pool: nothing to save it against.
            ("gone", Shown::Text("A"), Truth::Toxic, NotMergeable),
            ("gone", Shown::Absent, Truth::Toxic, NotMergeable),
            // Answered on other text. Checked FIRST, so even an answer that
            // differs from a's saved label is not a conflict (the I1 case).
            ("a", Shown::Text("B"), Truth::Clean, NotMergeable),
            ("a", Shown::Text("B"), Truth::Toxic, NotMergeable),
            ("c", Shown::Text("B"), Truth::Clean, NotMergeable),
            // A `content` that is not a string (e.g. null) names no text,
            // so it is not mergeable (review 3, M1).
            ("c", Shown::NotText, Truth::Clean, NotMergeable),
            // Same text, same answer as saved.
            ("a", Shown::Text("A"), Truth::Toxic, AlreadyMergedSame),
            ("a", Shown::Absent, Truth::Toxic, AlreadyMergedSame),
            // Same text, a different answer: the one refusal.
            (
                "a",
                Shown::Text("A"),
                Truth::Clean,
                ConflictsWithMerged {
                    saved: Truth::Toxic,
                },
            ),
            (
                "a",
                Shown::Absent,
                Truth::Clean,
                ConflictsWithMerged {
                    saved: Truth::Toxic,
                },
            ),
            // Unlabelled (or only stale-labelled) post on its current text;
            // a row with no `content` is judged by id alone.
            ("c", Shown::Text("C"), Truth::Clean, Mergeable),
            ("c", Shown::Absent, Truth::Toxic, Mergeable),
        ];
        for (id, shown, answer, want) in cases {
            assert_eq!(
                classify(id, *shown, *answer, &pool, &current),
                *want,
                "{id} {shown:?} {answer:?}"
            );
        }
    }

    /// Re-review 3, I1: a different answer for the same text as a saved
    /// label is refused by `--merge` too — before anything is appended — with
    /// the same message `label` gives. Merge used to drop it silently.
    #[test]
    fn merge_refuses_an_edited_answer_for_the_same_text() {
        let dir = tempfile::tempdir().unwrap();
        let to_label = dir.path().join("to-label.jsonl");
        let labels = dir.path().join("labels.jsonl");
        let (h, o) = (holdout_id(), other_id());
        let pool = vec![row(&h), row(&o)];
        answered_and_merged(&to_label, &labels, &pool, &h); // h toxic
        let before = std::fs::read_to_string(&labels).unwrap();
        let good = format!(r#"{{"id":"{o}","content":"post {o}","label":"toxic","note":""}}"#);
        let edit = format!(r#"{{"id":"{h}","content":"post {h}","label":"clean","note":""}}"#);
        std::fs::write(&to_label, format!("{good}\n{edit}\n")).unwrap();
        let err = format!("{:#}", merge_labels(&to_label, &labels, &pool).unwrap_err());
        assert!(
            err.contains("already in labels.jsonl as toxic") && err.contains("line 2"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&labels).unwrap(),
            before,
            "nothing appended, not even the good row"
        );
    }

    /// Re-review I1: the note `--merge` prints says what happened, in plain
    /// words, and is silent when nothing was skipped.
    #[test]
    fn the_skipped_note_explains_itself() {
        assert_eq!(skipped_note(0), "");
        let one = skipped_note(1);
        assert!(one.starts_with("1 answer not saved"), "{one}");
        assert!(
            one.contains("removed from the pool or its text changed")
                && one.contains("offered again"),
            "{one}"
        );
        assert!(skipped_note(2).starts_with("2 answers not saved"));
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
