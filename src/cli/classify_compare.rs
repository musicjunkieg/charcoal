//! `charcoal classify-compare` — run the same JSONL input through two
//! classifiers and emit a side-by-side comparison report.
//!
//! Per spec migration Step 4: A/B is informational, not a hard gate. Use this
//! command during development to characterize where backends diverge.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};

#[derive(Debug, Clone, Deserialize)]
pub struct FixtureRow {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub category: String,
    pub content: String,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pair {
    pub id: String,
    pub label: String,
    pub a: ClassifierVerdict,
    pub b: ClassifierVerdict,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct Summary {
    pub total: usize,
    pub scored_for_accuracy: usize,
    pub agreements: usize,
    pub disagreements: usize,
    pub a_toxic_only: usize,
    pub b_toxic_only: usize,
    pub a_correct_on_toxic: usize,
    pub a_correct_on_clean: usize,
    pub b_correct_on_toxic: usize,
    pub b_correct_on_clean: usize,
    pub a_name: String,
    pub b_name: String,
}

/// Pure summary — UI-free so unit tests can assert on numbers.
///
/// Uses raw `toxic_token` from each verdict — the same binary signal the
/// shadow-agreement gate compares (`crate::cli::classify_gate::evaluate`). For
/// A/B characterization at default thresholds (the typical use case for this
/// command), the raw token is what we want to compare.
pub fn summarize(pairs: &[Pair], a_name: &str, b_name: &str) -> Summary {
    let mut s = Summary {
        a_name: a_name.into(),
        b_name: b_name.into(),
        ..Default::default()
    };
    for p in pairs {
        s.total += 1;
        if p.a.toxic_token == p.b.toxic_token {
            s.agreements += 1;
        } else {
            s.disagreements += 1;
        }
        if p.a.toxic_token && !p.b.toxic_token {
            s.a_toxic_only += 1;
        }
        if !p.a.toxic_token && p.b.toxic_token {
            s.b_toxic_only += 1;
        }
        let want = match p.label.as_str() {
            "toxic" => Some(true),
            "clean" => Some(false),
            _ => None,
        };
        if let Some(want) = want {
            s.scored_for_accuracy += 1;
            if p.a.toxic_token == want {
                if want {
                    s.a_correct_on_toxic += 1;
                } else {
                    s.a_correct_on_clean += 1;
                }
            }
            if p.b.toxic_token == want {
                if want {
                    s.b_correct_on_toxic += 1;
                } else {
                    s.b_correct_on_clean += 1;
                }
            }
        }
    }
    s
}

pub async fn run(
    a: Arc<dyn ToxicityClassifier>,
    b: Arc<dyn ToxicityClassifier>,
    input: &Path,
) -> Result<Summary> {
    let body = std::fs::read_to_string(input).with_context(|| format!("read {input:?}"))?;
    let mut pairs = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: FixtureRow = serde_json::from_str(line)
            .with_context(|| format!("parse fixture line {} in {input:?}", i + 1))?;
        let va = a
            .classify(&row.content)
            .await
            .with_context(|| format!("backend {} failed on {}", a.name(), row.id))?;
        let vb = b
            .classify(&row.content)
            .await
            .with_context(|| format!("backend {} failed on {}", b.name(), row.id))?;
        pairs.push(Pair {
            id: row.id,
            label: row.label,
            a: va,
            b: vb,
        });
    }
    let summary = summarize(&pairs, a.name(), b.name());
    let report_path = input.with_extension("compare.jsonl");
    std::fs::write(
        &report_path,
        pairs
            .iter()
            .map(|p| serde_json::to_string(p).unwrap())
            .collect::<Vec<_>>()
            .join("\n"),
    )
    .with_context(|| format!("write {report_path:?}"))?;
    print_summary(&summary);
    Ok(summary)
}

fn print_summary(s: &Summary) {
    println!("=== A/B comparison ({} vs {}) ===", s.a_name, s.b_name);
    println!(
        "total: {}    agreements: {}    disagreements: {}",
        s.total, s.agreements, s.disagreements
    );
    println!("scored_for_accuracy: {}", s.scored_for_accuracy);
    println!(
        "{}: {} correct on toxic, {} correct on clean",
        s.a_name, s.a_correct_on_toxic, s.a_correct_on_clean
    );
    println!(
        "{}: {} correct on toxic, {} correct on clean",
        s.b_name, s.b_correct_on_toxic, s.b_correct_on_clean
    );
}
