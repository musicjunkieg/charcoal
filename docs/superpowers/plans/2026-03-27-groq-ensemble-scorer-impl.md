# Groq Ensemble Scorer Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the unreliable OpenAI Moderation secondary scorer with Groq GPT-OSS-Safeguard-20B, using a two-way correction strategy that dampens ONNX false positives and boosts missed hostility.

**Architecture:** Groq scorer calls the chat completions API with a custom moderation policy. The ensemble holds `GroqSafeguardScorer` directly (not via `Box<dyn ToxicityScorer>`) to preserve category and rationale data. Applies a correction matrix: dampen (0.4x) when ONNX flags high but Groq says safe, boost (1.5x-2.0x by category) when ONNX misses something Groq catches. Context-aware: sends text pairs for quotes/replies.

**Tech Stack:** Rust, reqwest (existing dep, no new Cargo.toml deps), Groq chat completions API (OpenAI-compatible), tokio semaphore for rate limiting

**Key design note:** The ensemble holds `Option<GroqSafeguardScorer>` (concrete type) instead of `Option<Box<dyn ToxicityScorer>>`. This is necessary because the correction matrix needs the full `SafeguardResult` (violation flag, category string, rationale) — not just a toxicity score. The `ToxicityScorer` trait collapses this to a single float, losing the category-dependent boost factors.

**Spec:** `docs/superpowers/specs/2026-03-27-groq-ensemble-scorer-design.md`

---

## File Structure

### New Files

| File | Responsibility |
|------|---------------|
| `src/toxicity/groq_safeguard.rs` | Groq GPT-OSS-Safeguard scorer with rate limiting and policy |

### Modified Files

| File | Changes |
|------|---------|
| `src/toxicity/mod.rs:10` | Add `pub mod groq_safeguard` alongside existing (Task 3), then remove `openai_moderation` (Task 5) |
| `src/toxicity/traits.rs:43-58` | Add `score_with_context` default method to ToxicityScorer trait |
| `src/toxicity/ensemble.rs` | Rewrite merge strategy: two-way correction replaces score comparison |
| `src/config.rs:39,95,187` | Replace `openai_api_key` with `groq_api_key` |
| `src/web/scan_job.rs:242-262` | Swap scorer construction to use GroqSafeguardScorer |
| `src/main.rs:929-941` | Swap scorer construction in CLI create_scorer() |
| `src/pipeline/amplification.rs:77-112` | Reorder code, call `score_with_context` with original post |
| `tests/unit_ensemble.rs` | Full rewrite: two-way correction tests replace score comparison |

### Deleted Files

| File | Reason |
|------|--------|
| `src/toxicity/openai_moderation.rs` | Replaced by groq_safeguard.rs |

---

## Chunk 1: Config, Trait, and Groq Scorer

### Task 1: Swap config from OpenAI to Groq

**Files:**
- Modify: `src/config.rs:39` (field), `src/config.rs:95` (loader), `src/config.rs:187` (test_defaults)

- [ ] **Step 1: Replace `openai_api_key` with `groq_api_key` in Config struct**

At line 39, change:
```rust
pub openai_api_key: Option<String>,
```
to:
```rust
pub groq_api_key: Option<String>,
```

At line 95, change:
```rust
openai_api_key: env::var("OPENAI_API_KEY").ok(),
```
to:
```rust
groq_api_key: env::var("GROQ_API_KEY").ok(),
```

At line 187 in test_defaults(), change:
```rust
openai_api_key: None,
```
to:
```rust
groq_api_key: None,
```

- [ ] **Step 2: Fix all references to `openai_api_key` in other files**

Search for `openai_api_key` across the codebase and update each reference to `groq_api_key`. Key locations:
- `src/web/scan_job.rs:244` — `config.openai_api_key.as_ref()`
- `src/main.rs:930` — `config.openai_api_key.as_ref()`

