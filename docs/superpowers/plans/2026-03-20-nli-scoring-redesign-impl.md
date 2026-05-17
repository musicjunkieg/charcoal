# NLI Scoring Redesign Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the blended 60/40 toxicity/context formula with a multiplicative context_multiplier, score amplifiers via build_profile() with direct text pairs, and add threshold-gated NLI for followers.

**Architecture:** The NLI context_score becomes a multiplier (1.0–1.5x) applied after behavioral boost, rather than blending into toxicity. Amplifiers get NLI-scored using their actual event texts (direct pairs). Followers only get NLI scoring when their raw_score >= 8.0 (Watch threshold), using embedding-matched inferred pairs. Audit logging captures every NLI call to structured tracing + JSONL file with 30-day rotation.

**Tech Stack:** Rust, ort (ONNX), tokenizers, tracing, serde_json, chrono

**Spec:** `docs/superpowers/specs/2026-03-20-nli-scoring-redesign.md`

**Branch:** `feat/nli-scoring-redesign` (from `staging`)

**Spec deviations (intentional):**
- Railway Bucket upload for rotated JSONL files is deferred to a follow-up. Initial implementation does local file rename only. The `/data` volume on Railway persists across deploys so archived files are durable enough for now.
- Two-pass follower scoring calls `build_profile` twice for Watch+ accounts (once without NLI, once with). The spec envisions a single pass with conditional NLI inside `build_profile`. The two-pass approach is simpler to implement without restructuring `build_profile`'s control flow. A follow-up can optimize by moving the threshold gate inside `build_profile` if the double-fetch cost is significant.

---

## Chunk 1: Pure Scoring Functions (avg_context_score, context_multiplier, score_pair return type)

These are all pure functions with no I/O — the foundation everything else builds on.

### Task 1: Replace max_context_score_opt with avg_context_score

**Files:**
- Modify: `src/scoring/nli.rs:50-59`
- Test: `tests/unit_nli.rs`

- [ ] **Step 1: Write failing tests for avg_context_score**

Add to `tests/unit_nli.rs` (replace the three `max_context_score_opt` tests):

```rust
#[test]
fn avg_context_score_from_multiple_pairs() {
    let scores = vec![0.3, 0.7, 0.5, 0.2];
    let result = avg_context_score(&scores);
    // (0.3 + 0.7 + 0.5 + 0.2) / 4 = 0.425
    assert!((result.unwrap() - 0.425).abs() < 0.001);
}

#[test]
fn avg_context_score_empty_returns_none() {
    let scores: Vec<f64> = vec![];
    assert!(avg_context_score(&scores).is_none());
}

#[test]
fn avg_context_score_single_value() {
    let scores = vec![0.42];
    assert_eq!(avg_context_score(&scores), Some(0.42));
}
```

Update the import line at the top of the test file:
```rust
use charcoal::scoring::nli::{compute_hostility_score, avg_context_score, HypothesisScores};
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_nli -- avg_context_score`
Expected: FAIL — `avg_context_score` not found

- [ ] **Step 3: Implement avg_context_score**

In `src/scoring/nli.rs`, replace the `max_context_score_opt` function (lines 50-59) with:

```rust
/// Return the average context score, or None if no scores provided.
/// Uses the mean across all scored pairs to capture overall engagement
/// patterns rather than worst-case moments.
pub fn avg_context_score(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        None
    } else {
        Some(scores.iter().sum::<f64>() / scores.len() as f64)
    }
}
```

- [ ] **Step 4: Update the call site in profile.rs**

In `src/scoring/profile.rs` line 221, change:
```rust
crate::scoring::nli::max_context_score_opt(&pair_scores)
```
to:
```rust
crate::scoring::nli::avg_context_score(&pair_scores)
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test unit_nli`
Expected: All NLI tests PASS

- [ ] **Step 6: Run full test suite to check for regressions**

Run: `cargo test --features web`
Expected: All tests PASS (no other code references `max_context_score_opt`)

- [ ] **Step 7: Commit**

```bash
git add src/scoring/nli.rs src/scoring/profile.rs tests/unit_nli.rs
git commit -m 'feat: replace max_context_score_opt with avg_context_score

Average across all pairs captures engagement patterns rather than
worst-case moments. Spec: nli-scoring-redesign §Context Score Aggregation'
```

---

### Task 2: Replace blended threat formula with context_multiplier

**Files:**
- Modify: `src/scoring/threat.rs:69-88`
- Test: `src/scoring/threat.rs` (inline tests)

- [ ] **Step 1: Write failing tests for context_multiplier formula**

Add to the `#[cfg(test)] mod tests` block in `src/scoring/threat.rs`:

