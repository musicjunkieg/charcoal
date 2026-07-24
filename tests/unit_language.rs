use charcoal::scoring::language::{assess_language, Assessability};

fn en() -> Vec<String> {
    vec!["en".to_string()]
}

#[test]
fn declared_non_english_is_unassessable() {
    // Row 1: any non-en primary tag → Unassessable, regardless of script.
    assert_eq!(
        assess_language("hello world", &["pt".to_string()]),
        Assessability::Unassessable
    );
}

#[test]
fn declared_english_latin_is_assessable() {
    // Row 2.
    assert_eq!(
        assess_language("you are terrible", &en()),
        Assessability::Assessable
    );
}

#[test]
fn declared_english_but_nonlatin_is_unassessable() {
    // Row 3: the measured 9.1% mis-declaration — trust the script, not the tag.
    assert_eq!(
        assess_language("ไปตายซะ นะไอ้โง่", &en()),
        Assessability::Unassessable
    );
}

#[test]
fn empty_langs_latin_is_assessable() {
    // Row 4: script-only fallback.
    assert_eq!(
        assess_language("you are terrible", &[]),
        Assessability::Assessable
    );
}

#[test]
fn empty_langs_nonlatin_is_unassessable() {
    // Row 5.
    assert_eq!(
        assess_language("お前は本当に馬鹿だ、死ね", &[]),
        Assessability::Unassessable
    );
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
    assert_eq!(
        assess_language("you are terrible", &["en-US".to_string()]),
        Assessability::Assessable
    );
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

// `extract_langs_strips_region_and_lowercases` previously lived here as an
// indirect check via `assess_language`, but it only ever fed an
// already-normalized "en" tag in — it never called `extract_langs` and never
// exercised a region-tagged or uppercase input, so it had no real coverage
// of the normalization behavior. Superseded by
// `bluesky::posts::tests::extract_langs_strips_region_and_lowercases` in
// src/bluesky/posts.rs, which calls `extract_langs` directly against
// "en-US" / "ZH-Hans" (#222).

// Task 2 tests for coverage_gate and partition_assessable.
use charcoal::bluesky::posts::{Post, PostSample, ReplyPost};
use charcoal::scoring::language::{coverage_gate, partition_assessable, CoverageOutcome};

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