Do NOT update the scorer construction yet (that's Task 4) — just rename the field references.

- [ ] **Step 3: Run `cargo test --features web` to verify compilation**

Expected: Tests should compile and pass (the field rename is straightforward).

- [ ] **Step 4: Commit**

```bash
git add src/config.rs src/web/scan_job.rs src/main.rs
git commit -m 'refactor: rename openai_api_key to groq_api_key in config'
```

---

### Task 2: Add `score_with_context` to ToxicityScorer trait

**Files:**
- Modify: `src/toxicity/traits.rs:43-58`

- [ ] **Step 1: Add default method to ToxicityScorer trait**

In `src/toxicity/traits.rs`, after the `score_batch` method (line 57), add:

```rust
    /// Score text with optional context (e.g., the original post being
    /// replied to or quoted). Default implementation ignores context.
    /// The ensemble overrides this to pass context to the secondary
    /// scorer while giving the primary only the amplifier text.
    async fn score_with_context(
        &self,
        text: &str,
        _context: Option<&str>,
    ) -> Result<ToxicityResult> {
        self.score_text(text).await
    }
```

- [ ] **Step 2: Run `cargo test --features web`**

Expected: All tests pass (default method, no behavioral change).

- [ ] **Step 3: Commit**

```bash
git add src/toxicity/traits.rs
git commit -m 'feat: add score_with_context default method to ToxicityScorer trait'
```

---

### Task 3: Create Groq Safeguard scorer

**Files:**
- Create: `src/toxicity/groq_safeguard.rs`
- Modify: `src/toxicity/mod.rs:10`

- [ ] **Step 1: Write tests for Groq scorer response parsing**

Add to `tests/unit_ensemble.rs` (new module alongside existing tests — existing tests are rewritten in Task 5):

```rust
#[cfg(test)]
mod groq_parsing_tests {
    use charcoal::toxicity::groq_safeguard::{parse_safeguard_response, SafeguardResult};

    #[test]
    fn test_parse_violation() {
        let json = r#"{"violation": 1, "category": "Targeted harassment", "rationale": "Direct insult"}"#;
        let result = parse_safeguard_response(json).unwrap();
        assert!(result.violation);
        assert_eq!(result.category, "Targeted harassment");
        assert_eq!(result.rationale, "Direct insult");
    }

    #[test]
    fn test_parse_safe() {
        let json = r#"{"violation": 0, "category": "none", "rationale": "Substantive disagreement"}"#;
        let result = parse_safeguard_response(json).unwrap();
        assert!(!result.violation);
    }

    #[test]
    fn test_parse_malformed_json() {
        let result = parse_safeguard_response("not json at all");
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_missing_fields() {
        let json = r#"{"violation": 1}"#;
        let result = parse_safeguard_response(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_truncated() {
        let json = r#"{"violation": 1, "categ"#;
        let result = parse_safeguard_response(json);
        assert!(result.is_none());
    }

    #[test]
    fn test_category_boost_identity() {
        assert_eq!(charcoal::toxicity::groq_safeguard::boost_for_category("Identity-based hostility"), 2.0);
    }

    #[test]
    fn test_category_boost_bad_faith() {
        assert_eq!(charcoal::toxicity::groq_safeguard::boost_for_category("Bad-faith engagement"), 1.5);
    }

    #[test]
    fn test_category_boost_unknown() {
        assert_eq!(charcoal::toxicity::groq_safeguard::boost_for_category("Something else"), 1.5);
    }
}
```

- [ ] **Step 2: Create `src/toxicity/groq_safeguard.rs`**

Full implementation:

```rust
//! Groq GPT-OSS-Safeguard scorer — policy-based content moderation.
//!
//! Calls Groq's chat completions API with a custom moderation policy
//! and returns structured violation assessments. Used as the secondary
//! scorer in the ensemble for two-way correction of ONNX scores.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

const ENDPOINT: &str = "https://api.groq.com/openai/v1/chat/completions";
const MODEL: &str = "openai/gpt-oss-safeguard-20b";

const MAX_RETRIES: u32 = 5;
const INITIAL_BACKOFF_MS: u64 = 2000;
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(2000);

/// Moderation policy sent as the system prompt.
const POLICY: &str = r#"# Charcoal Content Moderation Policy

## INSTRUCTIONS
Classify the following social media post for hostile or bad-faith content directed at or about other people. Return JSON with fields: violation (1 or 0), category (string), and rationale (string).

## DEFINITIONS
- "Hostile content" means language intended to harm, demean, intimidate, or silence a specific person or group.
- "Bad-faith engagement" means responses that attack the person rather than engaging with their ideas.

## VIOLATES (1)
- Identity-based hostility: slurs, dehumanization, hate speech targeting identity (race, gender, sexuality, body size, disability)
- Targeted harassment: direct insults, mockery, name-calling aimed at a specific person
- Threat language: explicit or implied threats of harm, doxxing, or encouraging others to harm someone
- Pile-on instigation: language designed to direct an audience at a target ("ratio this", "look at this idiot", quote-dunking)
- Bad-faith engagement: ad hominems, strawmanning, dismissive non-engagement ("you're just...", "people like you...")
- Category errors as attacks: reducing someone's argument to an identity trait ("you only think that because you're...")

## SAFE (0)
- Strong opinions or passionate political language without personal attacks
- Profanity not directed at a person
- Discussion of sensitive topics (body politics, identity, race) in supportive or analytical context
- Disagreement that engages with the substance of an argument
- Satire or humor not targeting a specific person

## RESPONSE FORMAT
Return a JSON object with exactly these fields:
{"violation": 1, "category": "Targeted harassment", "rationale": "..."}
{"violation": 0, "category": "none", "rationale": "..."}"#;

/// Parsed response from GPT-OSS-Safeguard.
#[derive(Debug, Deserialize)]
pub struct SafeguardResult {
    pub violation: bool,
    pub category: String,
    pub rationale: String,
}

// Serde helper: deserialize 0/1 as bool
mod violation_bool {
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<bool, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ViolationValue {
            Int(i64),
            Bool(bool),
        }
        match ViolationValue::deserialize(deserializer)? {
            ViolationValue::Int(n) => Ok(n != 0),
            ViolationValue::Bool(b) => Ok(b),
        }
    }
}

/// Raw serde struct for the Groq response.
#[derive(Debug, Deserialize)]
struct SafeguardResponseRaw {
    #[serde(deserialize_with = "violation_bool::deserialize")]
    violation: bool,
    category: String,
    rationale: String,
}

/// Parse the JSON content from a Groq safeguard response.
/// Returns None on any parse failure (fail safe — don't inflate scores).
pub fn parse_safeguard_response(content: &str) -> Option<SafeguardResult> {
    let raw: SafeguardResponseRaw = serde_json::from_str(content).ok()?;
    Some(SafeguardResult {
        violation: raw.violation,
        category: raw.category,
        rationale: raw.rationale,
    })
}

/// Get the boost factor for a Groq violation category.
pub fn boost_for_category(category: &str) -> f64 {
    match category {
        "Identity-based hostility" => 2.0,
        "Targeted harassment" => 2.0,
        "Threat language" => 2.0,
        "Pile-on instigation" => 1.75,
        "Bad-faith engagement" => 1.5,
        "Category errors as attacks" => 1.5,
        _ => 1.5, // Unknown categories get conservative boost
    }
}

/// Map a Groq category to ToxicityAttributes.
fn attributes_for_category(category: &str) -> ToxicityAttributes {
    match category {
        "Identity-based hostility" => ToxicityAttributes {
            identity_attack: Some(1.0),
            ..Default::default()
        },
        "Threat language" => ToxicityAttributes {
            threat: Some(1.0),
            ..Default::default()
        },
        _ => ToxicityAttributes {
            insult: Some(1.0),
            ..Default::default()
        },
    }
}

/// Groq chat completion response structures.
#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
    #[allow(dead_code)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

/// Groq GPT-OSS-Safeguard scorer.
pub struct GroqSafeguardScorer {
    client: reqwest::Client,
    api_key: String,
    semaphore: Arc<Semaphore>,
    last_request: Arc<Mutex<Instant>>,
}

impl GroqSafeguardScorer {
    pub fn new(api_key: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("charcoal/0.1 (threat-detection; @chaosgreml.in)")
            .build()
            .context("Failed to build HTTP client for Groq")?;

        Ok(Self {
            client,
            api_key: api_key.to_string(),
            semaphore: Arc::new(Semaphore::new(1)),
            last_request: Arc::new(Mutex::new(Instant::now() - MIN_REQUEST_INTERVAL)),
        })
    }

    /// Send a chat completion request with rate limiting and retry on 429.
    async fn send_with_retry(&self, user_content: &str) -> Result<Option<SafeguardResult>> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("Semaphore closed: {e}"))?;

        let body = serde_json::json!({
            "model": MODEL,
            "messages": [
                { "role": "system", "content": POLICY },
                { "role": "user", "content": user_content }
            ],
            "response_format": { "type": "json_object" }
        });

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        for attempt in 0..=MAX_RETRIES {
            // Enforce minimum interval
            {
                let mut last = self.last_request.lock().await;
                let elapsed = last.elapsed();
                if elapsed < MIN_REQUEST_INTERVAL {
                    tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
                }
                *last = Instant::now();
            }

            let response = self
                .client
                .post(ENDPOINT)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(&body)
                .send()
                .await
                .context("Groq API request failed")?;

            let status = response.status();

            if status.as_u16() == 429 {
                if attempt < MAX_RETRIES {
                    warn!(
                        attempt = attempt + 1,
                        backoff_ms, "Groq rate limited (429), retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= 2;
                    continue;
                }
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Groq API rate limited after {MAX_RETRIES} retries: {error_body}");
            }

            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("Groq API error {status}: {error_body}");
            }

            let chat_response: ChatResponse = response
                .json()
                .await
                .context("Failed to parse Groq chat response")?;

            let content = chat_response
                .choices
                .first()
                .and_then(|c| c.message.content.as_deref());

            return Ok(content.and_then(parse_safeguard_response));
        }

        anyhow::bail!("Groq API: exhausted retries")
    }

    /// Format the user message, optionally including original post context.
    fn format_user_message(text: &str, context: Option<&str>) -> String {
        match context {
            Some(original) => format!(
                "Original post by protected user:\n\"{original}\"\n\nResponse being evaluated:\n\"{text}\""
            ),
            None => format!("Post being evaluated:\n\"{text}\""),
        }
    }

    /// Score text with optional context, returning the SafeguardResult.
    pub async fn score_with_safeguard(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<Option<SafeguardResult>> {
        let user_message = Self::format_user_message(text, context);
        self.send_with_retry(&user_message).await
    }
}

/// Convert a SafeguardResult to a ToxicityResult for trait compatibility.
fn safeguard_to_toxicity(result: &Option<SafeguardResult>) -> ToxicityResult {
    match result {
        Some(sr) if sr.violation => {
            let attrs = attributes_for_category(&sr.category);
            ToxicityResult {
                toxicity: 1.0,
                attributes: attrs,
            }
        }
        _ => ToxicityResult {
            toxicity: 0.0,
            attributes: ToxicityAttributes::default(),
        },
    }
}

#[async_trait]
impl ToxicityScorer for GroqSafeguardScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let result = self.score_with_safeguard(text, None).await?;
        Ok(safeguard_to_toxicity(&result))
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        let result = self.score_with_safeguard(text, context).await?;
        Ok(safeguard_to_toxicity(&result))
    }
}
```

- [ ] **Step 3: Add module declaration (keep both temporarily)**

In `src/toxicity/mod.rs`, add alongside existing:
```rust
pub mod groq_safeguard;
```
Do NOT remove `pub mod openai_moderation` yet — that happens in Task 5.

- [ ] **Step 4: Run `cargo test --features web`**

Expected: Compilation succeeds, new parsing tests pass, existing ensemble tests may fail (they reference `DisagreementStrategy` which still exists — that's fine for now).

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/groq_safeguard.rs src/toxicity/mod.rs tests/unit_ensemble.rs
git commit -m 'feat: add Groq GPT-OSS-Safeguard scorer with moderation policy'
```

---

## Chunk 2: Ensemble Rewrite and Pipeline Integration

### Task 4: Rewrite ensemble scorer with two-way correction

**Files:**
- Modify: `src/toxicity/ensemble.rs` (full rewrite of merge logic)

- [ ] **Step 1: Rewrite `tests/unit_ensemble.rs` with two-way correction tests**

Replace the existing score-comparison tests (all 10) with:

```rust
//! Tests for ensemble two-way correction (Groq + ONNX).

use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};
use anyhow::Result;
use async_trait::async_trait;

/// Mock scorer that returns a fixed toxicity score.
struct FixedScorer(f64);

#[async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: self.0,
            attributes: ToxicityAttributes::default(),
        })
    }
}

