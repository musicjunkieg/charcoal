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

    let originals: Vec<Post> = sample
        .originals
        .iter()
        .filter(|p| keep_post(p))
        .cloned()
        .collect();
    let replies: Vec<ReplyPost> = sample
        .replies
        .iter()
        .filter(|r| keep_post(&r.post))
        .cloned()
        .collect();
    let quotes: Vec<Post> = sample
        .quotes
        .iter()
        .filter(|p| keep_post(p))
        .cloned()
        .collect();

    let original_count = sample.originals.len() + sample.replies.len() + sample.quotes.len();
    let kept = originals.len() + replies.len() + quotes.len();
    let dropped = original_count - kept;

    let non_repost = kept as f64;
    let reply_ratio = if non_repost > 0.0 {
        replies.len() as f64 / non_repost
    } else {
        0.0
    };
    let quote_ratio = if non_repost > 0.0 {
        quotes.len() as f64 / non_repost
    } else {
        0.0
    };

    (
        PostSample {
            originals,
            replies,
            quotes,
            reply_ratio,
            quote_ratio,
            total_posts: kept,
        },
        dropped,
    )
}
