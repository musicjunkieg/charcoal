//! Generalized audit log writer for classifier and NLI events.
//!
//! Each event is one JSONL line. Files rotate daily by UTC date — the filename
//! includes `YYYY-MM-DD`. The on-disk schema is `{kind, ...event-specific-fields}`.
//!
//! The writer takes an explicit `enabled` flag at construction so tests can
//! exercise both paths without env-var fiddling. Production callers use
//! [`AuditWriter::from_env`] which reads the per-kind env var.
//!
//! NOTE: this replaces the older `nli_audit` module which used a single
//! `nli-audit.jsonl` file rotated to dated archives when its first entry
//! exceeded 30 days. The new layout writes a fresh dated file every day.
//! Migration of any orphaned `nli-audit.jsonl` is handled by
//! [`migrate_legacy_nli_audit`].

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::scoring::nli::HypothesisScores;

#[derive(Debug, Clone, Copy)]
pub enum EventKind {
    Classifier,
    Nli,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Classifier => "classifier",
            EventKind::Nli => "nli",
        }
    }

    /// Env var that toggles whether events of this kind are written.
    pub fn env_var(self) -> &'static str {
        match self {
            EventKind::Classifier => "CHARCOAL_AUDIT_CLASSIFIER",
            EventKind::Nli => "CHARCOAL_AUDIT_NLI",
        }
    }
}

/// Pure path-formatting helper. Public so tests can exercise rotation
/// without running the clock forward.
pub fn format_log_path(dir: &Path, kind: EventKind, when: DateTime<Utc>) -> PathBuf {
    let date = when.format("%Y-%m-%d").to_string();
    dir.join(format!("{}-{}.jsonl", kind.as_str(), date))
}

/// Classifier-side event payload. Constructed via [`AuditEvent::classifier`].
#[derive(Debug, Clone)]
pub struct ClassifierFields {
    pub backend: String,
    pub model_id: String,
    pub policy_version: String,
    pub prompt_hash: String,
    pub toxic: bool,
    pub confidence: f32,
    pub latency_ms: u32,
}

/// NLI-side event payload. Mirrors the legacy `NliAuditEntry` field set.
#[derive(Debug, Clone)]
pub struct NliFields {
    pub target_did: String,
    pub target_handle: String,
    pub pair_type: String,
    pub original_text: String,
    pub response_text: String,
    pub hypothesis_scores: HypothesisScores,
    pub hostility_score: f64,
    pub similarity: Option<f64>,
}

/// Audit events are write-only (the writer never reads them back), so we only
/// derive `Serialize`. `HypothesisScores` in `src/scoring/nli.rs` similarly
/// derives only `Serialize`; adding `Deserialize` here would force adding it
/// there too, which we don't need.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AuditEvent {
    Classifier {
        timestamp: String,
        backend: String,
        model_id: String,
        policy_version: String,
        prompt_hash: String,
        toxic: bool,
        confidence: f32,
        latency_ms: u32,
    },
    Nli {
        timestamp: String,
        target_did: String,
        target_handle: String,
        pair_type: String,
        original_text: String,
        response_text: String,
        hypothesis_scores: HypothesisScores,
        hostility_score: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        similarity: Option<f64>,
    },
}

impl AuditEvent {
    pub fn classifier(fields: ClassifierFields) -> Self {
        AuditEvent::Classifier {
            timestamp: now_rfc3339(),
            backend: fields.backend,
            model_id: fields.model_id,
            policy_version: fields.policy_version,
            prompt_hash: fields.prompt_hash,
            toxic: fields.toxic,
            confidence: fields.confidence,
            latency_ms: fields.latency_ms,
        }
    }

    pub fn nli(fields: NliFields) -> Self {
        AuditEvent::Nli {
            timestamp: now_rfc3339(),
            target_did: fields.target_did,
            target_handle: fields.target_handle,
            pair_type: fields.pair_type,
            original_text: fields.original_text,
            response_text: fields.response_text,
            hypothesis_scores: fields.hypothesis_scores,
            hostility_score: fields.hostility_score,
            similarity: fields.similarity,
        }
    }
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub struct AuditWriter {
    dir: PathBuf,
    kind: EventKind,
    enabled: bool,
}

impl AuditWriter {
    /// Build a writer with the gate set explicitly. Use in tests.
    pub fn new(dir: &Path, kind: EventKind, enabled: bool) -> Result<Self> {
        std::fs::create_dir_all(dir).context("create audit log dir")?;
        Ok(Self {
            dir: dir.to_path_buf(),
            kind,
            enabled,
        })
    }

    /// Build a writer reading the gate from the kind's env var.
    ///
    /// Strict by design: the writer is enabled ONLY when the env var's value is
    /// exactly `"1"`. Any other value — `true`, `yes`, `0`, empty, unset — leaves
    /// it disabled. This is intentional and spec-backed; do not loosen it.
    pub fn from_env(dir: &Path, kind: EventKind) -> Result<Self> {
        let enabled = std::env::var(kind.env_var()).ok().as_deref() == Some("1");
        Self::new(dir, kind, enabled)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub fn current_path(&self) -> PathBuf {
        format_log_path(&self.dir, self.kind, Utc::now())
    }

    pub fn record(&self, event: AuditEvent) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let line = serde_json::to_string(&event).context("serialize audit event")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.current_path())
            .context("open audit log")?;
        writeln!(file, "{}", line).context("write audit line")?;
        Ok(())
    }
}

/// One-time rename of any pre-generalization NLI audit file so it isn't orphaned
/// after rotation changes from "single file + 30-day archive" to "one file per day".
/// Safe to call on every boot; no-op if the file is absent.
pub fn migrate_legacy_nli_audit(dir: &Path) {
    let legacy = dir.join("nli-audit.jsonl");
    if !legacy.exists() {
        return;
    }
    let target = dir.join(format!(
        "nli-legacy-{}.jsonl",
        Utc::now().format("%Y-%m-%d")
    ));
    match std::fs::rename(&legacy, &target) {
        Ok(()) => tracing::info!(
            from = %legacy.display(),
            to = %target.display(),
            "Migrated legacy NLI audit file"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "Failed to rename legacy nli-audit.jsonl"
        ),
    }
}
