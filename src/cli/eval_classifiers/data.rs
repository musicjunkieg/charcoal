//! Loading the eval's posts (the pool) and the human answers (labels).

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

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
pub fn load_labels(path: &Path) -> Result<HashMap<String, Answer>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let mut out = HashMap::new();
    for (i, l) in text.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let label: Label = serde_json::from_str(l).with_context(|| {
            format!(
                "{} line {} (label must be toxic|clean, source seed|holdout|diagnostic)",
                path.display(),
                i + 1
            )
        })?;
        out.insert(
            label.id,
            Answer {
                truth: label.label,
                source: label.source,
            },
        );
    }
    Ok(out)
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

    /// The committed pool must still load: its ids are unique.
    #[test]
    fn the_committed_pool_has_unique_ids() {
        let pool = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals/classifiers/pool.jsonl");
        assert_eq!(load_pool(&pool).unwrap().len(), 110);
    }

    #[test]
    fn labels_carry_truth_and_source() {
        let f = file(&[
            r#"{"id":"a","label":"toxic","source":"holdout"}"#,
            r#"{"id":"b","label":"clean","source":"seed","note":"ok"}"#,
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
            load_labels(file(&[r#"{"id":"c","label":"toxic"}"#]).path()).is_err(),
            "source is required"
        );
    }

    #[test]
    fn a_missing_labels_file_is_an_empty_set() {
        assert!(load_labels(Path::new("/nonexistent/labels.jsonl"))
            .unwrap()
            .is_empty());
    }
}
