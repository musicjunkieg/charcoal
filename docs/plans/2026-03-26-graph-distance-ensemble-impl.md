# Phase 2: Graph Distance Scoring + Ensemble Toxicity Scorer

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add social graph distance as a scoring input (strangers get amplified, mutual follows get dampened) and add OpenAI Moderation API as a concurrent toxicity signal alongside the existing ONNX model, with classifier agreement detection.

**Architecture:** `getRelationships` API classifies reply authors into 4 graph distance categories. `EnsembleToxicityScorer` wraps the existing ONNX scorer + OpenAI Moderation behind the same `ToxicityScorer` trait. Both features degrade gracefully (no API key = single scorer, no classification = neutral weight).

**Tech Stack:** Rust (reqwest for API calls, existing `ToxicityScorer` trait, serde for API responses), existing PublicAtpClient pattern.

**TDD mandate:** Write ALL tests BEFORE implementation code. Do NOT modify tests unless a dedicated review subagent confirms the test is faulty.

---

## CRITICAL: Read Before Implementing

### 1. PublicAtpClient::xrpc_get signature

The existing client uses a generic deserializer:
```rust
pub async fn xrpc_get<T: DeserializeOwned>(
    &self,
    nsid: &str,
    params: &[(&str, &str)],
) -> Result<T>
```

Array parameters use repeated keys — NOT comma-separated. Example:
```rust
let params = vec![
    ("actor", protected_did),
    ("others", did_1),
    ("others", did_2),  // repeated key, not "others=did_1,did_2"
];
```

### 2. getRelationships response uses `$type` discriminator

The AT Protocol response contains `$type` fields as discriminators. Serde's
`#[serde(tag = "$type")]` should work, but if it doesn't due to other fields
in the response, fall back to `#[serde(untagged)]` with manual matching.
**Test against the real API before assuming the serde model works.**

Verify the live response shape:
```bash
curl 'https://public.api.bsky.app/xrpc/app.bsky.graph.getRelationships?actor=did:plc:h3wpawnrlptr4534chevddo6&others=did:plc:ragtjsm2j2vknwkz3zp4oxrd' | jq .
```

### 3. AccountScore struct changes need DB migration

Adding `graph_distance` to `AccountScore` requires:
- Column in `account_scores` table (SQLite schema.rs migration)
- Column in PostgreSQL migration SQL file
- The `save_score` and `get_scores` methods in both backends
- All test code that constructs `AccountScore` literals

### 4. OpenAI Moderation API does not require an API key for basic use

Actually it DOES require an API key. The endpoint is free (no token charges)
but authentication is mandatory. The env var is `OPENAI_API_KEY`. When absent,
the ensemble falls back to ONNX-only (same as current behavior).

### 5. Existing scorer construction sites

The `ToxicityScorer` is constructed in TWO places:
- `src/main.rs` — CLI `scan`/`sweep`/`score`/`validate` commands
- `src/web/scan_job.rs` — web dashboard scan trigger

BOTH must be updated to construct the ensemble scorer.

### 6. The existing `NoopScorer` pattern

There's a `NoopScorer` in `traits.rs` that panics if called. The ensemble
scorer should handle "no secondary" gracefully (returns primary-only results),
NOT use `NoopScorer` as the secondary.

### 7. Graph distance weight placement in the scoring chain

The graph distance weight is applied AFTER the behavioral modifier and context
multiplier. The full chain in `profile.rs::build_profile` (around line 288-311):

```
1. raw_score = tox * 70 * (1 + overlap * 1.5)     [overlap gate at 0.15]
2. behavioral = apply_behavioral_modifier_contextual(raw_score, ...)
3. context = behavioral * (1 + context_score * 0.5)
4. final = context * graph_distance_weight           ← NEW STEP
5. tier = ThreatTier::from_score(final)
```

Graph distance MUST NOT bypass the benign gate — a mutual follow who is
behaviorally benign should stay benign regardless. Apply distance last.

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `src/bluesky/relationships.rs` | Graph distance classification via `getRelationships` API |
| `src/toxicity/openai_moderation.rs` | OpenAI Moderation API scorer behind `ToxicityScorer` trait |
| `src/toxicity/ensemble.rs` | Ensemble scorer: runs primary + secondary, computes agreement |
| `tests/unit_relationships.rs` | Graph distance classification and scoring weight tests |
| `tests/unit_ensemble.rs` | Ensemble agreement detection, disagreement strategies, merge logic |

