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