/// Mock scorer that always fails.
struct FailingScorer;

#[async_trait]
impl ToxicityScorer for FailingScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        anyhow::bail!("Mock failure")
    }
}

/// Mock Groq scorer: violation=true with given category.
struct MockGroqViolation(String);

#[async_trait]
impl ToxicityScorer for MockGroqViolation {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: 1.0,
            attributes: ToxicityAttributes {
                insult: Some(1.0),
                ..Default::default()
            },
        })
    }
}

/// Mock Groq scorer: violation=false (safe).
struct MockGroqSafe;

#[async_trait]
impl ToxicityScorer for MockGroqSafe {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: 0.0,
            attributes: ToxicityAttributes::default(),
        })
    }
}

mod ensemble_tests {
    use super::*;
    use charcoal::toxicity::ensemble::EnsembleToxicityScorer;

    #[tokio::test]
    async fn agree_both_high_and_violation() {
        // ONNX high + Groq violation = keep ONNX score
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.30)),
            Some(Box::new(MockGroqViolation("Targeted harassment".into()))),
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.30).abs() < 0.001);
    }

    #[tokio::test]
    async fn dampen_onnx_false_positive() {
        // ONNX high + Groq safe = dampen to 0.4x
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.25)),
            Some(Box::new(MockGroqSafe)),
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.10).abs() < 0.001); // 0.25 * 0.4 = 0.10
    }

    #[tokio::test]
    async fn boost_onnx_missed_hostility() {
        // ONNX low + Groq violation = boost with floor
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.05)),
            Some(Box::new(MockGroqViolation("Targeted harassment".into()))),
        );
        let result = ensemble.score_text("test").await.unwrap();
        // 0.05 * 2.0 = 0.10, but floor is 0.15
        assert!((result.toxicity - 0.15).abs() < 0.001);
    }

    #[tokio::test]
    async fn agree_both_low_and_safe() {
        // ONNX low + Groq safe = keep ONNX score
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.05)),
            Some(Box::new(MockGroqSafe)),
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.05).abs() < 0.001);
    }

    #[tokio::test]
    async fn boost_caps_at_one() {
        // Boosted score should cap at 1.0
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.10)),
            Some(Box::new(MockGroqViolation("Identity-based hostility".into()))),
        );
        let result = ensemble.score_text("test").await.unwrap();
        // 0.10 * 2.0 = 0.20, above floor, below cap
        assert!((result.toxicity - 0.20).abs() < 0.001);
    }

    #[tokio::test]
    async fn secondary_failure_uses_primary() {
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.20)),
            Some(Box::new(FailingScorer)),
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.20).abs() < 0.001);
    }

    #[tokio::test]
    async fn no_secondary_uses_primary() {
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.20)),
            None,
        );
        let result = ensemble.score_text("test").await.unwrap();
        assert!((result.toxicity - 0.20).abs() < 0.001);
    }

    #[tokio::test]
    async fn score_with_context_delegates() {
        let ensemble = EnsembleToxicityScorer::new(
            Box::new(FixedScorer(0.25)),
            Some(Box::new(MockGroqSafe)),
        );
        // score_with_context should dampen just like score_text
        let result = ensemble.score_with_context("test", Some("original post")).await.unwrap();
        assert!((result.toxicity - 0.10).abs() < 0.001);
    }
}
```

- [ ] **Step 2: Rewrite `src/toxicity/ensemble.rs`**

Replace the entire file:

```rust
//! Ensemble toxicity scorer — ONNX primary + Groq secondary with two-way correction.
//!
//! Correction matrix:
//! - ONNX high + Groq violation = agree (keep ONNX score)
//! - ONNX high + Groq safe = dampen (0.4x — ONNX false positive)
//! - ONNX low + Groq violation = boost (category-dependent + floor)
//! - ONNX low + Groq safe = agree (keep ONNX score)

