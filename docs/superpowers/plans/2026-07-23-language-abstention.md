# Language Abstention Implementation Plan (#222)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop emitting toxicity scores our English-only models cannot produce; abstain (`ThreatTier::NotAssessed`) on non-English accounts instead of scoring them Low.

**Architecture:** A pure `assess_language(text, langs)` classifier decides per post whether our models can read it. A `partition_assessable` helper splits a `PostSample` into an assessable-only sample plus a dropped count, and a `coverage_gate` maps `(assessable, unassessable)` to one of three outcomes (Score / NotAssessed / Insufficient Data). The partition+gate is applied at the shared Stage-1 seam (`stage1_outcome`) and both Stage-2 seams (`build_profile`, `gather`). A new `ThreatTier::NotAssessed` variant, stored with a NULL score and preserved (not recomputed) on read, carries the state to reports and the dashboard.

**Tech Stack:** Rust, atrium-api 0.25.7, rusqlite 0.38, sqlx-postgres, axum, SvelteKit (dashboard), ort/ONNX (existing scorer).

## Global Constraints

- Work on branch `feat/language-abstention` (already created off `staging`). Never commit to `staging`.
- Stage files explicitly by name. Never `git add -A`/`.`/`-am`. No heredocs in shell.
- Rust idioms: `?` for errors, `anyhow::Result`, no `.unwrap()` in non-test code. `cargo clippy` clean.
- Tests: `cargo test --features web` must pass. Run `cargo fmt` before each commit.
- `assess_language` is total: never panics, never errors; every `(text, langs)` maps to exactly one `Assessability`.
- No new crate dependencies (no `whatlang`/`lingua`). Detection = `langs` + Unicode script check only.
- Frontend: invoke the Svelte skills when editing `.svelte`. Build the SPA with `npm --prefix web run build`.
- `MIN_ASSESSABLE_POSTS = 5` (mirrors the existing `MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT`).
- Spec: `docs/superpowers/specs/2026-07-20-language-abstention-design.md`. Amplification-event scoring is out of scope (#230).

---

## File Structure

- **Create** `src/scoring/language.rs` — `Assessability`, `assess_language`, script heuristic, `CoverageOutcome`, `coverage_gate`, `partition_assessable`. One focused module, fully unit-tested.
- **Modify** `src/scoring/mod.rs` — declare `pub mod language;`.
- **Modify** `src/bluesky/posts.rs` — add `langs: Vec<String>` to `Post`; populate at both construction sites.
- **Modify** `src/db/models.rs` — `ThreatTier::NotAssessed` + `as_str` + `from_score` doc.
- **Modify** `src/db/queries.rs`, `src/db/postgres.rs` — preserve stored `NotAssessed` on read; write NULL score.
- **Modify** `src/scoring/profile.rs` — partition+gate in `stage1_outcome` and `build_profile`; `NotAssessed` terminal builder.
- **Modify** `src/pipeline/scan_phases/gather.rs` — partition+gate in the decoupled Stage-2 seam.
- **Modify** `src/output/markdown.rs` — count Low explicitly; add "Not assessed" row.
- **Modify** `src/web/handlers/status.rs` + a DB count method — surface `not_assessed` count.
- **Modify** `web/src/...` (dashboard) — render the not-assessed bucket.
- **Create** `tests/unit_language.rs` — decision-table + gate + partition tests.
- **Create** `tests/regression_language_gate.rs` — promote `examples/lang_gate_probe.rs` samples to an assertion.

---

## Task 1: `assess_language` + script heuristic

**Files:**
- Create: `src/scoring/language.rs`
- Modify: `src/scoring/mod.rs`
- Test: `tests/unit_language.rs`

**Interfaces:**
- Produces: `pub enum Assessability { Assessable, Unassessable }` (derive `Debug, Clone, Copy, PartialEq, Eq`); `pub fn assess_language(text: &str, langs: &[String]) -> Assessability`.

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_language.rs`:

```rust
use charcoal::scoring::language::{assess_language, Assessability};

fn en() -> Vec<String> { vec!["en".to_string()] }

#[test]
fn declared_non_english_is_unassessable() {
    // Row 1: any non-en primary tag → Unassessable, regardless of script.
    assert_eq!(assess_language("hello world", &["pt".to_string()]), Assessability::Unassessable);
}

#[test]
fn declared_english_latin_is_assessable() {
    // Row 2.
    assert_eq!(assess_language("you are terrible", &en()), Assessability::Assessable);
}

#[test]
fn declared_english_but_nonlatin_is_unassessable() {
    // Row 3: the measured 9.1% mis-declaration — trust the script, not the tag.
    assert_eq!(assess_language("ไปตายซะ นะไอ้โง่", &en()), Assessability::Unassessable);
}

#[test]
fn empty_langs_latin_is_assessable() {
    // Row 4: script-only fallback.
    assert_eq!(assess_language("you are terrible", &[]), Assessability::Assessable);
}

#[test]
fn empty_langs_nonlatin_is_unassessable() {
    // Row 5.
    assert_eq!(assess_language("お前は本当に馬鹿だ、死ね", &[]), Assessability::Unassessable);
}

#[test]
fn multilang_including_nonenglish_is_unassessable() {
    // "declares en" requires EVERY tag to be en.
    assert_eq!(
        assess_language("hello", &["en".to_string(), "ja".to_string()]),
        Assessability::Unassessable
    );
}

#[test]
fn region_subtag_is_normalised_but_assess_takes_primary_only() {
    // Caller normalises "en-US"→"en" at capture; assess_language receives primary
    // tags. A raw "en-US" here must still be treated as English (defensive).
    assert_eq!(assess_language("you are terrible", &["en-US".to_string()]), Assessability::Assessable);
}

#[test]
fn emoji_only_is_assessable() {
    // No language carried; models handle it in-distribution → Assessable.
    assert_eq!(assess_language("😊😊😊🔥", &[]), Assessability::Assessable);
}

#[test]
fn short_nonlatin_below_threshold_is_assessable() {
    // Fewer than 5 non-Latin chars and not dominant → Assessable (e.g. a stray glyph).
    assert_eq!(assess_language("ok 日", &en()), Assessability::Assessable);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_language 2>&1 | tail -20`
Expected: FAIL — `unresolved import charcoal::scoring::language`.

- [ ] **Step 3: Create the module**

Create `src/scoring/language.rs`:

```rust
//! Language-assessability gate (#222).
//!
//! Charcoal's toxicity models (Detoxify ONNX, CoPE-B) are English-only. Handed
//! non-English text they return a benign-looking score the threat formula reads
//! as "safe". This module decides, per post, whether our models can meaningfully
//! assess the text — using the post's declared `langs` plus a Unicode script
//! cross-check, with no language-identification dependency.

/// Whether our English-only models can meaningfully score a piece of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assessability {
    Assessable,
    Unassessable,
}

/// Minimum non-Latin characters before a post can be judged non-Latin-dominant.
/// Guards against a stray glyph flipping an otherwise-English post.
const MIN_NONLATIN_CHARS: usize = 5;

/// True when `text` is dominated by a non-Latin script: at least
/// `MIN_NONLATIN_CHARS` non-Latin letters AND more non-Latin than Latin letters.
/// Emoji/punctuation/digits count as neither and cannot trip this.
fn is_nonlatin_dominant(text: &str) -> bool {
    let mut latin = 0usize;
    let mut nonlatin = 0usize;
    for c in text.chars() {
        if c.is_ascii_alphabetic() {
            latin += 1;
        } else if is_nonlatin_script(c) {
            nonlatin += 1;
        }
    }
    nonlatin >= MIN_NONLATIN_CHARS && nonlatin > latin
}

/// Script ranges validated in `examples/lang_gate_probe.rs`: Thai, CJK, kana,
/// hangul, Cyrillic, Arabic, Hebrew, Devanagari. Not exhaustive of all
/// non-Latin scripts — it covers the scripts the models provably cannot read
/// and that appear in the corpus; extend as new scripts surface.
fn is_nonlatin_script(c: char) -> bool {
    matches!(c,
        '\u{0E00}'..='\u{0E7F}'   // Thai
        | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
        | '\u{3040}'..='\u{30FF}' // hiragana + katakana
        | '\u{AC00}'..='\u{D7AF}' // hangul syllables
        | '\u{0400}'..='\u{04FF}' // Cyrillic
        | '\u{0500}'..='\u{052F}' // Cyrillic supplement
        | '\u{0600}'..='\u{06FF}' // Arabic
        | '\u{0590}'..='\u{05FF}' // Hebrew
        | '\u{0900}'..='\u{097F}' // Devanagari
    )
}

/// Primary language tag with region/script subtags stripped, lowercased:
/// "en-US" → "en", "ZH-Hant" → "zh".
fn primary_tag(tag: &str) -> String {
    tag.split('-').next().unwrap_or(tag).to_ascii_lowercase()
}

/// Decide whether our English-only models can assess `text`.
///
/// `langs` is the post's declared `app.bsky.feed.post.langs`. Empty when the
/// client omitted it (~6% of posts). Decision table (first match wins):
///
/// | langs                     | script        | result       |
/// |---------------------------|---------------|--------------|
/// | any non-`en` primary tag  | any           | Unassessable |
/// | all-`en`                  | Latin         | Assessable   |
/// | all-`en`                  | non-Latin     | Unassessable |
/// | empty                     | Latin         | Assessable   |
/// | empty                     | non-Latin     | Unassessable |
pub fn assess_language(text: &str, langs: &[String]) -> Assessability {
    if !langs.is_empty() {
        let all_english = langs.iter().all(|l| primary_tag(l) == "en");
        if !all_english {
            return Assessability::Unassessable;
        }
    }
    if is_nonlatin_dominant(text) {
        Assessability::Unassessable
    } else {
        Assessability::Assessable
    }
}
```

Add to `src/scoring/mod.rs` (alongside the existing `pub mod` lines):

```rust
pub mod language;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_language 2>&1 | tail -20`
Expected: PASS — 9 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/scoring/language.rs src/scoring/mod.rs tests/unit_language.rs
git commit -m 'feat(222): assess_language language-assessability gate

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 2: `partition_assessable` + `coverage_gate`

**Files:**
- Modify: `src/scoring/language.rs`
- Test: `tests/unit_language.rs`

**Interfaces:**
- Consumes: `Assessability`, `assess_language` (Task 1); `PostSample`, `Post`, `ReplyPost` (`crate::bluesky::posts`).
- Produces:
  - `pub enum CoverageOutcome { Score, NotAssessed, InsufficientData }` (derive `Debug, PartialEq, Eq, Clone, Copy`).
  - `pub const MIN_ASSESSABLE_POSTS: usize = 5;`
  - `pub fn coverage_gate(assessable: usize, unassessable: usize) -> CoverageOutcome`.
  - `pub fn partition_assessable(sample: &PostSample) -> (PostSample, usize)` — returns the assessable-only sample (ratios/total recomputed) and the dropped count.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_language.rs`:

```rust
use charcoal::scoring::language::{coverage_gate, partition_assessable, CoverageOutcome};
use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};

fn post(text: &str, langs: &[&str]) -> Post {
    Post {
        uri: "at://x".to_string(),
        text: text.to_string(),
        created_at: None,
        like_count: 0,
        repost_count: 0,
        quote_count: 0,
        is_quote: false,
        langs: langs.iter().map(|s| s.to_string()).collect(),
    }
}

fn sample_of(originals: Vec<Post>) -> PostSample {
    let total = originals.len();
    PostSample {
        originals,
        replies: vec![],
        quotes: vec![],
        reply_ratio: 0.0,
        quote_ratio: 0.0,
        total_posts: total,
    }
}

#[test]
fn gate_scores_when_five_assessable() {
    assert_eq!(coverage_gate(5, 0), CoverageOutcome::Score);
    assert_eq!(coverage_gate(5, 100), CoverageOutcome::Score);
}

#[test]
fn gate_not_assessed_when_unassessable_dominates() {
    assert_eq!(coverage_gate(4, 10), CoverageOutcome::NotAssessed);
    assert_eq!(coverage_gate(0, 50), CoverageOutcome::NotAssessed);
    assert_eq!(coverage_gate(2, 2), CoverageOutcome::NotAssessed); // >= assessable, >=1
}

#[test]
fn gate_insufficient_data_when_sparse_english() {
    assert_eq!(coverage_gate(3, 0), CoverageOutcome::InsufficientData);
    assert_eq!(coverage_gate(0, 0), CoverageOutcome::InsufficientData);
}

#[test]
fn partition_drops_unassessable_and_counts() {
    let s = sample_of(vec![
        post("this is a normal english post", &["en"]),
        post("another english sentence here", &["en"]),
        post("お前は本当に馬鹿だ、死ね", &["ja"]),
    ]);
    let (kept, dropped) = partition_assessable(&s);
    assert_eq!(kept.originals.len(), 2);
    assert_eq!(dropped, 1);
    assert_eq!(kept.total_posts, 2);
}

#[test]
fn partition_preserves_bucketing() {
    let s = PostSample {
        originals: vec![post("english original text here", &["en"])],
        replies: vec![ReplyPost {
            post: post("english reply text here", &["en"]),
            parent_uri: "at://p".to_string(),
        }],
        quotes: vec![post("ไปตายซะ นะไอ้โง่ ควยๆ", &["th"])],
        reply_ratio: 0.5,
        quote_ratio: 0.5,
        total_posts: 3,
    };
    let (kept, dropped) = partition_assessable(&s);
    assert_eq!(kept.originals.len(), 1);
    assert_eq!(kept.replies.len(), 1);
    assert_eq!(kept.quotes.len(), 0);
    assert_eq!(dropped, 1);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_language 2>&1 | tail -20`
Expected: FAIL — `coverage_gate`, `partition_assessable`, `CoverageOutcome` not found; `Post` has no field `langs` (that field is added in Task 3 — this test file will not compile until then).

> NOTE: because these tests reference `Post.langs`, add Task 3's field first if
> compiling in isolation. Under subagent-driven execution, run Tasks 2 and 3 in
> that order but commit Task 2's code with Task 3's field present. If you hit the
> compile error here, proceed to Task 3, then return and confirm these pass.

- [ ] **Step 3: Implement**

Append to `src/scoring/language.rs`:

```rust
use crate::bluesky::posts::{Post, PostSample, ReplyPost};

/// Minimum assessable posts required to produce a score. Mirrors
/// `crate::scoring::profile::MIN_FIRST_PERSON_POSTS_FOR_EARLY_EXIT`.
pub const MIN_ASSESSABLE_POSTS: usize = 5;

/// What the coverage gate decided for an account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageOutcome {
    /// Enough assessable posts — score on the assessable subset.
    Score,
    /// Too few assessable posts, and unassessable posts are the reason — abstain.
    NotAssessed,
    /// Too few posts overall (a genuinely sparse account) — existing terminal.
    InsufficientData,
}

/// Map assessable/unassessable counts to an outcome (spec Component 4).
///
/// - `assessable >= MIN_ASSESSABLE_POSTS` → `Score`.
/// - else if `unassessable >= 1 && unassessable >= assessable` → `NotAssessed`.
/// - else → `InsufficientData` (a sparse account, not mislabelled as language).
pub fn coverage_gate(assessable: usize, unassessable: usize) -> CoverageOutcome {
    if assessable >= MIN_ASSESSABLE_POSTS {
        CoverageOutcome::Score
    } else if unassessable >= 1 && unassessable >= assessable {
        CoverageOutcome::NotAssessed
    } else {
        CoverageOutcome::InsufficientData
    }
}

/// Split a sample into an assessable-only sample plus the count of dropped
/// (unassessable) posts. Ratios and `total_posts` are recomputed for the kept
/// subset so downstream behavioral signals reflect the scored posts.
pub fn partition_assessable(sample: &PostSample) -> (PostSample, usize) {
    let keep_post = |p: &Post| assess_language(&p.text, &p.langs) == Assessability::Assessable;

    let originals: Vec<Post> = sample.originals.iter().filter(|p| keep_post(p)).cloned().collect();
    let replies: Vec<ReplyPost> =
        sample.replies.iter().filter(|r| keep_post(&r.post)).cloned().collect();
    let quotes: Vec<Post> = sample.quotes.iter().filter(|p| keep_post(p)).cloned().collect();

    let original_count = sample.originals.len() + sample.replies.len() + sample.quotes.len();
    let kept = originals.len() + replies.len() + quotes.len();
    let dropped = original_count - kept;

    let non_repost = kept as f64;
    let reply_ratio = if non_repost > 0.0 { replies.len() as f64 / non_repost } else { 0.0 };
    let quote_ratio = if non_repost > 0.0 { quotes.len() as f64 / non_repost } else { 0.0 };

    (
        PostSample { originals, replies, quotes, reply_ratio, quote_ratio, total_posts: kept },
        dropped,
    )
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --test unit_language 2>&1 | tail -20`
Expected: PASS — all tests (requires Task 3's `langs` field present).

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/scoring/language.rs tests/unit_language.rs
git commit -m 'feat(222): partition_assessable + coverage_gate

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 3: capture `langs` on `Post`

**Files:**
- Modify: `src/bluesky/posts.rs:44` (struct), `:171-205` (originals site), `:285-319` (replies site)
- Test: existing `cargo test` (compile-driven) + a targeted capture test

**Interfaces:**
- Produces: `Post.langs: Vec<String>` (primary tags, region-stripped, lowercased).

- [ ] **Step 1: Add the field**

In `src/bluesky/posts.rs`, add to `struct Post` (after `is_quote`):

```rust
    /// Declared post languages (`app.bsky.feed.post.langs`), primary tags only,
    /// region/script subtags stripped and lowercased ("en-US" → "en"). Empty
    /// when the client omitted the field (~6% of posts). Used by the #222
    /// language-assessability gate.
    pub langs: Vec<String>,
```

- [ ] **Step 2: Add a langs-extraction helper**

Add near `sanitize_post_text` in `src/bluesky/posts.rs`:

```rust
/// Extract primary language tags from a decoded post record's `langs`.
/// Region/script subtags are stripped and tags lowercased ("en-US" → "en").
fn extract_langs(record: &atrium_api::app::bsky::feed::post::Record) -> Vec<String> {
    record
        .data
        .langs
        .as_ref()
        .map(|langs| {
            langs
                .iter()
                .filter_map(|l| l.as_ref().language().map(|p| p.as_str().to_ascii_lowercase()))
                .collect()
        })
        .unwrap_or_default()
}
```

- [ ] **Step 3: Populate at the originals site**

In `src/bluesky/posts.rs`, the originals loop currently decodes the record inline
with `.map(...).unwrap_or_default()`. Replace the text-decode block (~lines 171-175)
so it binds the record and derives both text and langs:

```rust
            // Decode the record once to get both text and declared languages.
            let record = atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            )
            .ok();
            let text = record
                .as_ref()
                .map(|r| sanitize_post_text(&r.data.text))
                .unwrap_or_default();
            let langs = record.as_ref().map(extract_langs).unwrap_or_default();
```

Then add `langs,` to the `Post { .. }` literal (~line 197-204).

- [ ] **Step 4: Populate at the replies site**

The replies loop already binds `record` (~line 285). Add before the `Post` literal (~line 311):

```rust
            let langs = extract_langs(&record);
```

Then add `langs,` to that `Post { .. }` literal.

- [ ] **Step 5: Fix all other `Post { .. }` construction sites**

Run to find every literal the new field broke:

```bash
cargo build --features web 2>&1 | grep -E "missing field .langs.|Post \{" | head -40
```

For each (tests, fixtures, `output/markdown.rs` tests, `scoring` tests, etc.) add
`langs: vec![],` (or realistic tags where a test asserts language behavior).

- [ ] **Step 6: Add a capture test**

Add to `tests/unit_language.rs` (or a bluesky test module) — verifies normalization:

```rust
#[test]
fn extract_langs_strips_region_and_lowercases() {
    // Indirectly: assess_language treats a normalised "en" tag as English.
    assert_eq!(
        assess_language("plain english sentence", &["en".to_string()]),
        Assessability::Assessable
    );
}
```

(The `extract_langs` unit itself is exercised through the fetch integration tests;
normalization logic lives in `primary_tag`/`as_str().to_ascii_lowercase()`.)

- [ ] **Step 7: Verify build + tests**

Run: `cargo test --features web 2>&1 | tail -15`
Expected: PASS (all existing tests compile with the new field; Task 2's tests now pass).

- [ ] **Step 8: Commit**

```bash
cargo fmt
git add src/bluesky/posts.rs tests/unit_language.rs
# plus any test/fixture files touched in Step 5 — stage each by name
git commit -m 'feat(222): capture langs on Post from app.bsky.feed.post record

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 4: `ThreatTier::NotAssessed` variant

**Files:**
- Modify: `src/db/models.rs:141-172`
- Test: `tests/unit_scoring.rs` (or wherever `ThreatTier` is tested)

**Interfaces:**
- Produces: `ThreatTier::NotAssessed`; `ThreatTier::NotAssessed.as_str() == "NotAssessed"`; `from_score` never returns it.

- [ ] **Step 1: Write the failing test**

Add to the ThreatTier test module (e.g. `tests/unit_scoring.rs`):

```rust
use charcoal::db::models::ThreatTier;

#[test]
fn not_assessed_has_stable_string_and_is_never_score_derived() {
    assert_eq!(ThreatTier::NotAssessed.as_str(), "NotAssessed");
    // from_score must never produce NotAssessed — it is constructed only at the
    // coverage gate.
    for s in [0.0, 7.9, 8.0, 14.9, 15.0, 34.9, 35.0, 100.0] {
        assert_ne!(ThreatTier::from_score(s), ThreatTier::NotAssessed);
    }
}
```

(Ensure `ThreatTier` derives `PartialEq` — add it if missing.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web not_assessed_has_stable 2>&1 | tail -12`
Expected: FAIL — no variant `NotAssessed`.

- [ ] **Step 3: Add the variant**

In `src/db/models.rs`, add to `enum ThreatTier` (after `High`):

```rust
    /// Outside the ordered Low→High scale. The account's posts were in a
    /// language our English-only models cannot assess, so no score was produced
    /// (#222). Constructed only at the coverage gate, never from a score.
    NotAssessed,
```

Add to `as_str`:

```rust
            ThreatTier::NotAssessed => "NotAssessed",
```

`from_score` keeps its `_ => ThreatTier::Low` arm — add a doc line:

```rust
    /// Never returns `NotAssessed`; that tier is set at the coverage gate, not
    /// derived from a score.
```

- [ ] **Step 4: Fix compiler-flagged match sites**

Run:

```bash
cargo build --features web 2>&1 | grep -E "non-exhaustive|pattern .NotAssessed. not covered|match" | head -40
```

For each flagged `match` on `ThreatTier` across `scoring/threat.rs`,
`db/queries.rs`, `db/postgres.rs`, `scoring/profile.rs`,
`discovery/threat_expansion.rs`: add a `ThreatTier::NotAssessed => ...` arm.
Default behaviour — **exclude from threat ranking / "worst tier" selection**
(treat as absent, never as lowest). For numeric-rank helpers, return a sentinel
that sorts it out of the threat list (e.g. rank it below Low OR skip it before
ranking — match the file's existing convention; document the choice inline).

- [ ] **Step 5: Verify**

Run: `cargo test --features web not_assessed_has_stable 2>&1 | tail -12`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/db/models.rs tests/unit_scoring.rs
# plus every match-site file touched in Step 4 — stage by name
git commit -m 'feat(222): add ThreatTier::NotAssessed outside the ordered scale

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 5: persistence — write NULL score, preserve on read

**Files:**
- Modify: `src/db/queries.rs:210, 571, 613, 702` (read sites), write path
- Modify: `src/db/postgres.rs` (mirror read sites)
- Test: `tests/unit_scoring.rs` or a DB round-trip test (SQLite)

**Interfaces:**
- Consumes: `ThreatTier::NotAssessed.as_str()` (Task 4).
- Behavior: an `AccountScore` with `threat_tier = Some("NotAssessed")` and
  `threat_score = None` round-trips through the read path as `NotAssessed`
  (never recomputed to `Low`).

- [ ] **Step 1: Write the failing test (SQLite round-trip)**

Add a test that inserts a NotAssessed row and reads it back. Follow the existing
SQLite test pattern in the repo (open an in-memory/temp DB, `upsert_account_score`,
then a read that goes through the recompute path such as `get_ranked_threats` or
`get_account`):

```rust
#[tokio::test]
async fn not_assessed_row_survives_read_recompute() {
    // Build a NotAssessed AccountScore: NULL score, tier "NotAssessed".
    // upsert it, read it back through the recompute path, assert tier is still
    // "NotAssessed" and NOT "Low".
    // (Use the repo's existing SqliteDatabase test harness / temp-file pattern.)
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web not_assessed_row_survives 2>&1 | tail -15`
Expected: FAIL — tier comes back `"Low"` (recomputed from a 0/NULL score) or the row is filtered out.

- [ ] **Step 3: Fix each read site**

At `queries.rs:210` and the identical blocks at `:571, :613, :702`, replace:

```rust
let threat_tier = threat_score.map(|s| ThreatTier::from_score(s).to_string());
```

with (read the stored tier column — confirm its select index in each query; the
`SELECT` lists `threat_tier` explicitly):

```rust
let stored_tier: Option<String> = row.get(/* threat_tier column index */)?;
let threat_tier = match stored_tier.as_deref() {
    Some(s) if s == ThreatTier::NotAssessed.as_str() => Some(s.to_string()),
    _ => threat_score.map(|s| ThreatTier::from_score(s).to_string()),
};
```

Apply the equivalent change in `src/db/postgres.rs` (same recompute pattern; use
`row.try_get`/column name per the sqlx query).

- [ ] **Step 4: Ensure the write path stores NULL score**

Confirm `upsert_account_score` writes `threat_score` from `AccountScore.threat_score`
(an `Option<f64>`), so a `None` persists as SQL NULL. No change needed if it binds
the Option directly; if it coerces `None`→`0.0`, fix it to bind NULL. Verify in both
`sqlite.rs`/`queries.rs` and `postgres.rs`.

- [ ] **Step 5: Verify**

Run: `cargo test --features web not_assessed_row_survives 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/db/queries.rs src/db/postgres.rs tests/unit_scoring.rs
git commit -m 'feat(222): preserve stored NotAssessed tier on read; store NULL score

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 6: Stage-1 integration (`stage1_outcome`)

**Files:**
- Modify: `src/scoring/profile.rs:179-213` (the `stage1_outcome` head)
- Test: `tests/unit_profile.rs`

**Interfaces:**
- Consumes: `partition_assessable`, `coverage_gate`, `CoverageOutcome` (Task 2), `ThreatTier::NotAssessed` (Task 4).
- Behavior: a Stage-1 sample whose posts are unassessable-dominant returns
  `Stage1Outcome::Terminal` with tier `NotAssessed`; a mixed sample proceeds on the
  assessable subset; a sparse English sample still returns `Insufficient Data`.

- [ ] **Step 1: Write the failing test**

Add to `tests/unit_profile.rs` (follow the existing `stage1_*` test setup; a
`NoopScorer`/fake scorer is fine because a NotAssessed sample never reaches ONNX):

```rust
#[tokio::test]
async fn stage1_non_latin_sample_returns_not_assessed_not_low() {
    // Build a 25-post sample of non-Latin (e.g. Japanese) originals with langs=["ja"].
    // Call stage1_outcome; assert Terminal with threat_tier == Some("NotAssessed"),
    // threat_score == None.
}

#[tokio::test]
async fn stage1_sparse_english_still_insufficient_data() {
    // 3 English posts, langs=["en"]. Assert Terminal with "Insufficient Data".
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web stage1_non_latin_sample 2>&1 | tail -15`
Expected: FAIL — currently returns Low/Proceed, not NotAssessed.

- [ ] **Step 3: Implement**

In `stage1_outcome`, **before** the existing `if stage1_sample.total_posts < 5`
block, partition and gate. Replace the opening so it reads:

```rust
    // #222: drop posts our English-only models cannot assess, and decide whether
    // the account is scoreable, unassessable, or genuinely sparse — BEFORE the
    // ONNX clean-pass, so a non-English account can't early-exit to Low.
    let (assessable_sample, dropped) = crate::scoring::language::partition_assessable(stage1_sample);
    match crate::scoring::language::coverage_gate(assessable_sample.total_posts, dropped) {
        crate::scoring::language::CoverageOutcome::NotAssessed => {
            return Ok(Stage1Outcome::Terminal(Box::new(AccountScore {
                did: target_did.to_string(),
                handle: target_handle.to_string(),
                toxicity_score: None,
                topic_overlap: None,
                threat_score: None,
                threat_tier: Some(ThreatTier::NotAssessed.as_str().to_string()),
                posts_analyzed: (assessable_sample.total_posts + dropped) as u32,
                top_toxic_posts: vec![],
                scored_at: String::new(),
                behavioral_signals: None,
                context_score: None,
                graph_distance: graph_distance.map(|d| d.as_str().to_string()),
                fingerprint_quality: None,
                scoring_confidence: None,
            })));
        }
        crate::scoring::language::CoverageOutcome::InsufficientData => {
            // Falls through to the existing <5 terminal below (sparse account).
        }
        crate::scoring::language::CoverageOutcome::Score => {}
    }

    // From here down, operate on the assessable-only sample.
    let stage1_sample = &assessable_sample;
```

Ensure `ThreatTier` is imported in `profile.rs` (add `use crate::db::models::ThreatTier;`
if not already present). The subsequent existing body now references the shadowed
`stage1_sample` (assessable-only), so the `<5` Insufficient-Data branch and the
clean-pass both see only assessable posts.

- [ ] **Step 4: Verify**

Run: `cargo test --features web stage1_ 2>&1 | tail -20`
Expected: PASS — new tests pass, existing `stage1_*` tests still pass.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/scoring/profile.rs tests/unit_profile.rs
git commit -m 'feat(222): Stage-1 abstains on unassessable-language accounts

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 7: Stage-2 monolithic integration (`build_profile`)

**Files:**
- Modify: `src/scoring/profile.rs:101-137` (build_profile Stage 2)
- Test: `tests/composition.rs`

**Interfaces:**
- Consumes: `partition_assessable`, `coverage_gate` (Task 2), `NotAssessed` terminal builder (Task 6 pattern).
- Behavior: after fetching 50 posts, unassessable posts never reach
  `classify_batch_with_contexts`; a now-unassessable-dominant account returns a
  terminal `NotAssessed` score.

- [ ] **Step 1: Write the failing test**

Add to `tests/composition.rs` a test that drives `build_profile` (or, if
`build_profile` needs network, assert at the `score_from_sample` seam using a
partitioned sample). Prefer a focused assertion: a 50-post mixed sample partitions
to the English subset and the denominator equals the assessable count.

```rust
#[tokio::test]
async fn stage2_scores_only_assessable_subset() {
    // Construct a PostSample: 6 English originals + 40 Japanese originals.
    // partition_assessable → 6 kept, 40 dropped → coverage_gate → Score.
    // Assert the classifier is invoked with exactly 6 texts (or the denominator
    // in the resulting score reflects 6, not 46).
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web stage2_scores_only_assessable 2>&1 | tail -15`
Expected: FAIL — currently all 46 posts are classified.

- [ ] **Step 3: Implement**

In `build_profile`, immediately after `let sample = posts::fetch_posts_with_replies(client, target_handle, 50).await?;`
(line 103), insert:

```rust
    // #222: partition before classification so unassessable posts never reach the
    // ONNX gate / CoPE-B, and a now-unassessable-dominant account abstains.
    let (sample, dropped) = crate::scoring::language::partition_assessable(&sample);
    if crate::scoring::language::coverage_gate(sample.total_posts, dropped)
        == crate::scoring::language::CoverageOutcome::NotAssessed
    {
        return Ok(AccountScore {
            did: target_did.to_string(),
            handle: target_handle.to_string(),
            toxicity_score: None,
            topic_overlap: None,
            threat_score: None,
            threat_tier: Some(ThreatTier::NotAssessed.as_str().to_string()),
            posts_analyzed: (sample.total_posts + dropped) as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: graph_distance.map(|d| d.as_str().to_string()),
            fingerprint_quality: None,
            scoring_confidence: None,
        });
    }
```

All subsequent references to `sample` (building `all_post_texts`, contexts,
`score_from_sample`) now use the assessable-only, shadowed `sample`.

- [ ] **Step 4: Verify**

Run: `cargo test --features web 2>&1 | tail -15`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/scoring/profile.rs tests/composition.rs
git commit -m 'feat(222): Stage-2 (monolithic) partitions + abstains

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 8: Stage-2 decoupled integration (`gather`)

**Files:**
- Modify: `src/pipeline/scan_phases/gather.rs:262` (after the 50-post fetch)
- Test: `tests/` gather/phase test (follow existing `gather_*` tests using the `PostFetcher`/`CleanPassScorer` doubles)

**Interfaces:**
- Consumes: `partition_assessable`, `coverage_gate` (Task 2); `db.upsert_account_score`; `GatherOutcome::Terminal`.
- Behavior: the decoupled Phase-A path partitions the 50-post sample before building
  `QueueRow`s (so the burst classifier never sees unassessable posts), and writes a
  terminal `NotAssessed` score + returns `GatherOutcome::Terminal` when the gate abstains.

- [ ] **Step 1: Write the failing test**

Add a gather test using the existing canned `PostFetcher` double: feed a 50-post
sample dominated by non-Latin posts, assert `db` received an `AccountScore` with
tier `NotAssessed` and that no pending `QueueRow`s were enqueued.

```rust
#[tokio::test]
async fn gather_abstains_on_unassessable_stage2_sample() {
    // fetch_sample(25) → mixed but proceeds; fetch_sample(50) → unassessable-dominant.
    // Assert: GatherOutcome::Terminal, upserted tier == "NotAssessed", 0 pending rows.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web gather_abstains 2>&1 | tail -15`
Expected: FAIL.

- [ ] **Step 3: Implement**

In `gather_account`, right after `let sample = fetcher.fetch_sample(inputs.account_handle, 50).await?;`
(line 262), insert:

```rust
    // #222: partition before building QueueRows so the burst never classifies
    // unassessable text; abstain if the assessable subset is too thin.
    let (sample, dropped) = crate::scoring::language::partition_assessable(&sample);
    if crate::scoring::language::coverage_gate(sample.total_posts, dropped)
        == crate::scoring::language::CoverageOutcome::NotAssessed
    {
        let score = AccountScore {
            did: inputs.account_did.to_string(),
            handle: inputs.account_handle.to_string(),
            toxicity_score: None,
            topic_overlap: None,
            threat_score: None,
            threat_tier: Some(crate::db::models::ThreatTier::NotAssessed.as_str().to_string()),
            posts_analyzed: (sample.total_posts + dropped) as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: inputs.graph_distance.map(|d| d.as_str().to_string()),
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(user_did, &score).await?;
        return Ok(GatherOutcome::Terminal);
    }
```

Add `use crate::db::models::AccountScore;` to `gather.rs` if not already imported.
The subsequent row-building loop uses the shadowed assessable-only `sample`.

- [ ] **Step 4: Verify**

Run: `cargo test --features web gather 2>&1 | tail -20`
Expected: PASS (new + existing gather tests).

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/pipeline/scan_phases/gather.rs tests/  # stage the specific test file by name
git commit -m 'feat(222): Stage-2 (decoupled gather) partitions + abstains

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 9: markdown report surfacing

**Files:**
- Modify: `src/output/markdown.rs:35-58`
- Test: `src/output/markdown.rs` test module (existing, ~line 236-275)

**Interfaces:**
- Behavior: `NotAssessed` accounts are counted in their own summary row and NOT
  folded into Low.

- [ ] **Step 1: Write the failing test**

Add to the markdown test module:

```rust
#[test]
fn not_assessed_accounts_are_counted_separately_not_as_low() {
    // Build accounts incl. one with threat_tier = Some("NotAssessed").
    // Render; assert the summary contains a "Not assessed" row with count 1
    // AND that the Low count does NOT include it.
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web not_assessed_accounts_are_counted 2>&1 | tail -12`
Expected: FAIL — `low = total - high - elevated - watch` folds it in; no "Not assessed" row.

- [ ] **Step 3: Implement**

In `src/output/markdown.rs`, add a `not_assessed` count and fix `low`:

```rust
    let not_assessed = accounts
        .iter()
        .filter(|a| a.threat_tier.as_deref() == Some("NotAssessed"))
        .count();
    let low = total - high - elevated - watch - not_assessed;
```

Add a summary row after the Low row:

```rust
    writeln!(md, "| Not assessed (unsupported language) | {not_assessed} |")?;
```

> Note: pre-existing "Insufficient Data" accounts remain folded into `low` as they
> are today — out of scope for #222. Only `NotAssessed` is broken out.

- [ ] **Step 4: Verify**

Run: `cargo test --features web 2>&1 | tail -12`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add src/output/markdown.rs
git commit -m 'feat(222): report NotAssessed as its own bucket, not as Low

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 10: web status handler + DB count

**Files:**
- Modify: `src/web/handlers/status.rs:139-165`
- Modify: `src/db/traits.rs` + `src/db/sqlite.rs`/`queries.rs` + `src/db/postgres.rs` (new count method)
- Test: `src/web/handlers/status.rs` test module + a DB test

**Interfaces:**
- Produces: `Database::count_not_assessed(&self, user_did: &str) -> Result<i64>` (counts `threat_tier = 'NotAssessed'`).
- Behavior: `tier_counts` JSON includes `not_assessed`; those rows are counted even
  though `get_ranked_threats(0.0)` filters out their NULL score.

- [ ] **Step 1: Write the failing test**

Add a DB test asserting `count_not_assessed` returns the number of NotAssessed rows,
and a status-handler test asserting the `not_assessed` key is present in `tier_counts`.
(Follow the existing status.rs test doubles.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web count_not_assessed 2>&1 | tail -12`
Expected: FAIL — method does not exist.

- [ ] **Step 3: Add the trait method + impls**

In `src/db/traits.rs`, add to the `Database` trait:

```rust
    /// Count accounts abstained as NotAssessed (unsupported language, #222).
    async fn count_not_assessed(&self, user_did: &str) -> Result<i64>;
```

SQLite impl (in `sqlite.rs`/`queries.rs`, following the `table_count` pattern):

```rust
    async fn count_not_assessed(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND threat_tier = 'NotAssessed'",
            [user_did],
            |r| r.get(0),
        )?;
        Ok(n)
    }
```

Postgres impl (in `postgres.rs`, following its async sqlx pattern):

```rust
    async fn count_not_assessed(&self, user_did: &str) -> Result<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM account_scores WHERE user_did = $1 AND threat_tier = 'NotAssessed'",
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.0)
    }
```

- [ ] **Step 4: Wire into the status handler**

In `status.rs`, after computing high/elevated/watch/low, fetch the count and add it
to the JSON. Keep the `_ => low += 1` loop but exclude NotAssessed from it:

```rust
    for account in &threats {
        match account.threat_tier.as_deref() {
            Some("High") => high += 1,
            Some("Elevated") => elevated += 1,
            Some("Watch") => watch += 1,
            Some("NotAssessed") => {}          // counted separately below
            Some("Insufficient Data") => {}    // pre-existing terminal, not a threat tier
            _ => low += 1,
        }
    }
    let not_assessed = state
        .db
        .count_not_assessed(&auth.effective_did)
        .await
        .unwrap_or(0);
```

Add `"not_assessed": not_assessed,` to the `tier_counts` object. (Because
`get_ranked_threats(0.0)` filters NULL-score rows, the loop won't see NotAssessed
rows anyway — the explicit arm documents intent and guards against a future
threshold change; the authoritative count comes from `count_not_assessed`.)

- [ ] **Step 5: Verify**

Run: `cargo test --features web 2>&1 | tail -12`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add src/web/handlers/status.rs src/db/traits.rs src/db/sqlite.rs src/db/queries.rs src/db/postgres.rs
git commit -m 'feat(222): surface not_assessed count in status API

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 11: dashboard bucket (SvelteKit)

**Files:**
- Modify: the dashboard component that renders `tier_counts` (locate via
  `rg -l "tier_counts|elevated|watch" web/src`)
- Test: manual (SPA) + `npm --prefix web run build`

**Interfaces:**
- Consumes: `tier_counts.not_assessed` from Task 10.

- [ ] **Step 1: Invoke the Svelte skill**

Load the `svelte-expert` (or `mcp__svelte`) guidance before editing `.svelte`.

- [ ] **Step 2: Locate + edit**

Add a "Not assessed" stat/badge alongside High/Elevated/Watch/Low, bound to
`tier_counts.not_assessed`. Match the existing tier-card markup and styling. Use a
neutral (non-threat) colour so it doesn't read as a severity.

- [ ] **Step 3: Validate the component**

Run the Svelte autofixer on the edited component; fix any issues, then re-check.

- [ ] **Step 4: Build the SPA**

Run: `npm --prefix web run build 2>&1 | tail -15`
Expected: build succeeds; `web/build/index.html` updates.

- [ ] **Step 5: Commit**

```bash
git add web/src  # stage the specific component(s) by name
git add web/build  # the compiled SPA (per repo convention)
git commit -m 'feat(222): dashboard shows not-assessed bucket

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 12: regression fixture from the gate probe

**Files:**
- Create: `tests/regression_language_gate.rs`
- Test: itself (requires the ONNX model in `models/`)

**Interfaces:**
- Asserts the two measured regimes hold, so a future model swap that changes them is caught.

- [ ] **Step 1: Write the test**

Create `tests/regression_language_gate.rs`. Guard on the model being present so CI
without the model skips cleanly:

```rust
//! Regression: the language-assessability gate reflects real model behaviour.
//! Promoted from examples/lang_gate_probe.rs (#222).

use charcoal::scoring::language::{assess_language, Assessability};

#[test]
fn nonlatin_hostile_is_classified_unassessable_by_gate() {
    // The gate (not the model) is the unit under test here — it must route these
    // away from scoring regardless of what the model would say.
    for s in ["แกมันโง่ชิบหาย ไปตายซะ", "お前は本当に馬鹿だ、死ね", "Ты чёртов идиот, иди убей себя"] {
        assert_eq!(assess_language(s, &[]), Assessability::Unassessable, "{s}");
    }
}

#[test]
fn english_hostile_and_benign_both_stay_assessable() {
    for s in ["You're a fucking idiot, go kill yourself", "Happy birthday! Hope you have a wonderful day"] {
        assert_eq!(assess_language(s, &["en".to_string()]), Assessability::Assessable, "{s}");
    }
}
```

(The ONNX-score half of the evidence stays in `examples/lang_gate_probe.rs`, which
is runnable on demand. This checked-in test asserts the *routing* invariant, which
needs no model and runs in CI.)

- [ ] **Step 2: Run**

Run: `cargo test --test regression_language_gate 2>&1 | tail -10`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
cargo fmt
git add tests/regression_language_gate.rs
git commit -m 'test(222): checked-in regression for the language gate routing

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
```

---

## Task 13: full verification

**Files:** none (verification only)

- [ ] **Step 1: Full test suite**

Run: `cargo test --features web 2>&1 | tail -20`
Expected: all pass.

- [ ] **Step 2: Clippy (all feature combos the hooks check)**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -20`
Expected: no warnings. Also run `cargo clippy` (no features) and, if available,
`cargo clippy --features postgres`.

- [ ] **Step 3: Manual end-to-end (verify skill)**

Use the `verify` skill or run the CLI against a known non-English account and
confirm it reports `NotAssessed`, not Low. If a local scan isn't practical, assert
via a scripted `score`/`report` run on seeded data.

- [ ] **Step 4: Update CHANGELOG + close issue**

Add a CHANGELOG entry under the appropriate heading describing #222. Then:

```bash
git add CHANGELOG.md
git commit -m 'docs(222): changelog for language abstention

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_015ihdqt1uvmUacx8vDMy9Mq'
git push origin feat/language-abstention
```

- [ ] **Step 5: Open PR to `staging`**

Open a PR `feat/language-abstention` → `staging` summarising the abstention design,
the two spec amendments (Stage-1 seam, NotAssessed/Insufficient-Data split), and the
measurement evidence. Do NOT merge — leave for review.

---

## Notes for the implementer

- **Task ordering:** Tasks 2 and 3 are mutually referential (Task 2's tests use
  `Post.langs` from Task 3). Do Task 1, then Task 3's field addition (Steps 1-2),
  then Task 2, then finish Task 3. Under subagent execution, hand Tasks 2+3 to one
  agent or note the dependency.
- **Terminal `NotAssessed` builder repeats** across Tasks 6/7/8. If review prefers,
  extract a `fn not_assessed_score(did, handle, posts_analyzed, graph_distance) -> AccountScore`
  in `profile.rs` and call it from all three sites (DRY). Left inline here so each
  task is independently reviewable; consolidating in a follow-up cleanup is fine.
- **Column indices** in Task 5 must be read from each specific `SELECT` — they
  differ per query. Do not hardcode without checking the select list.