### Modified Files
| File | Changes |
|------|---------|
| `src/bluesky/mod.rs` | Add `pub mod relationships;` |
| `src/toxicity/mod.rs` | Add `pub mod openai_moderation;` and `pub mod ensemble;` |
| `src/db/models.rs` | Add `graph_distance: Option<String>` to `AccountScore` |
| `src/db/schema.rs` | Add `graph_distance TEXT` column to `account_scores` in migration |
| `src/db/sqlite.rs` | Update `save_score` and `get_scores` for new column |
| `src/db/postgres.rs` | Update `save_score` and `get_scores` for new column |
| `src/db/queries.rs` | Update SQL strings if queries live here |
| `src/scoring/profile.rs` | Accept `Option<GraphDistance>`, apply weight in Step 6 |
| `src/config.rs` | Add `openai_api_key: Option<String>`, `ensemble_strategy` |
| `src/main.rs` | Construct ensemble scorer, pass graph distance to `build_profile` |
| `src/web/scan_job.rs` | Construct ensemble scorer, call `classify_relationships` for drive-by repliers, pass graph distance |
| `src/output/markdown.rs` | Display graph distance in threat report |
| `tests/composition.rs` | Add `graph_distance: None` to all `AccountScore` literals |
| `tests/unit_scoring.rs` | Add `graph_distance: None` to AccountScore literals if any |
| `tests/unit_behavioral.rs` | Add `graph_distance: None` to AccountScore literals if any |
| `tests/db_postgres.rs` | Add `graph_distance: None` to AccountScore literals if any |
| All other tests constructing `AccountScore` | Add `graph_distance: None` |

---

## Chunk 1: GraphDistance Types + Unit Tests (Task 1)

### What

Create the `GraphDistance` enum and scoring weight function. No API calls yet — pure types and logic.

### Steps

- [ ] **Step 1: Create the test file**

Create `tests/unit_relationships.rs`:

```rust
use charcoal::bluesky::relationships::GraphDistance;

// ============================================================
// GraphDistance enum basics
// ============================================================

#[test]
fn graph_distance_as_str() {
    assert_eq!(GraphDistance::MutualFollow.as_str(), "Mutual follow");
    assert_eq!(GraphDistance::InboundFollow.as_str(), "Follows you");
    assert_eq!(GraphDistance::OutboundFollow.as_str(), "You follow");
    assert_eq!(GraphDistance::Stranger.as_str(), "Stranger");
}

#[test]
fn graph_distance_display() {
    assert_eq!(format!("{}", GraphDistance::Stranger), "Stranger");
    assert_eq!(format!("{}", GraphDistance::MutualFollow), "Mutual follow");
}

// ============================================================
// Threat weights — risk ordering
// ============================================================

#[test]
fn threat_weight_ordering() {
    // Strangers are highest risk, mutual follows lowest
    assert!(GraphDistance::Stranger.threat_weight() > GraphDistance::OutboundFollow.threat_weight());
    assert!(GraphDistance::OutboundFollow.threat_weight() > GraphDistance::InboundFollow.threat_weight());
    assert!(GraphDistance::InboundFollow.threat_weight() > GraphDistance::MutualFollow.threat_weight());
}

#[test]
fn threat_weight_stranger_amplifies() {
    assert!(GraphDistance::Stranger.threat_weight() > 1.0, "Strangers should amplify score");
}

#[test]
fn threat_weight_mutual_dampens() {
    assert!(GraphDistance::MutualFollow.threat_weight() < 1.0, "Mutual follows should dampen score");
}

#[test]
fn threat_weight_specific_values() {
    assert!((GraphDistance::MutualFollow.threat_weight() - 0.6).abs() < 0.001);
    assert!((GraphDistance::InboundFollow.threat_weight() - 0.8).abs() < 0.001);
    assert!((GraphDistance::OutboundFollow.threat_weight() - 0.9).abs() < 0.001);
    assert!((GraphDistance::Stranger.threat_weight() - 1.2).abs() < 0.001);
}

// ============================================================
// Serde roundtrip
// ============================================================

#[test]
fn graph_distance_serde_roundtrip() {
    let original = GraphDistance::Stranger;
    let json = serde_json::to_string(&original).unwrap();
    let restored: GraphDistance = serde_json::from_str(&json).unwrap();
    assert_eq!(original, restored);
}

// ============================================================
// Response parsing
// ============================================================

#[test]
fn parse_relationship_mutual_follow() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "following": "at://did:plc:protected/app.bsky.graph.follow/1",
            "followedBy": "at://did:plc:abc123/app.bsky.graph.follow/2"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::MutualFollow));
}

#[test]
fn parse_relationship_inbound_only() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "followedBy": "at://did:plc:abc123/app.bsky.graph.follow/2"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::InboundFollow));
}

#[test]
fn parse_relationship_outbound_only() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123",
            "following": "at://did:plc:protected/app.bsky.graph.follow/1"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::OutboundFollow));
}

#[test]
fn parse_relationship_no_connection() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#relationship",
            "did": "did:plc:abc123"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::Stranger));
}

#[test]
fn parse_relationship_not_found_actor() {
    let json = serde_json::json!({
        "relationships": [{
            "$type": "app.bsky.graph.defs#notFoundActor",
            "did": "did:plc:abc123"
        }]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.get("did:plc:abc123"), Some(&GraphDistance::Stranger));
}

#[test]
fn parse_relationship_multiple() {
    let json = serde_json::json!({
        "relationships": [
            {
                "$type": "app.bsky.graph.defs#relationship",
                "did": "did:plc:mutual",
                "following": "at://x/y/1",
                "followedBy": "at://x/y/2"
            },
            {
                "$type": "app.bsky.graph.defs#relationship",
                "did": "did:plc:stranger"
            },
            {
                "$type": "app.bsky.graph.defs#notFoundActor",
                "did": "did:plc:gone"
            }
        ]
    });
    let result = charcoal::bluesky::relationships::parse_relationships_response(&json).unwrap();
    assert_eq!(result.len(), 3);
    assert_eq!(result["did:plc:mutual"], GraphDistance::MutualFollow);
    assert_eq!(result["did:plc:stranger"], GraphDistance::Stranger);
    assert_eq!(result["did:plc:gone"], GraphDistance::Stranger);
}
```

