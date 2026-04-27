# Groq Ensemble Scorer Design Spec

**Date**: 2026-03-27
**Status**: Approved
**Issue**: #154

## Goal

Replace the unreliable OpenAI Moderation secondary scorer with Groq
GPT-OSS-Safeguard-20B. The new scorer provides policy-based content
moderation with structured violation assessments, giving the ensemble
a qualitatively different second opinion rather than a second numerical
score.

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Secondary scorer | Groq GPT-OSS-Safeguard-20B | 30 RPM free tier, policy-based moderation, reliable API |
| HTTP client | reqwest (direct) | Consistent with existing patterns (Constellation, old OpenAI scorer), no new dependencies |
| Merge strategy | Two-way correction | Groq dampens ONNX false positives (0.4x) and boosts missed hostility. Addresses the primary ONNX problem: over-flagging benign content |
| Correction factors | Dampen 0.4x / Boost by category | Dampen when ONNX high + Groq safe. Boost (1.5x-2.0x by category) when ONNX low + Groq violation |
| Text input | Pairs when available | Groq gets original post + amplifier response for context; ONNX gets amplifier text only |
| OpenAI scorer | Remove entirely | Persistent rate limit issues, code preserved in git history |

## 1. Groq Safeguard Scorer

### New file: `src/toxicity/groq_safeguard.rs`

**Config**: `GROQ_API_KEY` env var. When set, enables the Groq secondary
scorer. When absent, ONNX-only (no ensemble).

**Struct**:
```rust
pub struct GroqSafeguardScorer {
    client: reqwest::Client,
    api_key: String,
    semaphore: Arc<Semaphore>,
    last_request: Arc<Mutex<Instant>>,
}
```

Rate limiting: `Semaphore(1)` ensures one request at a time,
`Mutex<Instant>` enforces 2000ms (2s) minimum interval between requests
to stay within Groq's 30 RPM free tier limit. Exponential backoff retry
on 429 responses (up to 5 retries, starting at 2s).

**API call**: `POST https://api.groq.com/openai/v1/chat/completions`
```json
{
  "model": "openai/gpt-oss-safeguard-20b",
  "messages": [
    { "role": "system", "content": "<MODERATION_POLICY>" },
    { "role": "user", "content": "<text to evaluate>" }
  ],
  "response_format": { "type": "json_object" }
}
```

**Response parsing**: The model returns JSON in the assistant message
content:
```json
{
  "violation": 1,
  "category": "Targeted harassment",
  "rationale": "The post directly insults and mocks the user..."
}
```

Parse the content string as JSON. Extract `violation` (0 or 1),
`category` (string), and `rationale` (string).