use anyhow::Result;
use async_trait::async_trait;
use tracing::{debug, warn};

use super::groq_safeguard::boost_for_category;
use super::traits::{ToxicityResult, ToxicityScorer};

/// ONNX toxicity threshold: above this is "high", at or below is "low".
const ONNX_HIGH_THRESHOLD: f64 = 0.15;

/// Dampening factor for ONNX false positives (Groq says safe).
const DAMPEN_FACTOR: f64 = 0.4;

/// Minimum toxicity floor when Groq flags a violation.
const GROQ_FLOOR: f64 = 0.15;

/// Result from the ensemble scorer with correction metadata.
pub struct EnsembleResult {
    /// The final toxicity result (ONNX, potentially corrected)
    pub result: ToxicityResult,
    /// Whether Groq flagged a violation
    pub groq_flagged: bool,
    /// Groq violation category (if flagged)
    pub groq_category: Option<String>,
    /// Groq rationale (if flagged)
    pub groq_rationale: Option<String>,
    /// The correction factor applied (0.4 for dampen, >1.0 for boost, 1.0 for agree)
    pub correction_applied: f64,
    /// Whether ONNX and Groq agreed
    pub models_agree: bool,
}

/// Ensemble scorer: ONNX primary + optional Groq secondary (concrete type).
///
/// Holds GroqSafeguardScorer directly (not Box<dyn ToxicityScorer>) so we
/// can access the full SafeguardResult with category and rationale — these
/// are needed for category-dependent boost factors and observability.
pub struct EnsembleToxicityScorer {
    primary: Box<dyn ToxicityScorer>,
    secondary: Option<GroqSafeguardScorer>,
}