```rust
#[test]
fn context_multiplier_none_no_change() {
    let weights = ThreatWeights::default();
    let (with_ctx, _) = compute_threat_score_contextual(0.3, 0.4, None, &weights);
    let (without_ctx, _) = compute_threat_score(0.3, 0.4, &weights);
    assert!((with_ctx - without_ctx).abs() < 0.001);
}

#[test]
fn context_multiplier_zero_no_change() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.3, 0.4, &weights);
    let (ctx, _) = compute_threat_score_contextual(0.3, 0.4, Some(0.0), &weights);
    assert!((ctx - base).abs() < 0.001, "ctx=0.0 should multiply by 1.0x, got {ctx} vs {base}");
}

#[test]
fn context_multiplier_moderate_boosts_25pct() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.3, 0.4, &weights);
    let (ctx, _) = compute_threat_score_contextual(0.3, 0.4, Some(0.5), &weights);
    let expected = base * 1.25;
    assert!((ctx - expected).abs() < 0.1, "ctx=0.5 should be ~1.25x base, got {ctx} vs {expected}");
}

#[test]
fn context_multiplier_extreme_boosts_50pct() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.3, 0.4, &weights);
    let (ctx, _) = compute_threat_score_contextual(0.3, 0.4, Some(1.0), &weights);
    let expected = base * 1.5;
    assert!((ctx - expected).abs() < 0.1, "ctx=1.0 should be ~1.5x base, got {ctx} vs {expected}");
}

#[test]
fn context_multiplier_zero_toxicity_stays_zero() {
    let weights = ThreatWeights::default();
    let (score, tier) = compute_threat_score_contextual(0.0, 0.5, Some(1.0), &weights);
    assert!((score - 0.0).abs() < 0.001, "Zero tox + any context = 0, got {score}");
    assert_eq!(tier, ThreatTier::Low);
}

#[test]
fn context_multiplier_watch_borderline_promoted() {
    // raw_score = 0.12 * 70 * (1 + 0.35 * 1.5) = 8.4 * 1.525 = 12.81
    // with ctx=1.0: 12.81 * 1.5 = 19.2 → Elevated (was Watch)
    let weights = ThreatWeights::default();
    let (score, tier) = compute_threat_score_contextual(0.12, 0.35, Some(1.0), &weights);
    assert!(score > 15.0, "Borderline Watch + extreme context should promote to Elevated, got {score}");
    assert_eq!(tier, ThreatTier::Elevated);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib scoring::threat`
Expected: FAIL — the blended formula produces different values than multiplied

- [ ] **Step 3: Implement context_multiplier formula**

Replace `compute_threat_score_contextual` in `src/scoring/threat.rs` (lines 69-88) with:

```rust
/// Compute the combined threat score with optional context multiplier.
///
/// When a context_score is provided (from NLI pair scoring), it amplifies
/// the base threat score multiplicatively:
///   context_multiplier = 1.0 + (context_score * 0.5)   // range: 1.0–1.5
///   final = base_score * context_multiplier
///
/// This ensures context can only boost existing threat signals — an account
/// with zero toxicity stays at zero regardless of context_score.
pub fn compute_threat_score_contextual(
    toxicity: f64,
    topic_overlap: f64,
    context_score: Option<f64>,
    weights: &ThreatWeights,
) -> (f64, ThreatTier) {
    let (base_score, _) = compute_threat_score(toxicity, topic_overlap, weights);
    let context_multiplier = match context_score {
        Some(ctx) => 1.0 + (ctx * 0.5),
        None => 1.0,
    };
    let score = (base_score * context_multiplier).clamp(0.0, 100.0);
    let tier = ThreatTier::from_score(score);
    (score, tier)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib scoring::threat`
Expected: All threat tests PASS

- [ ] **Step 5: Run full test suite**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/scoring/threat.rs
git commit -m 'feat: replace blended 60/40 formula with multiplicative context_multiplier

context_multiplier = 1.0 + (context_score * 0.5), range 1.0-1.5x.
Context amplifies existing threat signals instead of replacing toxicity.
Zero toxicity + any context = zero threat score.