**Error handling** (all fail safe — treat as non-violation):
- JSON parse failure (malformed response)
- `finish_reason: "length"` (truncated response — JSON may be cut off)
- `finish_reason: "content_filter"` (blocked by Groq's own safety)
- Empty `choices` array
- Missing `message.content` field
- Missing or unexpected field types in the parsed JSON

**ToxicityScorer trait implementation**: `score_text` sends only the
amplifier text. Returns a `ToxicityResult` where:
- `violation == 0`: toxicity = 0.0, all attributes None
- `violation == 1`: toxicity = 1.0, relevant attribute set to 1.0 based
  on category mapping (identity_attack for identity hostility, insult
  for harassment/bad-faith, threat for threats)

This preserves the `ToxicityScorer` trait contract. The ensemble merges
the results using the boost-on-flag strategy.

## 2. Moderation Policy

Static const in `groq_safeguard.rs`, compiled into the binary.
Structured per Groq docs (400-600 tokens):

```text
# Charcoal Content Moderation Policy

## INSTRUCTIONS
Classify the following social media post for hostile or bad-faith content
directed at or about other people. Return JSON with fields: violation
(1 or 0), category (string), and rationale (string).

## DEFINITIONS
- "Hostile content" means language intended to harm, demean, intimidate,
  or silence a specific person or group.
- "Bad-faith engagement" means responses that attack the person rather
  than engaging with their ideas.

## VIOLATES (1)
- Identity-based hostility: slurs, dehumanization, hate speech targeting
  identity (race, gender, sexuality, body size, disability)
- Targeted harassment: direct insults, mockery, name-calling aimed at a
  specific person
- Threat language: explicit or implied threats of harm, doxxing, or
  encouraging others to harm someone
- Pile-on instigation: language designed to direct an audience at a
  target ("ratio this", "look at this idiot", quote-dunking)
- Bad-faith engagement: ad hominems, strawmanning, dismissive
  non-engagement ("you're just...", "people like you...")
- Category errors as attacks: reducing someone's argument to an identity
  trait ("you only think that because you're...")

## SAFE (0)
- Strong opinions or passionate political language without personal attacks
- Profanity not directed at a person
- Discussion of sensitive topics (body politics, identity, race) in
  supportive or analytical context
- Disagreement that engages with the substance of an argument
- Satire or humor not targeting a specific person

## RESPONSE FORMAT
Return a JSON object with exactly these fields:
{"violation": 1, "category": "Targeted harassment", "rationale": "..."}
{"violation": 0, "category": "none", "rationale": "..."}
```

## 3. Ensemble Merge Strategy: Two-Way Correction

### Replaces: score comparison + DisagreementStrategy

The ensemble scorer changes from comparing two numerical scores to
ONNX-score + Groq-second-opinion. The primary problem with ONNX is
**false positives** — it flags passionate political language, identity
discussion, and news commentary with violent keywords as toxic. Groq's
policy-based approach provides contextual correction in both directions.

### Correction Matrix

| ONNX Score | Groq Result | Action | Rationale |
|------------|-------------|--------|-----------|
| High (>0.15) | Violation | **Agree** — keep ONNX score | Both models confirm hostility |
| High (>0.15) | Safe | **Dampen** — multiply by 0.4x | ONNX false positive — Groq says context is benign |
| Low (<=0.15) | Violation | **Boost** — apply category boost + floor | ONNX missed coded hostility |
| Low (<=0.15) | Safe | **Agree** — keep ONNX score | Both models confirm benign |

### Dampening (the primary value-add)

When ONNX scores high but Groq says safe, apply `DAMPEN_FACTOR = 0.4`:
```
final_tox = onnx_tox * 0.4
```

This directly addresses issue #114 (toxicity false positives on news
commentary with violent keywords). An account posting about "fighting
for fat liberation" might get ONNX tox = 0.25 due to the aggressive
framing, but Groq recognizes it as supportive advocacy and dampens
the score to 0.10.

### Boosting (secondary value-add)

When ONNX scores low but Groq flags a violation, apply a
category-dependent boost with a minimum floor:

| Groq Category | Boost | Maps to Attribute |
|--------------|-------|-------------------|
| Identity-based hostility | 2.0x | identity_attack |
| Targeted harassment | 2.0x | insult |
| Threat language | 2.0x | threat |
| Pile-on instigation | 1.75x | insult |
| Bad-faith engagement | 1.5x | insult |
| Category errors as attacks | 1.5x | insult |
| Unknown/other | 1.5x | (none) |

Boost formula: `final_tox = min(max(onnx_tox * boost, GROQ_FLOOR), 1.0)`

`GROQ_FLOOR = 0.15` ensures Groq-flagged text has meaningful impact
even when ONNX returns a near-zero score (concern trolls, coded language).

### Where correction applies

The correction modifies the raw `tox` value **before** it enters the
scoring formula: `tox * 70 * (1 + overlap * 1.5) * behavioral * context * graph_distance`.
This is the same position where ONNX produces the score — the rest of
the chain applies on top as before.

### EnsembleResult changes

```rust
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
    /// Whether ONNX and Groq agreed (both high+violation, or both low+safe)
    pub models_agree: bool,
}
```

### Constructor change

`EnsembleToxicityScorer::new` drops the strategy parameter:
```rust
pub fn new(
    primary: Box<dyn ToxicityScorer>,
    secondary: Option<Box<dyn ToxicityScorer>>,
) -> Self
```

Update call sites in `src/main.rs` and `src/web/scan_job.rs`.

### Removed

- `DisagreementStrategy` enum
- `merge_results`, `merge_attributes`, `merge_option` functions
- `AGREEMENT_THRESHOLD` constant
- `secondary_score` and `score_difference` fields on `EnsembleResult`

## 4. Context-Aware Scoring