impl EnsembleToxicityScorer {
    pub fn new(
        primary: Box<dyn ToxicityScorer>,
        secondary: Option<GroqSafeguardScorer>,
    ) -> Self {
        Self { primary, secondary }
    }

    /// Score text with optional context. Returns full ensemble metadata.
    pub async fn score_ensemble_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<EnsembleResult> {
        let (primary_result, safeguard_result) = match &self.secondary {
            Some(groq) => {
                // Run ONNX and Groq concurrently
                let (primary, groq_result) = tokio::join!(
                    self.primary.score_text(text),
                    groq.score_with_safeguard(text, context),
                );
                let safeguard = match groq_result {
                    Ok(result) => result, // Option<SafeguardResult>
                    Err(e) => {
                        warn!(error = %e, "Groq scorer failed, using ONNX only");
                        None
                    }
                };
                (primary?, safeguard)
            }
            None => (self.primary.score_text(text).await?, None),
        };

        let onnx_tox = primary_result.toxicity;

        match &safeguard_result {
            Some(sr) => {
                let groq_flagged = sr.violation;
                let onnx_high = onnx_tox > ONNX_HIGH_THRESHOLD;

                let (correction, models_agree) = match (onnx_high, groq_flagged) {
                    (true, true) => {
                        debug!(onnx_tox, category = %sr.category, "Ensemble: agree (both hostile)");
                        (1.0, true)
                    }
                    (true, false) => {
                        debug!(onnx_tox, dampened = onnx_tox * DAMPEN_FACTOR, "Ensemble: dampening ONNX false positive");
                        (DAMPEN_FACTOR, false)
                    }
                    (false, true) => {
                        let boost = boost_for_category(&sr.category);
                        debug!(onnx_tox, boost, category = %sr.category, "Ensemble: boosting missed hostility");
                        (boost, false)
                    }
                    (false, false) => {
                        debug!(onnx_tox, "Ensemble: agree (both safe)");
                        (1.0, true)
                    }
                };

                let corrected_tox = if groq_flagged && !onnx_high {
                    (onnx_tox * correction).max(GROQ_FLOOR).min(1.0)
                } else {
                    (onnx_tox * correction).min(1.0)
                };

                let mut result = primary_result;
                result.toxicity = corrected_tox;

                Ok(EnsembleResult {
                    result,
                    groq_flagged,
                    groq_category: Some(sr.category.clone()),
                    groq_rationale: Some(sr.rationale.clone()),
                    correction_applied: correction,
                    models_agree,
                })
            }
            None => {
                Ok(EnsembleResult {
                    result: primary_result,
                    groq_flagged: false,
                    groq_category: None,
                    groq_rationale: None,
                    correction_applied: 1.0,
                    models_agree: true,
                })
            }
        }
    }
}

