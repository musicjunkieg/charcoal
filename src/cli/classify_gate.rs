//! `charcoal classify-gate` — spec migration Step 4.5 shadow-agreement gate.
//!
//! Runs the candidate backend AND the reference (Zentropi) backend over
//! ab_sample.jsonl and asserts they agree on the binary toxic verdict for at
//! least 90% of the sample. Disagreements are dumped to a report file for
//! review. Exits non-zero on any failure so CI can gate prod cutover.

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;

use crate::toxicity::classifier::{ClassifierVerdict, ToxicityClassifier};

pub const MIN_SAMPLE: usize = 50;
pub const AGREEMENT_THRESHOLD: f32 = 0.90;

/// One sampled post, classified by both backends. No ground-truth label —
/// the reference backend's verdict is the comparison target.
#[derive(Debug, Clone, Serialize)]
pub struct GateRow {
    pub id: String,
    pub candidate: ClassifierVerdict,
    pub reference: ClassifierVerdict,
}

#[derive(Debug)]
pub struct GateInputs {
    pub candidate_name: String,
    pub reference_name: String,
    pub rows: Vec<GateRow>,
}

#[derive(Debug, Clone, Serialize)]
pub enum GateOutcome {
    Pass {
        agreement: f32,
        sample: usize,
    },
    Fail {
        agreement: f32,
        sample: usize,
        reason: String,
    },
}

pub fn evaluate(inputs: &GateInputs) -> GateOutcome {
    let sample = inputs.rows.len();
    if sample < MIN_SAMPLE {
        return GateOutcome::Fail {
            agreement: 0.0,
            sample,
            reason: format!("sample too small: {sample} rows (minimum {MIN_SAMPLE})"),
        };
    }

    let agree = inputs
        .rows
        .iter()
        .filter(|r| r.candidate.toxic_token == r.reference.toxic_token)
        .count();
    let agreement = agree as f32 / sample as f32;

    if agreement >= AGREEMENT_THRESHOLD {
        GateOutcome::Pass { agreement, sample }
    } else {
        GateOutcome::Fail {
            agreement,
            sample,
            reason: format!(
                "agreement below {:.0}%: {:.1}% ({}/{} rows)",
                AGREEMENT_THRESHOLD * 100.0,
                agreement * 100.0,
                agree,
                sample
            ),
        }
    }
}

/// Rows where the two backends disagree on the binary verdict.
pub fn disagreements(inputs: &GateInputs) -> Vec<&GateRow> {
    inputs
        .rows
        .iter()
        .filter(|r| r.candidate.toxic_token != r.reference.toxic_token)
        .collect()
}

pub async fn run(
    candidate: Arc<dyn ToxicityClassifier>,
    reference: Arc<dyn ToxicityClassifier>,
    sample_path: &Path,
    disagreements_path: &Path,
) -> Result<GateOutcome> {
    let rows = classify_pairs(&*candidate, &*reference, sample_path).await?;
    let inputs = GateInputs {
        candidate_name: candidate.name().into(),
        reference_name: reference.name().into(),
        rows,
    };

    // Always write the disagreement report (empty file is fine) so reviewers
    // know exactly which posts diverged.
    let disagreeing = disagreements(&inputs);
    if let Some(parent) = disagreements_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let body = disagreeing
        .iter()
        .map(serde_json::to_string)
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("serialize disagreement rows for {disagreements_path:?}"))?
        .join("\n");
    std::fs::write(disagreements_path, body)
        .with_context(|| format!("write {disagreements_path:?}"))?;
    println!(
        "{} disagreement(s) written to {disagreements_path:?}",
        disagreeing.len()
    );

    Ok(evaluate(&inputs))
}

async fn classify_pairs(
    candidate: &dyn ToxicityClassifier,
    reference: &dyn ToxicityClassifier,
    path: &Path,
) -> Result<Vec<GateRow>> {
    let body = std::fs::read_to_string(path).with_context(|| format!("read {path:?}"))?;
    let mut rows = Vec::new();
    for (i, line) in body.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: super::classify_compare::FixtureRow = serde_json::from_str(line)
            .with_context(|| format!("parse line {} in {path:?}", i + 1))?;
        let cand = candidate
            .classify(&row.content)
            .await
            .with_context(|| format!("candidate {} failed on {}", candidate.name(), row.id))?;
        let refr = reference
            .classify(&row.content)
            .await
            .with_context(|| format!("reference {} failed on {}", reference.name(), row.id))?;
        rows.push(GateRow {
            id: row.id,
            candidate: cand,
            reference: refr,
        });
    }
    Ok(rows)
}