Spec: nli-scoring-redesign §Threat Score Formula'
```

---

### Task 3: Return HypothesisScores from score_pair

**Files:**
- Modify: `src/scoring/nli.rs:209-245`
- Modify: `src/pipeline/amplification.rs:108-127` (update call site)
- Modify: `src/scoring/profile.rs:214` (update call site)

- [ ] **Step 1: Change score_pair return type**

In `src/scoring/nli.rs`, change the `score_pair` method signature (line 209) to:

```rust
    pub async fn score_pair(&self, original_text: &str, response_text: &str) -> Result<(f64, HypothesisScores)> {
```

And the return line (line 244) from `Ok(hostility)` to:

```rust
        Ok((hostility, hypothesis_scores))
```

- [ ] **Step 2: Update call site in amplification.rs**

In `src/pipeline/amplification.rs` line 111, change:
```rust
match nli.score_pair(orig_text, amp_text).await {
    Ok(score) => {
```
to:
```rust
match nli.score_pair(orig_text, amp_text).await {
    Ok((score, _hypothesis_scores)) => {
```

- [ ] **Step 3: Update call site in profile.rs**

In `src/scoring/profile.rs` line 214, change:
```rust
match nli.score_pair(original, target_text).await {
    Ok(score) => pair_scores.push(score),
```
to:
```rust
match nli.score_pair(original, target_text).await {
    Ok((score, _hypothesis_scores)) => pair_scores.push(score),
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test --features web`
Expected: All tests PASS (no tests call `score_pair` directly — it requires a live model)

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean (the `_hypothesis_scores` prefix suppresses unused warnings)

- [ ] **Step 6: Commit**

```bash
git add src/scoring/nli.rs src/pipeline/amplification.rs src/scoring/profile.rs
git commit -m 'refactor: return HypothesisScores from score_pair for audit logging

score_pair now returns (f64, HypothesisScores) so callers can log
the full hypothesis breakdown. Prepares for NLI audit logging.

Spec: nli-scoring-redesign §NLI Audit Logging'
```

---

## Chunk 2: NLI Audit Logging

Structured tracing + JSONL file logging with 30-day rotation. Self-contained infrastructure used by all NLI call sites.

### Task 4: Add NLI audit logging module

**Files:**
- Create: `src/scoring/nli_audit.rs`
- Modify: `src/scoring/mod.rs` (add module declaration)
- Modify: `src/scoring/nli.rs:29` (add Serialize derive to HypothesisScores)
- Test: `tests/unit_nli.rs`

- [ ] **Step 1: Write failing tests for NliAuditEntry serialization**

Add to `tests/unit_nli.rs`:

```rust
use charcoal::scoring::nli_audit::NliAuditEntry;

#[test]
fn nli_audit_entry_serializes_to_json() {
    let entry = NliAuditEntry {
        timestamp: "2026-03-20T12:00:00Z".to_string(),
        target_did: "did:plc:abc123".to_string(),
        target_handle: "test.bsky.social".to_string(),
        pair_type: "direct".to_string(),
        original_text: "Original post".to_string(),
        response_text: "Response post".to_string(),
        hypothesis_scores: HypothesisScores {
            attack: 0.8,
            contempt: 0.3,
            misrepresent: 0.1,
            good_faith_disagree: 0.05,
            support: 0.02,
        },
        hostility_score: 0.78,
        similarity: None,
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"pair_type\":\"direct\""));
    assert!(json.contains("\"hostility_score\":0.78"));
    // None fields with skip_serializing_if should be absent
    assert!(!json.contains("similarity"));
}

#[test]
fn nli_audit_entry_with_similarity() {
    let entry = NliAuditEntry {
        timestamp: "2026-03-20T12:00:00Z".to_string(),
        target_did: "did:plc:abc123".to_string(),
        target_handle: "test.bsky.social".to_string(),
        pair_type: "inferred".to_string(),
        original_text: "Original".to_string(),
        response_text: "Response".to_string(),
        hypothesis_scores: HypothesisScores {
            attack: 0.1,
            contempt: 0.1,
            misrepresent: 0.1,
            good_faith_disagree: 0.5,
            support: 0.7,
        },
        hostility_score: 0.0,
        similarity: Some(0.85),
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(json.contains("\"similarity\":0.85"));
    assert!(json.contains("\"pair_type\":\"inferred\""));
}
```

Also add rotation boundary tests:

```rust
use charcoal::scoring::nli_audit::should_rotate;

#[test]
fn should_rotate_true_for_old_entry() {
    // 31 days ago
    let old_ts = (chrono::Utc::now() - chrono::Duration::days(31)).to_rfc3339();
    let line = format!(r#"{{"timestamp":"{}","target_did":"x"}}"#, old_ts);
    assert!(should_rotate(&line));
}

#[test]
fn should_rotate_false_for_recent_entry() {
    let recent_ts = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
    let line = format!(r#"{{"timestamp":"{}","target_did":"x"}}"#, recent_ts);
    assert!(!should_rotate(&line));
}

#[test]
fn should_rotate_false_for_invalid_json() {
    assert!(!should_rotate("not valid json"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_nli -- nli_audit should_rotate`
Expected: FAIL — `nli_audit` module doesn't exist

- [ ] **Step 3: Add Serialize derive to HypothesisScores**

In `src/scoring/nli.rs`, update line 29:

```rust
#[derive(Debug, Clone, serde::Serialize)]
pub struct HypothesisScores {
```

- [ ] **Step 4: Create the nli_audit module**

Create `src/scoring/nli_audit.rs`:

```rust
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
    // Structured tracing — visible in Railway log dashboard
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

    // JSONL file append (best-effort — don't fail the pipeline on I/O errors)
    if let Some(dir) = data_dir {
        if let Err(e) = append_jsonl(entry, dir) {
            tracing::warn!(error = %e, "Failed to write NLI audit JSONL");
        }
    }
}

/// Append one JSON line to the audit file. Rotates if first entry is >30 days old.
fn append_jsonl(entry: &NliAuditEntry, data_dir: &Path) -> anyhow::Result<()> {
    let audit_path = data_dir.join("nli-audit.jsonl");

    // Check rotation: read only the first line (not the whole file)
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
```

- [ ] **Step 5: Register the module in mod.rs**

In `src/scoring/mod.rs`, add:

```rust
pub mod nli_audit;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test unit_nli`
Expected: All NLI tests PASS (including audit entry serialization)

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 8: Commit**

```bash
git add src/scoring/nli_audit.rs src/scoring/mod.rs src/scoring/nli.rs tests/unit_nli.rs
git commit -m 'feat: add NLI audit logging with JSONL file rotation

Structured tracing entries for Railway log dashboard + JSONL file
at {data_dir}/nli-audit.jsonl with 30-day rotation. Each entry
captures full hypothesis breakdown for debugging and validation.

Spec: nli-scoring-redesign §NLI Audit Logging'
```

---

## Chunk 3: Amplifier Scoring via build_profile with Direct Pairs

The core pipeline change: after recording events, collect unique amplifier DIDs, gather their text pairs, and call `build_profile()` with NLI scoring enabled.

### Task 5: Add direct_pairs parameter to build_profile

**Files:**
- Modify: `src/scoring/profile.rs:37-50` (add parameter)
- Modify: `src/scoring/profile.rs:157-231` (use direct_pairs when present)
- Modify: `src/pipeline/amplification.rs:240-253` (pass None for followers)
- Modify: `src/pipeline/sweep.rs` (pass None at build_profile call site)
- Modify: `src/main.rs` (pass None at all build_profile call sites)

- [ ] **Step 1: Add direct_pairs parameter to build_profile signature**

In `src/scoring/profile.rs`, update the function signature (lines 37-50). Add after `protected_posts_with_embeddings`:

```rust
pub async fn build_profile(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    direct_pairs: Option<&[(String, String)]>,
) -> Result<AccountScore> {
```

- [ ] **Step 2: Update NLI scoring block to branch on direct_pairs**

Replace the context_score block in `src/scoring/profile.rs` (lines 157-231) with:

```rust
    // Step 5: Compute context score via NLI
    //
    // Two modes:
    // - Direct pairs (amplifiers): NLI-score the actual event texts (amplifier
    //   quote/reply paired with the original post they interacted with).
    // - Inferred pairs (followers): find top 3 most similar posts by embedding
    //   and pair each with the closest matching protected user post.
    let context_score = if let Some(nli) = nli_scorer {
        if let Some(pairs) = direct_pairs {
            // Mode A: Direct pairs — score each real interaction
            if pairs.is_empty() {
                None
            } else {
                let mut pair_scores = Vec::new();
                for (original, response) in pairs {
                    match nli.score_pair(original, response).await {
                        Ok((score, _scores)) => pair_scores.push(score),
                        Err(e) => {
                            warn!(error = %e, "NLI scoring failed for direct pair");
                        }
                    }
                }
                crate::scoring::nli::avg_context_score(&pair_scores)
            }
        } else if let (Some(emb), Some(user_posts)) = (embedder, protected_posts_with_embeddings) {
            // Mode B: Inferred pairs — embedding-matched
            if user_posts.is_empty() {
                None
            } else {
                match emb.embed_batch(&post_texts).await {
                    Ok(target_embeddings) => {
                        let target_with_emb: Vec<(String, Vec<f64>)> = post_texts
                            .iter()
                            .zip(target_embeddings.into_iter())
                            .map(|(text, emb)| (text.clone(), emb))
                            .collect();

                        let user_mean: Vec<f64> = {
                            let dim = user_posts[0].1.len();
                            let mut mean = vec![0.0; dim];
                            for (_, emb) in user_posts {
                                for (i, v) in emb.iter().enumerate() {
                                    mean[i] += v;
                                }
                            }
                            let n = user_posts.len() as f64;
                            mean.iter_mut().for_each(|v| *v /= n);
                            mean
                        };
                        let top_target_posts = crate::scoring::context::find_most_similar_posts(
                            &user_mean,
                            &target_with_emb,
                            3,
                        );

                        let mut pair_scores = Vec::new();
                        for (target_text, _similarity) in &top_target_posts {
                            let target_emb = target_with_emb
                                .iter()
                                .find(|(t, _)| t == target_text)
                                .map(|(_, e)| e.as_slice());

                            let user_text = target_emb.and_then(|emb| {
                                crate::scoring::context::find_best_matching_user_post(emb, user_posts)
                            });

                            let original = user_text.as_deref().unwrap_or("");
                            if original.is_empty() {
                                continue;
                            }

                            match nli.score_pair(original, target_text).await {
                                Ok((score, _scores)) => pair_scores.push(score),
                                Err(e) => {
                                    warn!(error = %e, "NLI scoring failed for inferred pair");
                                }
                            }
                        }
                        crate::scoring::nli::avg_context_score(&pair_scores)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to embed target posts for NLI");
                        None
                    }
                }
            }
        } else {
            None
        }
    } else {
        None
    };
```

- [ ] **Step 3: Update all existing build_profile call sites to pass None for direct_pairs**

There are 4 call sites. Add `None, // direct_pairs` as the last argument to each:

1. `src/pipeline/amplification.rs` (~line 251) — follower scoring closure
2. `src/pipeline/sweep.rs` — sweep build_profile call
3. `src/main.rs` score command (~line 425)
4. `src/main.rs` validate command (~line 589)

- [ ] **Step 4: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS — no behavior change, all call sites pass None

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 6: Commit**

```bash
git add src/scoring/profile.rs src/pipeline/amplification.rs src/pipeline/sweep.rs src/main.rs
git commit -m 'feat: add direct_pairs parameter to build_profile

When present, NLI scores real event texts instead of embedding-matched
inferred pairs. All existing call sites pass None (no behavior change).
Amplifier scoring will populate this in the next commit.

Spec: nli-scoring-redesign §Direct Pairs for Amplifiers'
```

---

### Task 6: Add get_events_by_amplifier to Database trait

**Files:**
- Modify: `src/db/traits.rs` (add trait method)
- Modify: `src/db/sqlite.rs` (implement for SQLite)
- Modify: `src/db/postgres.rs` (implement for Postgres, behind feature gate)

- [ ] **Step 1: Check existing trait methods**

Read `src/db/traits.rs` to find where to add the new method and verify the pattern used by existing methods. Also check `AmplificationEvent` field names in `src/db/models.rs`.

- [ ] **Step 2: Add trait method**

In `src/db/traits.rs`, add to the `Database` trait (near other event-related methods):

```rust
    /// Get all amplification events for a specific amplifier DID.
    async fn get_events_by_amplifier(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> Result<Vec<AmplificationEvent>>;
```

- [ ] **Step 3: Implement for SqliteDatabase**

In `src/db/sqlite.rs`, add:

```rust
    async fn get_events_by_amplifier(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> Result<Vec<AmplificationEvent>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, event_type, amplifier_did, amplifier_handle, original_post_uri,
                    amplifier_post_uri, amplifier_text, detected_at, followers_fetched,
                    followers_scored, original_post_text, context_score
             FROM amplification_events
             WHERE user_did = ?1 AND amplifier_did = ?2
             ORDER BY detected_at DESC"
        )?;
        let events = stmt.query_map(rusqlite::params![user_did, amplifier_did], |row| {
            Ok(AmplificationEvent {
                id: row.get(0)?,
                event_type: row.get(1)?,
                amplifier_did: row.get(2)?,
                amplifier_handle: row.get(3)?,
                original_post_uri: row.get(4)?,
                amplifier_post_uri: row.get(5)?,
                amplifier_text: row.get(6)?,
                detected_at: row.get(7)?,
                followers_fetched: row.get::<_, i32>(8)? != 0,
                followers_scored: row.get::<_, i32>(9)? != 0,
                original_post_text: row.get(10)?,
                context_score: row.get(11)?,
            })
        })?.collect::<Result<Vec<_>, _>>()?;
        Ok(events)
    }
```

- [ ] **Step 4: Implement for PgDatabase**

In `src/db/postgres.rs` (behind `#[cfg(feature = "postgres")]`), add the equivalent using sqlx. Match the query pattern used by other Postgres methods in that file.

- [ ] **Step 5: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/db/traits.rs src/db/sqlite.rs src/db/postgres.rs
git commit -m 'feat: add get_events_by_amplifier to Database trait

Query all events for a specific amplifier DID. Used by the amplifier
scoring loop to gather direct text pairs for NLI scoring.'
```

---

### Task 7: Add Phase B amplifier scoring loop

**Files:**
- Modify: `src/pipeline/amplification.rs` (insert after event recording, before follower scoring)

- [ ] **Step 1: Insert amplifier scoring loop**

In `src/pipeline/amplification.rs`, after the event recording loop ends (after line 160, the closing `}` of the `for event in &events` loop), and before `let mut accounts_scored = 0;` (line 162), insert:

```rust
    // Phase B: Score amplifiers via build_profile() with direct NLI pairs.
    //
    // Collect unique amplifier DIDs and their text pairs from stored events,
    // then run full profile builds. This gives each amplifier a threat tier
    // informed by their actual interactions with the protected user.
    let mut accounts_scored = 0;
    {
        let mut amplifier_handles: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();

        for event in &events {
            amplifier_handles
                .entry(event.amplifier_did.clone())
                .or_insert_with(|| event.amplifier_handle.clone());
        }

        let amplifier_count = amplifier_handles.len();
        if amplifier_count > 0 {
            println!("\nScoring {} amplifiers…", amplifier_count);

            for (did, handle) in &amplifier_handles {
                if handle == protected_handle {
                    continue;
                }
                if !db.is_score_stale(user_did, did, 7).await.unwrap_or(true) {
                    continue;
                }

                // Gather direct text pairs from stored events
                let mut pairs: Vec<(String, String)> = Vec::new();
                if let Ok(db_events) = db.get_events_by_amplifier(user_did, did).await {
                    for ev in db_events {
                        if let (Some(orig), Some(amp)) = (ev.original_post_text, ev.amplifier_text) {
                            if !orig.is_empty() && !amp.is_empty() {
                                pairs.push((orig, amp));
                            }
                        }
                    }
                }

                match profile::build_profile(
                    client,
                    scorer,
                    handle,
                    did,
                    protected_fingerprint,
                    weights,
                    embedder,
                    protected_embedding,
                    median_engagement,
                    pile_on_dids,
                    nli_scorer,
                    None, // No inferred pairs — using direct pairs
                    Some(&pairs),
                )
                .await
                {
                    Ok(score) => {
                        db.upsert_account_score(user_did, &score).await?;
                        accounts_scored += 1;
                        println!(
                            "  @{}: {} (context: {})",
                            handle,
                            score.threat_tier.as_deref().unwrap_or("?"),
                            score
                                .context_score
                                .map(|s| format!("{:.2}", s))
                                .unwrap_or_else(|| "n/a".to_string())
                        );
                    }
                    Err(e) => {
                        warn!(handle = handle.as_str(), error = %e, "Failed to score amplifier");
                    }
                }
            }
        }
    }
```

Also remove the duplicate `let mut accounts_scored = 0;` that was on line 162 (the new code declares it).

- [ ] **Step 2: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 4: Commit**

```bash
git add src/pipeline/amplification.rs
git commit -m 'feat: score amplifiers via build_profile with direct NLI pairs

After recording events, each unique amplifier gets a full profile
build. Their actual quote/reply texts are gathered from stored events
and passed as direct_pairs for NLI scoring.

Spec: nli-scoring-redesign §Pipeline Flow Phase B'
```

---

## Chunk 4: Threshold-Gated Follower NLI + Protected Post Embeddings

### Task 8: Build protected_posts_with_embeddings in scan_job and pass through pipeline

**Files:**
- Modify: `src/web/scan_job.rs` (~line 228, compute per-post embeddings)
- Modify: `src/pipeline/amplification.rs` (add parameter to `run` signature)
- Modify: `src/main.rs` (pass None at `amplification::run` call site)

- [ ] **Step 1: Add protected_posts_with_embeddings to amplification::run signature**

In `src/pipeline/amplification.rs`, add a new parameter to `run` after `nli_scorer`:

```rust
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
```

- [ ] **Step 2: Build per-post embeddings in scan_job.rs**

In `src/web/scan_job.rs`, after line 228 (`let protected_embedding = ...`) and before the Phase 4 comment, add:

```rust
    // Build per-post embeddings for follower NLI inferred pair matching.
    // Each protected post gets its own embedding so followers' posts can be
    // matched to the closest protected post for NLI pair scoring.
    let protected_posts_with_embeddings: Option<Vec<(String, Vec<f64>)>> =
        if embedder.is_some() && nli_scorer.is_some() {
            // Re-use the posts we already fetched (or fetch fresh 50)
            let pp_texts: Vec<String> = crate::bluesky::posts::fetch_recent_posts(
                &client, actor_handle, 50,
            )
            .await
            .unwrap_or_default()
            .iter()
            .map(|p| p.text.clone())
            .collect();

            if let Some(ref emb) = embedder {
                match emb.embed_batch(&pp_texts).await {
                    Ok(embeddings) => Some(
                        pp_texts.into_iter().zip(embeddings.into_iter()).collect()
                    ),
                    Err(e) => {
                        warn!(error = %e, "Failed to embed protected posts for NLI pairs");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };
```

- [ ] **Step 3: Pass to amplification::run in scan_job.rs**

Update the `amplification::run` call (~line 365) to pass the new parameter:

```rust
        nli_scorer.as_ref(),
        protected_posts_with_embeddings.as_deref(),
    )
```

- [ ] **Step 4: Pass None in main.rs CLI scan command**

Update the `amplification::run` call in `src/main.rs` (~line 319) to add `None`:

```rust
        None, // NLI scorer not loaded in CLI mode
        None, // No protected post embeddings in CLI mode
    )
```

- [ ] **Step 5: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 6: Commit**

```bash
git add src/web/scan_job.rs src/pipeline/amplification.rs src/main.rs
git commit -m 'feat: build per-post embeddings for follower NLI pair matching

Compute individual embeddings for each protected user post so
followers can be matched to the closest post for NLI inference.
Only computed when both embedder and NLI scorer are available.

Spec: nli-scoring-redesign §Inferred Pairs for Followers'
```

---

### Task 9: Implement two-pass follower scoring with threshold-gated NLI

**Files:**
- Modify: `src/pipeline/amplification.rs` (follower scoring block, ~lines 237-259)
- Test: `tests/unit_nli.rs` (threshold boundary test)

- [ ] **Step 1: Write boundary test for Watch threshold**

Add to `tests/unit_nli.rs`:

```rust
#[test]
fn watch_threshold_is_8() {
    use charcoal::db::models::ThreatTier;
    assert_eq!(ThreatTier::from_score(8.0), ThreatTier::Watch);
    assert_eq!(ThreatTier::from_score(7.9), ThreatTier::Low);
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --test unit_nli -- watch_threshold`
Expected: PASS

- [ ] **Step 3: Replace follower scoring closure with two-pass approach**

In `src/pipeline/amplification.rs`, replace the scoring stream closure (the `stream::iter(...).map(|follower| { ... })` block) with a two-pass version:

```rust
                    let nli_ref = nli_scorer;
                    let ppwe_ref = protected_posts_with_embeddings;

                    let mut stream = stream::iter(stale_followers.into_iter().map(|follower| {
                        let handle_for_panic = follower.handle.clone();
                        async move {
                            // Pass 1: score without NLI (fast)
                            let result = AssertUnwindSafe(profile::build_profile(
                                client,
                                scorer,
                                &follower.handle,
                                &follower.did,
                                protected_fingerprint,
                                weights,
                                embedder,
                                protected_embedding,
                                median_engagement,
                                pile_on_dids,
                                None, // No NLI in pass 1
                                None, // No protected post embeddings
                                None, // No direct pairs
                            ))
                            .catch_unwind()
                            .await
                            .unwrap_or_else(|_| {
                                Err(anyhow::anyhow!("Panic while scoring @{}", handle_for_panic))
                            });

                            match result {
                                Ok(ref score)
                                    if score.threat_score.unwrap_or(0.0) >= 8.0
                                        && nli_ref.is_some()
                                        && ppwe_ref.is_some() =>
                                {
                                    // Pass 2: above Watch threshold — re-score with NLI
                                    info!(
                                        handle = follower.handle.as_str(),
                                        raw_score = format!("{:.1}", score.threat_score.unwrap_or(0.0)),
                                        "Follower above Watch threshold, running NLI"
                                    );
                                    AssertUnwindSafe(profile::build_profile(
                                        client,
                                        scorer,
                                        &follower.handle,
                                        &follower.did,
                                        protected_fingerprint,
                                        weights,
                                        embedder,
                                        protected_embedding,
                                        median_engagement,
                                        pile_on_dids,
                                        nli_ref,  // NLI enabled
                                        ppwe_ref, // Inferred pairs
                                        None,     // No direct pairs
                                    ))
                                    .catch_unwind()
                                    .await
                                    .unwrap_or(result) // Fall back to pass 1 on panic
                                }
                                other => other,
                            }
                        }
                    }))
                    .buffer_unordered(concurrency);
```

- [ ] **Step 4: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 6: Commit**

```bash
git add src/pipeline/amplification.rs tests/unit_nli.rs
git commit -m 'feat: two-pass follower scoring with threshold-gated NLI

Pass 1 scores without NLI (fast). If raw_score >= 8.0 (Watch),
pass 2 re-scores with NLI inferred pairs. Falls back to pass 1
if NLI panics. Keeps NLI compute proportional to actual threats.

Spec: nli-scoring-redesign §Pipeline Flow Phase C'
```

---

## Chunk 5: Profile Builder Formula Reorder + Audit Wiring

### Task 10: Reorder scoring formula in build_profile

**Files:**
- Modify: `src/scoring/profile.rs:233-249` (reorder: raw -> behavioral -> context_multiplier)

- [ ] **Step 1: Replace scoring block**

In `src/scoring/profile.rs`, replace the scoring block (lines 233-251) with:

```rust
    // Step 6: Apply scoring formula in spec order:
    //   1. raw_score = tox * 70 * (1 + overlap * 1.5)
    //   2. score_with_behavioral = raw_score * behavioral_boost (via gate)
    //   3. context_multiplier = 1.0 + (context_score * 0.5)
    //   4. final_score = score_with_behavioral * context_multiplier
    let (raw_score, _) = threat::compute_threat_score(avg_toxicity, topic_overlap, weights);

    let (score_with_behavioral, benign_gate) = behavioral::apply_behavioral_modifier_contextual(
        raw_score,
        quote_ratio,
        reply_ratio,
        pile_on,
        avg_engagement,
        median_engagement,
        context_score,
    );

    let context_multiplier = match context_score {
        Some(ctx) => 1.0 + (ctx * 0.5),
        None => 1.0,
    };
    let final_score = (score_with_behavioral * context_multiplier).clamp(0.0, 100.0);

    let tier = crate::db::models::ThreatTier::from_score(final_score);
```

Also update the tracing log line and AccountScore to use `final_score`:

```rust
        threat = format!("{:.1}", final_score),
```

```rust
        threat_score: Some(final_score),
```

- [ ] **Step 2: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 3: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean (compute_threat_score_contextual may get a dead_code warning — add `#[allow(dead_code)]` if so, since its inline tests still document the formula)

- [ ] **Step 4: Commit**

```bash
git add src/scoring/profile.rs
git commit -m 'feat: apply context_multiplier after behavioral boost

Scoring order: raw -> behavioral -> context_multiplier. Context
amplifies the already-modified score. The old blended formula in
compute_threat_score_contextual is retained for test documentation.

Spec: nli-scoring-redesign §Threat Score Formula'
```

---

### Task 11: Wire audit logging into NLI call sites

**Files:**
- Modify: `src/scoring/profile.rs` (emit audit entries from both direct and inferred NLI paths)
- Modify: `src/pipeline/amplification.rs` (emit audit entries from event-level NLI scoring)

- [ ] **Step 1: Add data_dir parameter to build_profile**

In `src/scoring/profile.rs`, add `data_dir: Option<&std::path::Path>` as the last parameter.

- [ ] **Step 2: Emit audit entries in direct pairs block**

In the direct pairs NLI scoring block of `src/scoring/profile.rs`, after `Ok((score, hypothesis_scores))`:

```rust
                        Ok((score, hypothesis_scores)) => {
                            pair_scores.push(score);
                            if let Some(dir) = data_dir {
                                crate::scoring::nli_audit::log_nli_audit(
                                    &crate::scoring::nli_audit::NliAuditEntry {
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                        target_did: target_did.to_string(),
                                        target_handle: target_handle.to_string(),
                                        pair_type: "direct".to_string(),
                                        original_text: original.to_string(),
                                        response_text: response.to_string(),
                                        hypothesis_scores,
                                        hostility_score: score,
                                        similarity: None,
                                    },
                                    Some(dir),
                                );
                            }
                        }
```

- [ ] **Step 3: Emit audit entries in inferred pairs block**

In the inferred pairs NLI scoring block, after `Ok((score, hypothesis_scores))`:

```rust
                                Ok((score, hypothesis_scores)) => {
                                    pair_scores.push(score);
                                    if let Some(dir) = data_dir {
                                        crate::scoring::nli_audit::log_nli_audit(
                                            &crate::scoring::nli_audit::NliAuditEntry {
                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                                target_did: target_did.to_string(),
                                                target_handle: target_handle.to_string(),
                                                pair_type: "inferred".to_string(),
                                                original_text: original.to_string(),
                                                response_text: target_text.to_string(),
                                                hypothesis_scores,
                                                hostility_score: score,
                                                similarity: Some(*similarity),
                                            },
                                            Some(dir),
                                        );
                                    }
                                }
```

- [ ] **Step 4: Add data_dir to amplification::run and event-level NLI**

Add `data_dir: Option<&std::path::Path>` to the `run` signature. In the event-level NLI scoring block (~line 111), update:

```rust
                    Ok((score, hypothesis_scores)) => {
                        info!(
                            handle = event.amplifier_handle,
                            context_score = format!("{:.3}", score),
                            "NLI scored event pair"
                        );
                        if let Some(dir) = data_dir {
                            crate::scoring::nli_audit::log_nli_audit(
                                &crate::scoring::nli_audit::NliAuditEntry {
                                    timestamp: chrono::Utc::now().to_rfc3339(),
                                    target_did: event.amplifier_did.clone(),
                                    target_handle: event.amplifier_handle.clone(),
                                    pair_type: "direct".to_string(),
                                    original_text: orig_text.to_string(),
                                    response_text: amp_text.to_string(),
                                    hypothesis_scores,
                                    hostility_score: score,
                                    similarity: None,
                                },
                                Some(dir),
                            );
                        }
                        Some(score)
                    }
```

- [ ] **Step 5: Add data_dir() method to Config**

In `src/config.rs`, add a method to `Config`:

```rust
    /// Data directory for audit logs and other persistent files.
    /// On Railway: /data (parent of model_dir=/data/models).
    /// Locally: falls back to model_dir itself.
    pub fn data_dir(&self) -> &std::path::Path {
        self.model_dir.parent().unwrap_or(&self.model_dir)
    }
```

- [ ] **Step 6: Update all call sites with data_dir**

Update callers:
- `src/web/scan_job.rs` `amplification::run` call: add `Some(config.data_dir())`
- `src/main.rs` `amplification::run` call: add `Some(config.data_dir())`
- All `build_profile` call sites (4 total): add `Some(config.data_dir())` where config is available, `None` where it isn't (e.g. inside closures — pass as a captured reference)
- `src/pipeline/amplification.rs` Phase B amplifier loop: forward `data_dir` from `run` parameter to `build_profile`
- `src/pipeline/amplification.rs` follower scoring closure: forward `data_dir` as captured reference
- `src/pipeline/sweep.rs`: add `data_dir` to `run` signature, forward to `build_profile`

- [ ] **Step 6: Compile and run tests**

Run: `cargo test --features web`
Expected: All tests PASS

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 8: Commit**

```bash
git add src/config.rs src/scoring/profile.rs src/pipeline/amplification.rs src/web/scan_job.rs src/main.rs src/pipeline/sweep.rs
git commit -m 'feat: wire NLI audit logging into all scoring paths

Every NLI score_pair call now emits structured tracing + JSONL audit
entry with full hypothesis breakdown, pair type, and similarity.
File written to {data_dir}/nli-audit.jsonl on the persistent volume.
Config.data_dir() returns model_dir parent (/data on Railway).

Spec: nli-scoring-redesign §NLI Audit Logging'
```

---

## Chunk 6: Final Verification + Staging Deploy

### Task 12: Full integration verification

- [ ] **Step 1: Run full test suite**

Run: `cargo test --features web`
Expected: All 307+ tests PASS (plus new tests added in this plan)

- [ ] **Step 2: Run clippy clean**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 3: Build release binary**

Run: `cargo build --release --features web`
Expected: Compiles successfully

- [ ] **Step 4: Merge to staging**

```bash
git checkout staging
git merge feat/nli-scoring-redesign
git push origin staging
```

- [ ] **Step 5: Verify Railway staging deployment**

Wait for Railway to deploy the staging branch. Then:
1. Log in at charcoal-web-staging.up.railway.app
2. Trigger a scan
3. Verify in Railway logs:
   - "NLI audit" entries appear with hypothesis breakdowns
   - Amplifiers get context_score values
   - Followers above Watch threshold show NLI pass 2 logs
4. Check review queue — context_score column should populate for scored accounts

- [ ] **Step 6: Check JSONL audit file**

Via Railway shell or logs, verify `/data/nli-audit.jsonl` contains properly formatted entries.