#[async_trait]
impl ToxicityScorer for EnsembleToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let r = self.score_ensemble_with_context(text, None).await?;
        Ok(r.result)
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        let r = self.score_ensemble_with_context(text, context).await?;
        Ok(r.result)
    }
}
```

- [ ] **Step 3: Run `cargo test --features web`**

Expected: All tests pass with the new ensemble logic.

- [ ] **Step 4: Commit**

```bash
git add src/toxicity/ensemble.rs tests/unit_ensemble.rs
git commit -m 'feat: rewrite ensemble with two-way correction — dampen false positives, boost missed hostility'
```

---

### Task 5: Wire Groq scorer into scan pipeline and CLI

**Files:**
- Modify: `src/web/scan_job.rs:242-262`
- Modify: `src/main.rs:929-941`
- Delete: `src/toxicity/openai_moderation.rs`

- [ ] **Step 1: Update scorer construction in scan_job.rs**

At lines 242-262, replace the OpenAI scorer construction with Groq:

```rust
    let secondary_scorer: Option<crate::toxicity::groq_safeguard::GroqSafeguardScorer> =
        config.groq_api_key.as_ref().and_then(|key| {
            match crate::toxicity::groq_safeguard::GroqSafeguardScorer::new(key) {
                Ok(s) => {
                    info!("Groq Safeguard scorer loaded — ensemble scoring enabled");
                    Some(s)
                }
                Err(e) => {
                    warn!(error = %e, "Failed to init Groq scorer, using ONNX only");
                    None
                }
            }
        });

    let scorer: Box<dyn ToxicityScorer> =
        Box::new(crate::toxicity::ensemble::EnsembleToxicityScorer::new(
            primary_scorer,
            secondary_scorer,
        ));