Run: `cargo test --test unit_relationships`
Expected: FAIL — module doesn't exist

- [ ] **Step 2: Create `src/bluesky/relationships.rs`**

Implement the `GraphDistance` enum, `threat_weight()`, `as_str()`, Display,
Serialize/Deserialize, and `parse_relationships_response()`.

`parse_relationships_response` takes a `&serde_json::Value` (not a typed
struct) because the `$type` tagged enum may need manual parsing if serde's
tag detection doesn't work with the AT Protocol's response shape. Use
manual JSON field extraction as the safe approach:

```rust
pub fn parse_relationships_response(
    json: &serde_json::Value,
) -> Result<HashMap<String, GraphDistance>> {
    let mut result = HashMap::new();
    let relationships = json["relationships"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Missing relationships array"))?;

    for entry in relationships {
        let did = entry["did"].as_str().unwrap_or_default().to_string();
        if did.is_empty() {
            continue;
        }

        let type_str = entry["$type"].as_str().unwrap_or_default();
        if type_str == "app.bsky.graph.defs#notFoundActor" {
            result.insert(did, GraphDistance::Stranger);
            continue;
        }

        let has_following = entry.get("following")
            .and_then(|v| v.as_str())
            .is_some();
        let has_followed_by = entry.get("followedBy")
            .and_then(|v| v.as_str())
            .is_some();

        let distance = match (has_following, has_followed_by) {
            (true, true) => GraphDistance::MutualFollow,
            (false, true) => GraphDistance::InboundFollow,
            (true, false) => GraphDistance::OutboundFollow,
            (false, false) => GraphDistance::Stranger,
        };
        result.insert(did, distance);
    }

    Ok(result)
}
```

Also implement the async `classify_relationships` function that calls
`client.xrpc_get` and delegates to `parse_relationships_response`.
Chunk the `target_dids` slice into groups of 30 (API limit).

- [ ] **Step 3: Add to module tree**

In `src/bluesky/mod.rs`, add: `pub mod relationships;`

- [ ] **Step 4: Run tests**

Run: `cargo test --test unit_relationships`
Expected: ALL PASS

Run: `cargo test --all-targets`
Expected: ALL existing tests still pass

- [ ] **Step 5: Commit**

```bash
git add src/bluesky/relationships.rs src/bluesky/mod.rs tests/unit_relationships.rs
git commit -m 'feat: add GraphDistance enum and relationship response parsing

GraphDistance classifies the social relationship between the protected
user and another account into MutualFollow, InboundFollow, OutboundFollow,
or Stranger. Each category has a threat_weight() used as a scoring
multiplier. Response parsing handles both relationship and notFoundActor
entries from the AT Protocol getRelationships endpoint.'
```

---

## Chunk 2: Add graph_distance to AccountScore + DB Migration (Task 2)

### What

Add the `graph_distance` field to `AccountScore` and update the database schema.

### Steps

- [ ] **Step 1: Add field to `src/db/models.rs`**

Add to `AccountScore` struct, after `context_score`:

```rust
    /// Social graph distance to the protected user (None if not classified)
    pub graph_distance: Option<String>,
```

- [ ] **Step 2: Fix ALL compilation errors**

Run: `cargo check 2>&1 | grep "missing field" | head -30`