The ensemble needs to pass different inputs to ONNX vs Groq:
- ONNX: amplifier text only (token-level toxicity — shouldn't score
  the protected user's words)
- Groq: formatted pair (original post + amplifier response) when
  available, for contextual policy evaluation

### New default method on ToxicityScorer trait

Add `score_with_context` as a default method on the `ToxicityScorer`
trait in `src/toxicity/traits.rs`. This preserves the `&dyn ToxicityScorer`
signature everywhere and avoids leaking the ensemble's concrete type
into the pipeline.

```rust
/// Score text with optional context (e.g., the original post being
/// replied to or quoted). Default implementation ignores context and
/// delegates to score_text. The ensemble overrides this to pass
/// context to Groq while giving ONNX only the primary text.
async fn score_with_context(
    &self,
    text: &str,
    _context: Option<&str>,
) -> Result<ToxicityResult> {
    self.score_text(text).await
}
```

The `EnsembleToxicityScorer` overrides this to:
1. Pass `text` to ONNX via `primary.score_text(text)`
2. Format the pair for Groq when `context` is provided:
   ```
   Original post by protected user:
   "{context}"

   Response being evaluated:
   "{text}"
   ```
3. When `context` is None, send only the amplifier text to Groq

### Pipeline integration

In `src/pipeline/amplification.rs`, the scorer call site (line 85)
currently calls `scorer.score_text(&text)` before `original_post_text`
is computed (line 108). **The code must be reordered**: compute
`original_post_text` first, then call the scorer with context.

Change the call from:
```rust
scorer.score_text(&text).await
```
to:
```rust
scorer.score_with_context(&text, original_post_text.as_deref()).await
```

Since `score_with_context` is on the trait, the pipeline continues to
receive `&dyn ToxicityScorer` — no type change needed.

**Note**: `score_batch` in `src/scoring/profile.rs` is intentionally
unchanged. Follower post scoring does not have text pairs and continues
using the default sequential `score_batch` implementation. The
`GroqSafeguardScorer` does not override `score_batch` since Groq's chat
completions API is single-request-per-call.

## 5. Config Changes

### Remove

- `openai_api_key` field from `Config` struct
- `OPENAI_API_KEY` env var loading
- References to OpenAI in `test_defaults()`

### Add

- `groq_api_key: Option<String>` field on `Config`
- Loaded from `GROQ_API_KEY` env var
- Added to `test_defaults()` as `None`

### Scorer construction (scan_job.rs + main.rs)

```rust
let secondary_scorer: Option<Box<dyn ToxicityScorer>> =
    config.groq_api_key.as_ref().and_then(|key| {
        match GroqSafeguardScorer::new(key) {
            Ok(s) => {
                info!("Groq Safeguard scorer loaded — ensemble scoring enabled");
                Some(Box::new(s) as Box<dyn ToxicityScorer>)
            }
            Err(e) => {
                warn!(error = %e, "Failed to init Groq scorer, using ONNX only");
                None
            }
        }
    });
```

## 6. File Changes

| Action | File | What |
|--------|------|------|
| Delete | `src/toxicity/openai_moderation.rs` | Remove OpenAI Moderation scorer |
| Create | `src/toxicity/groq_safeguard.rs` | New Groq Safeguard scorer |
| Modify | `src/toxicity/ensemble.rs` | Boost-on-flag merge, remove score comparison |
| Modify | `src/toxicity/mod.rs` | Swap module declaration |
| Modify | `src/toxicity/traits.rs` | Add `score_with_context` default method |
| Modify | `src/config.rs` | Swap `openai_api_key` for `groq_api_key` |
| Modify | `src/web/scan_job.rs` | Swap scorer construction |
| Modify | `src/main.rs` | Swap scorer construction for CLI |
| Modify | `src/pipeline/amplification.rs` | Call `score_with_context` with original post |
| Modify | `tests/unit_ensemble.rs` | New tests for boost-on-flag |

## 7. Testing Strategy

### Unit tests

`tests/unit_ensemble.rs` is a full rewrite — the existing score-comparison
tests are deleted and replaced with boost-on-flag tests.

- `GroqSafeguardScorer` response parsing: violation=1, violation=0,
  malformed JSON, missing fields, truncated responses
- Category-to-boost mapping: each category returns correct factor
- **Dampening**: ONNX high (0.25) + Groq safe = dampened (0.10)
- **Boosting**: ONNX low (0.05) + Groq violation = boosted + floor (0.15)
- **Agreement (high)**: ONNX high + Groq violation = unchanged
- **Agreement (low)**: ONNX low + Groq safe = unchanged
- Ensemble with Groq failure: falls back to ONNX-only (correction = 1.0)
- Floor enforcement: boosted score never below GROQ_FLOOR when flagged
- Cap enforcement: corrected score never above 1.0
- Context formatting: pair format when context provided, single when not

### Integration

- Full scoring pipeline with mock Groq responses
- Verify boost flows through to final threat score

## 8. Railway Environment

### Staging

Set `GROQ_API_KEY` on staging service. Remove `OPENAI_API_KEY`.

### Production

Set `GROQ_API_KEY` when promoting to main. Remove `OPENAI_API_KEY`.