```

Note: remove the `DisagreementStrategy::TakeLower` argument from `EnsembleToxicityScorer::new`.

- [ ] **Step 2: Update scorer construction in main.rs**

At lines 929-941, same change — replace OpenAI with Groq and drop the strategy argument.

- [ ] **Step 3: Delete `src/toxicity/openai_moderation.rs`**

```bash
rm src/toxicity/openai_moderation.rs
```

- [ ] **Step 4: Run `cargo test --features web`**

Expected: All tests pass, no references to OpenAI remain.

- [ ] **Step 5: Verify no dangling references**

Run: `grep -rn 'openai_moderation\|OpenAiModeration\|OPENAI_API_KEY\|openai_api_key' src/`
Expected: No matches (or only in comments/docs).

- [ ] **Step 6: Commit**

```bash
git rm src/toxicity/openai_moderation.rs
git add src/toxicity/mod.rs src/web/scan_job.rs src/main.rs
git commit -m 'feat: wire Groq scorer into pipeline, remove OpenAI Moderation'
```

---

### Task 6: Add context to amplification pipeline scoring

**Files:**
- Modify: `src/pipeline/amplification.rs:77-112`

- [ ] **Step 1: Reorder code to compute original_post_text before scoring**

The current code at lines 77-112 calls `scorer.score_text` (line 85) before
`original_post_text` is computed (line 108). Reorder so that
`original_post_text` is resolved first, then pass it to `score_with_context`.

The reordered flow:
1. Look up `original_post_text` from the cache (currently lines 107-112)
2. For quote/reply events, fetch amplifier text (currently lines 82-93)
3. Score with context: `scorer.score_with_context(&text, original_post_text).await`

- [ ] **Step 2: Run `cargo test --features web`**

Expected: All tests pass.

- [ ] **Step 3: Run `cargo clippy --features web -- -D warnings`**

Expected: Clean.

- [ ] **Step 4: Commit**

```bash
git add src/pipeline/amplification.rs
git commit -m 'feat: pass original post context to scorer for contextual moderation'
```

---

### Task 7: Final verification and push

- [ ] **Step 1: Run full test suite**

```bash
cargo test --features web
```

- [ ] **Step 2: Run clippy**

```bash
cargo clippy --features web -- -D warnings
```

- [ ] **Step 3: Push and create PR**

```bash
git push origin feat/groq-ensemble
```

Create PR: `feat/groq-ensemble` -> `staging`
Title: "Replace OpenAI Moderation with Groq Safeguard ensemble scorer"