Every file that constructs an `AccountScore` literal needs `graph_distance: None` added.
Known locations (check ALL of these):
- `src/scoring/profile.rs` (the `AccountScore { ... }` return in `build_profile`)
- `src/output/markdown.rs` (if it constructs AccountScore)
- `tests/composition.rs` (the `make_account` helper and any other literals)
- `tests/unit_scoring.rs`
- `tests/unit_behavioral.rs`
- `tests/db_postgres.rs`
- Any other test files that construct AccountScore

Do NOT skip any — run `cargo check` and fix every error.

- [ ] **Step 3: Update SQLite schema migration**

In `src/db/schema.rs`, add a `migrate_v5_to_v6` function (or whatever the
next version is — check the current latest version first):

```rust
fn migrate_v5_to_v6(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "ALTER TABLE account_scores ADD COLUMN graph_distance TEXT;
         PRAGMA user_version = 6;"
    )?;
    Ok(())
}
```

Wire this into the migration chain in the `run_migrations` function.

- [ ] **Step 4: Update SQLite save_score and get_scores**

In `src/db/sqlite.rs`:
- `save_score`: add `graph_distance` to the INSERT/REPLACE column list and bind it
- `get_scores`: add `graph_distance` to the SELECT and map it into `AccountScore`

- [ ] **Step 5: Update PostgreSQL if applicable**

In `src/db/postgres.rs`: same changes as SQLite.
Add a migration SQL file in `migrations/postgres/` for the new column.

- [ ] **Step 6: Run tests**

Run: `cargo test --all-targets`
Expected: ALL PASS (field is None everywhere, neutral behavior)

Run: `cargo test --features web --all-targets`
Expected: ALL PASS

- [ ] **Step 7: Commit**

```bash
git add src/db/models.rs src/db/schema.rs src/db/sqlite.rs src/db/postgres.rs
git add tests/composition.rs tests/unit_scoring.rs tests/unit_behavioral.rs
git add -N migrations/postgres/  # if new migration file created
git commit -m 'feat: add graph_distance field to AccountScore and DB schema

New nullable TEXT column on account_scores stores the social graph
distance label (Mutual follow, Follows you, You follow, Stranger).
Schema migration v5→v6 for SQLite, corresponding Postgres migration.
All existing tests pass with graph_distance: None (neutral).'
```

---

## Chunk 3: Wire Graph Distance into Scoring Formula (Task 3)

### What

Apply the graph distance weight in `build_profile` as the final scoring step.

### Steps

- [ ] **Step 1: Add scoring tests**

Append to `tests/unit_relationships.rs`:

```rust
use charcoal::bluesky::relationships::GraphDistance;
use charcoal::scoring::threat::{ThreatWeights, compute_threat_score};

#[test]
fn stranger_amplifies_score() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.5, 0.3, &weights);
    let amplified = base * GraphDistance::Stranger.threat_weight();
    assert!(amplified > base, "Stranger should amplify: {amplified} > {base}");
}

#[test]
fn mutual_dampens_score() {
    let weights = ThreatWeights::default();
    let (base, _) = compute_threat_score(0.5, 0.3, &weights);
    let dampened = base * GraphDistance::MutualFollow.threat_weight();
    assert!(dampened < base, "Mutual follow should dampen: {dampened} < {base}");
}

#[test]
fn no_distance_is_neutral() {
    let weight: f64 = None::<GraphDistance>.map(|d| d.threat_weight()).unwrap_or(1.0);
    assert!((weight - 1.0).abs() < 0.001, "None distance = 1.0 weight");
}

#[test]
fn stranger_toxic_in_topic_gets_boosted() {
    let weights = ThreatWeights::default();
    // High toxicity + topic overlap + stranger = max danger
    let (base, _) = compute_threat_score(0.7, 0.5, &weights);
    let final_score = (base * GraphDistance::Stranger.threat_weight()).clamp(0.0, 100.0);
    assert!(final_score >= 35.0, "Should be High tier: {final_score}");
}

#[test]
fn mutual_follow_moderate_tox_stays_low() {
    let weights = ThreatWeights::default();
    // Moderate toxicity + overlap + mutual follow = dampened
    let (base, _) = compute_threat_score(0.3, 0.3, &weights);
    let final_score = (base * GraphDistance::MutualFollow.threat_weight()).clamp(0.0, 100.0);
    // base = 0.3 * 70 * (1 + 0.3 * 1.5) = 21 * 1.45 = 30.45
    // dampened = 30.45 * 0.6 = 18.27 → Elevated, not High
    assert!(final_score < 35.0, "Mutual follow should stay below High: {final_score}");
}
```

