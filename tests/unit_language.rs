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
