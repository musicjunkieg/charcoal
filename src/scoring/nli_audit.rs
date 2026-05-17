//! NLI audit logging — structured records for every NLI scoring call.
//!
//! Emits to both tracing (Railway log dashboard) and a JSONL file
//! on the persistent volume with 30-day rotation.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use tracing::info;

use crate::scoring::nli::HypothesisScores;

/// A single NLI audit log entry.
#[derive(Debug, Serialize)]
pub struct NliAuditEntry {
    pub timestamp: String,
    pub target_did: String,
    pub target_handle: String,
    pub pair_type: String,
    pub original_text: String,
    pub response_text: String,
    pub hypothesis_scores: HypothesisScores,
    pub hostility_score: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity: Option<f64>,
}

/// Emit an audit entry to tracing and append to the JSONL file.
pub fn log_nli_audit(entry: &NliAuditEntry, data_dir: Option<&Path>) {
    info!(
        target_did = entry.target_did,
        target_handle = entry.target_handle,
        pair_type = entry.pair_type,
        hostility_score = format!("{:.3}", entry.hostility_score),
        attack = format!("{:.3}", entry.hypothesis_scores.attack),
        contempt = format!("{:.3}", entry.hypothesis_scores.contempt),
        misrepresent = format!("{:.3}", entry.hypothesis_scores.misrepresent),
        good_faith = format!("{:.3}", entry.hypothesis_scores.good_faith_disagree),
        support = format!("{:.3}", entry.hypothesis_scores.support),
        "NLI audit"
    );

    if let Some(dir) = data_dir {
        if let Err(e) = append_jsonl(entry, dir) {
            tracing::warn!(error = %e, "Failed to write NLI audit JSONL");
        }
    }
}

/// Append one JSON line to the audit file. Rotates if first entry is >30 days old.
fn append_jsonl(entry: &NliAuditEntry, data_dir: &Path) -> anyhow::Result<()> {
    let audit_path = data_dir.join("nli-audit.jsonl");

    if audit_path.exists() {
        if let Ok(file) = std::fs::File::open(&audit_path) {
            use std::io::BufRead;
            if let Some(Ok(first_line)) = std::io::BufReader::new(file).lines().next() {
                if should_rotate(&first_line) {
                    rotate_audit_file(&audit_path, data_dir)?;
                }
            }
        }
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&audit_path)?;

    let json = serde_json::to_string(entry)?;
    writeln!(file, "{}", json)?;

    Ok(())
}

/// Check if the first JSONL entry's timestamp is more than 30 days old.
/// Public for testing.
pub fn should_rotate(first_line: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct TimestampOnly {
        timestamp: String,
    }

    if let Ok(entry) = serde_json::from_str::<TimestampOnly>(first_line) {
        if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp) {
            let age = chrono::Utc::now().signed_duration_since(ts);
            return age.num_days() >= 30;
        }
    }
    false
}

/// Rotate: rename current file to dated archive, start fresh.
fn rotate_audit_file(audit_path: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let date_str = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let archive_name = format!("nli-audit-{}.jsonl", date_str);
    let archive_path = data_dir.join(archive_name);

    std::fs::rename(audit_path, &archive_path)?;
    tracing::info!(
        archive = archive_path.display().to_string(),
        "Rotated NLI audit log"
    );

    Ok(())
}