Run: `cargo test --test unit_relationships`
Expected: PASS (these only use existing functions + enum methods)

- [ ] **Step 2: Update `src/scoring/profile.rs`**

Add `graph_distance: Option<GraphDistance>` parameter to `build_profile`.

In the scoring chain (around line 288-311), after computing `final_score` from
the context multiplier, apply the graph distance weight:

```rust
    // Step 7: Apply graph distance weight
    // Strangers get amplified (1.2x), mutual follows get dampened (0.6x).
    // Applied AFTER benign gate so it cannot bypass ally protections.
    let distance_weight = graph_distance
        .map(|d| d.threat_weight())
        .unwrap_or(1.0);
    let final_score = (final_score * distance_weight).clamp(0.0, 100.0);

    let tier = crate::db::models::ThreatTier::from_score(final_score);
```

Also set the `graph_distance` field on the returned `AccountScore`:

```rust
    graph_distance: graph_distance.map(|d| d.as_str().to_string()),
```

- [ ] **Step 3: Update all callers of `build_profile`**

`build_profile` is called from:
- `src/pipeline/amplification.rs` — pass `None` for now (graph distance
  integration into the pipeline is Task 4)
- `src/main.rs` — pass `None` for CLI commands

Search: `grep -rn "build_profile" src/`

Add `None` as the new parameter at every call site.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets`
Expected: ALL PASS

Run: `cargo test --features web --all-targets`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add src/scoring/profile.rs src/pipeline/amplification.rs src/main.rs
git commit -m 'feat: apply graph distance weight in scoring formula

Graph distance multiplier is the final step in the scoring chain:
raw → behavioral → context → graph_distance. Strangers get 1.2x,
mutual follows get 0.6x. Applied after benign gate so allies stay
protected. All callers pass None for now (wired in next task).'
```

---

## Chunk 4: Wire Graph Distance into Scan Pipeline (Task 4)

### What

Call `classify_relationships` for drive-by repliers in `scan_job.rs` and pass
the results through to `build_profile`.

### Steps

- [ ] **Step 1: Update `src/web/scan_job.rs`**

After the drive-by reply detection loop (around line 292-328), collect all
unique drive-by DIDs, then batch-classify:

```rust
    // Collect all drive-by reply DIDs for batch relationship classification
    let drive_by_did_set: HashSet<String> = events.iter()
        .filter(|e| e.event_type == "reply")
        .map(|e| e.amplifier_did.clone())
        .collect();

    let graph_distances = if !drive_by_did_set.is_empty() {
        let did_refs: Vec<&str> = drive_by_did_set.iter().map(|s| s.as_str()).collect();
        crate::bluesky::relationships::classify_relationships(
            &client, user_did, &did_refs,
        ).await.unwrap_or_default()
    } else {
        HashMap::new()
    };
```

Then pass `graph_distances` through to the amplification pipeline `run()`
function. The pipeline passes it to `build_profile` for each account being
scored.

This requires adding a `graph_distances: &HashMap<String, GraphDistance>`
parameter to `amplification::run()` and threading it through to
`build_profile()` calls.

- [ ] **Step 2: Update `src/pipeline/amplification.rs`**

Add `graph_distances: &HashMap<String, GraphDistance>` parameter to `run()`.

When calling `build_profile` for each account, look up the graph distance:

```rust
let distance = graph_distances.get(target_did).copied();
```

Pass it as the new parameter to `build_profile`.

- [ ] **Step 3: Update CLI caller in `src/main.rs`**

For CLI scan/sweep commands, pass an empty `HashMap` for graph distances
(CLI doesn't do drive-by reply detection yet — that's web-only).

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets`
Expected: ALL PASS

Run: `cargo test --features web --all-targets`
Expected: ALL PASS

- [ ] **Step 5: Commit**

```bash
git add src/web/scan_job.rs src/pipeline/amplification.rs src/main.rs
git commit -m 'feat: classify drive-by repliers via getRelationships in scan pipeline

Batch-classifies all drive-by reply DIDs using getRelationships (30 per
API call). Graph distance is threaded through the amplification pipeline
to build_profile where it multiplies the final threat score. CLI commands
pass empty graph distances (web-only feature for now).'
```

---

## Chunk 5: OpenAI Moderation Scorer (Task 5)

### What

Implement the OpenAI Moderation API as a `ToxicityScorer` backend.

### Steps

- [ ] **Step 1: Create the test file**

Create `tests/unit_ensemble.rs`:

```rust
// ============================================================
// OpenAI Moderation scorer — category mapping
// ============================================================

// NOTE: These tests use a mock scorer, not the real API.
// Integration tests against the live API are #[ignore].

use charcoal::toxicity::traits::{ToxicityResult, ToxicityAttributes, ToxicityScorer};
use charcoal::toxicity::ensemble::{
    EnsembleToxicityScorer, DisagreementStrategy,
};

// -- Mock scorers for testing --

struct FixedScorer {
    result: ToxicityResult,
}

#[async_trait::async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
        Ok(self.result.clone())
    }
}

struct FailingScorer;

#[async_trait::async_trait]
impl ToxicityScorer for FailingScorer {
    async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
        anyhow::bail!("Scorer unavailable")
    }
}

fn make_result(toxicity: f64, identity_attack: f64, insult: f64) -> ToxicityResult {
    ToxicityResult {
        toxicity,
        attributes: ToxicityAttributes {
            severe_toxicity: Some(0.0),
            identity_attack: Some(identity_attack),
            insult: Some(insult),
            profanity: Some(0.0),
            threat: Some(0.0),
        },
    }
}

// ============================================================
// Ensemble — agreement
// ============================================================

#[tokio::test]
async fn ensemble_both_agree_low() {
    let primary = Box::new(FixedScorer { result: make_result(0.1, 0.05, 0.08) });
    let secondary = Box::new(FixedScorer { result: make_result(0.12, 0.06, 0.07) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::TakeLower,
    );
    let result = ensemble.score_ensemble("test text").await.unwrap();
    assert!(result.classifiers_agree, "Should agree — diff is 0.02");
    assert!(result.score_difference < 0.05);
}

#[tokio::test]
async fn ensemble_both_agree_high() {
    let primary = Box::new(FixedScorer { result: make_result(0.85, 0.7, 0.6) });
    let secondary = Box::new(FixedScorer { result: make_result(0.88, 0.72, 0.58) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::TakeLower,
    );
    let result = ensemble.score_ensemble("test text").await.unwrap();
    assert!(result.classifiers_agree);
    // Averaged: (0.85 + 0.88) / 2 = 0.865
    assert!((result.result.toxicity - 0.865).abs() < 0.01);
}

// ============================================================
// Ensemble — disagreement strategies
// ============================================================

#[tokio::test]
async fn ensemble_disagree_take_lower() {
    let primary = Box::new(FixedScorer { result: make_result(0.8, 0.7, 0.6) });
    let secondary = Box::new(FixedScorer { result: make_result(0.2, 0.1, 0.1) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::TakeLower,
    );
    let result = ensemble.score_ensemble("reclaimed slur text").await.unwrap();
    assert!(!result.classifiers_agree, "Should disagree — diff is 0.6");
    assert!((result.result.toxicity - 0.2).abs() < 0.01, "TakeLower should use 0.2");
}

#[tokio::test]
async fn ensemble_disagree_take_higher() {
    let primary = Box::new(FixedScorer { result: make_result(0.2, 0.1, 0.1) });
    let secondary = Box::new(FixedScorer { result: make_result(0.8, 0.7, 0.6) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::TakeHigher,
    );
    let result = ensemble.score_ensemble("coded hostility").await.unwrap();
    assert!(!result.classifiers_agree);
    assert!((result.result.toxicity - 0.8).abs() < 0.01, "TakeHigher should use 0.8");
}

#[tokio::test]
async fn ensemble_disagree_average() {
    let primary = Box::new(FixedScorer { result: make_result(0.8, 0.7, 0.6) });
    let secondary = Box::new(FixedScorer { result: make_result(0.2, 0.1, 0.1) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::Average,
    );
    let result = ensemble.score_ensemble("ambiguous text").await.unwrap();
    assert!(!result.classifiers_agree);
    assert!((result.result.toxicity - 0.5).abs() < 0.01, "Average should be 0.5");
}

// ============================================================
// Ensemble — fallback when secondary fails
// ============================================================

#[tokio::test]
async fn ensemble_secondary_fails_uses_primary() {
    let primary = Box::new(FixedScorer { result: make_result(0.4, 0.3, 0.2) });
    let secondary = Box::new(FailingScorer);
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::TakeLower,
    );
    let result = ensemble.score_ensemble("test").await.unwrap();
    assert!(result.classifiers_agree, "Vacuously true when secondary fails");
    assert!((result.result.toxicity - 0.4).abs() < 0.01);
    assert!(result.secondary_score.is_none());
}

#[tokio::test]
async fn ensemble_no_secondary_uses_primary() {
    let primary = Box::new(FixedScorer { result: make_result(0.4, 0.3, 0.2) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, None, DisagreementStrategy::TakeLower,
    );
    let result = ensemble.score_ensemble("test").await.unwrap();
    assert!(result.classifiers_agree);
    assert!((result.result.toxicity - 0.4).abs() < 0.01);
}

// ============================================================
// Ensemble — ToxicityScorer trait compliance
// ============================================================

#[tokio::test]
async fn ensemble_implements_toxicity_scorer_trait() {
    let primary = Box::new(FixedScorer { result: make_result(0.5, 0.3, 0.2) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, None, DisagreementStrategy::TakeLower,
    );
    // Use through the trait interface
    let scorer: &dyn ToxicityScorer = &ensemble;
    let result = scorer.score_text("test").await.unwrap();
    assert!((result.toxicity - 0.5).abs() < 0.01);
}

// ============================================================
// Ensemble — attribute merging
// ============================================================

#[tokio::test]
async fn ensemble_merges_attributes_on_agreement() {
    let primary = Box::new(FixedScorer { result: make_result(0.5, 0.4, 0.3) });
    let secondary = Box::new(FixedScorer { result: make_result(0.5, 0.6, 0.5) });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::Average,
    );
    let result = ensemble.score_ensemble("test").await.unwrap();
    // identity_attack: (0.4 + 0.6) / 2 = 0.5
    assert!((result.result.attributes.identity_attack.unwrap() - 0.5).abs() < 0.01);
    // insult: (0.3 + 0.5) / 2 = 0.4
    assert!((result.result.attributes.insult.unwrap() - 0.4).abs() < 0.01);
}

#[tokio::test]
async fn ensemble_handles_missing_secondary_profanity() {
    // Primary has profanity, secondary doesn't (OpenAI lacks profanity category)
    let mut primary_result = make_result(0.5, 0.3, 0.2);
    primary_result.attributes.profanity = Some(0.8);
    let mut secondary_result = make_result(0.5, 0.3, 0.2);
    secondary_result.attributes.profanity = None;

    let primary = Box::new(FixedScorer { result: primary_result });
    let secondary = Box::new(FixedScorer { result: secondary_result });
    let ensemble = EnsembleToxicityScorer::new(
        primary, Some(secondary), DisagreementStrategy::Average,
    );
    let result = ensemble.score_ensemble("test").await.unwrap();
    // When one has it and other doesn't, keep the one that exists
    assert!(result.result.attributes.profanity.is_some());
    assert!((result.result.attributes.profanity.unwrap() - 0.8).abs() < 0.01);
}
```

Run: `cargo test --test unit_ensemble`
Expected: FAIL — modules don't exist

- [ ] **Step 2: Create `src/toxicity/openai_moderation.rs`**

Implement `OpenAiModerationScorer` behind the `ToxicityScorer` trait.

Key implementation details:
- Endpoint: `POST https://api.openai.com/v1/moderations`
- Model: `"omni-moderation-2024-09-26"` (version-pinned string constant)
- Auth header: `Authorization: Bearer {api_key}`
- Request body: `{"model": "...", "input": "text"}`
- Response: `results[0].category_scores` contains named float fields

Category mapping to `ToxicityAttributes`:
- `identity_attack` ← `hate`
- `insult` ← `harassment`
- `threat` ← `max(harassment/threatening, violence)`
- `severe_toxicity` ← `max(hate/threatening, violence/graphic)`
- `profanity` ← `None` (OpenAI doesn't have this)
- `toxicity` (overall) ← `max(identity_attack, insult, threat, severe)`

Note: serde field names use `/` in the API response. Use
`#[serde(rename = "harassment/threatening")]` etc.

- [ ] **Step 3: Create `src/toxicity/ensemble.rs`**

Implement `EnsembleToxicityScorer`, `EnsembleResult`, `DisagreementStrategy`,
and the `merge_results` helper. Follow the logic from the tests:

- Agreement threshold: `0.25` (constant)
- When agree: average both scores (50/50 weight)
- When disagree: apply strategy (TakeLower, Average, TakeHigher)
- When secondary fails: use primary, mark `classifiers_agree: true` (vacuously)
- When no secondary: same as above

Implement both `score_ensemble` (returns `EnsembleResult` with metadata) and
the `ToxicityScorer` trait (calls `score_ensemble`, returns just the result).

- [ ] **Step 4: Add to module tree**

In `src/toxicity/mod.rs`, add:
```rust
pub mod ensemble;
pub mod openai_moderation;
```

- [ ] **Step 5: Run tests**

Run: `cargo test --test unit_ensemble`
Expected: ALL PASS

Run: `cargo test --all-targets`
Expected: ALL existing tests still pass

- [ ] **Step 6: Commit**

```bash
git add src/toxicity/openai_moderation.rs src/toxicity/ensemble.rs src/toxicity/mod.rs
git add tests/unit_ensemble.rs
git commit -m 'feat: add OpenAI Moderation scorer and ensemble toxicity scorer

OpenAiModerationScorer wraps the free moderation endpoint (pinned to
omni-moderation-2024-09-26). EnsembleToxicityScorer runs primary (ONNX)
and secondary (OpenAI) concurrently, detects agreement (threshold 0.25),
and applies configurable disagreement strategy (TakeLower default).
Implements ToxicityScorer trait so it is invisible to downstream pipeline.'
```

---

## Chunk 6: Wire Ensemble into Config and Scan Pipeline (Task 6)

### What

Add config env vars and construct the ensemble in both CLI and web scan paths.

### Steps

- [ ] **Step 1: Update `src/config.rs`**

Add fields:
```rust
    /// OpenAI API key (free, enables ensemble scoring)
    pub openai_api_key: Option<String>,
```

Load from env:
```rust
    openai_api_key: std::env::var("OPENAI_API_KEY").ok(),
```

- [ ] **Step 2: Update scorer construction in `src/web/scan_job.rs`**

Find where the scorer is constructed (search for `Box<dyn ToxicityScorer>`
or `OnnxToxicityScorer`). Wrap the existing primary scorer in an ensemble:

```rust
let secondary_scorer: Option<Box<dyn ToxicityScorer>> = config
    .openai_api_key
    .as_ref()
    .and_then(|key| {
        match crate::toxicity::openai_moderation::OpenAiModerationScorer::new(key) {
            Ok(s) => {
                info!("OpenAI Moderation scorer loaded — ensemble scoring enabled");
                Some(Box::new(s) as Box<dyn ToxicityScorer>)
            }
            Err(e) => {
                warn!(error = %e, "Failed to init OpenAI scorer, using ONNX only");
                None
            }
        }
    });

let scorer: Box<dyn ToxicityScorer> = Box::new(
    crate::toxicity::ensemble::EnsembleToxicityScorer::new(
        primary_scorer,  // the existing ONNX scorer, already boxed
        secondary_scorer,
        crate::toxicity::ensemble::DisagreementStrategy::TakeLower,
    )
);
```

- [ ] **Step 3: Update scorer construction in `src/main.rs`**

Same pattern as scan_job.rs for CLI commands that construct a scorer.

- [ ] **Step 4: Run tests**

Run: `cargo test --all-targets`
Expected: ALL PASS (no OPENAI_API_KEY in test env = ensemble with primary only)

Run: `cargo test --features web --all-targets`
Expected: ALL PASS

- [ ] **Step 5: Update README.md**

Add to the Environment Variables section:
```
OPENAI_API_KEY          (optional) Enables ensemble toxicity scoring with OpenAI Moderation
```

- [ ] **Step 6: Commit**

```bash
git add src/config.rs src/web/scan_job.rs src/main.rs README.md
git commit -m 'feat: wire ensemble scorer into scan pipeline and CLI

When OPENAI_API_KEY is set, the scanner uses EnsembleToxicityScorer
(ONNX primary + OpenAI Moderation secondary with TakeLower disagreement
strategy). When absent, falls back to ONNX-only (same as before).
No behavior change without the env var.'
```

---

## Chunk 7: Report Display + Final Polish (Task 7)

### What

Show graph distance and ensemble agreement in the markdown report and terminal output.

### Steps

- [ ] **Step 1: Update `src/output/markdown.rs`**

In the account detail section of the report, add the graph distance label:

```
| @handle | High (72.1) | Stranger | tox: 0.82, overlap: 0.45, ctx: 0.7 |
```

- [ ] **Step 2: Update `src/output/terminal.rs`**

Add graph distance to the colored terminal output, after the threat tier.

- [ ] **Step 3: Run tests**

Run: `cargo test --all-targets`
Expected: ALL PASS

- [ ] **Step 4: Commit**

```bash
git add src/output/markdown.rs src/output/terminal.rs
git commit -m 'feat: display graph distance in reports and terminal output

Threat reports now show the social graph relationship (Stranger, Follows
you, You follow, Mutual follow) alongside the threat tier. Stranger
accounts are visually distinguished in the report.'
```

---

## Verification

After all chunks are complete:

- [ ] `cargo test --all-targets` — ALL PASS
- [ ] `cargo test --features web --all-targets` — ALL PASS
- [ ] `cargo clippy -- -D warnings` — CLEAN
- [ ] `cargo fmt --check` — CLEAN
- [ ] Run a live scan and verify graph distance appears in output
- [ ] Set `OPENAI_API_KEY` and verify ensemble logs appear in tracing output
- [ ] Unset `OPENAI_API_KEY` and verify fallback to ONNX-only works
