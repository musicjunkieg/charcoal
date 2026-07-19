//! Regression tests for #220 — the toxicity ONNX scorer must not blow up on
//! input that tokenizes past the model's position-embedding limit.
//!
//! Background: Bluesky caps a post at 300 GRAPHEMES, not tokens. For Latin
//! ASCII that is ~63 tokens, but a single 300-grapheme post measures 602 tokens
//! in emoji, 902 in CJK, and 4202 in ZWJ family emoji — all far past the 512
//! that unbiased-toxic-roberta can accept. Before the fix, `session.run`
//! hard-errored ("invalid expand shape" at the /roberta/Expand node) and, because
//! scoring is batched per account, ONE over-long post took the ENTIRE batch with
//! it — so gather skipped the whole account and the scan silently went degraded.
//! That dropped 34 accounts on the 2026-07-19 staging scan, selected by script
//! rather than at random.
//!
//! These tests are model-gated: they need the real toxicity model, since the
//! bug lives in the interaction between tokenizer output and the ONNX graph and
//! cannot be reproduced with a stub.

use charcoal::toxicity::traits::ToxicityScorer;
use std::path::PathBuf;

/// Resolve the model dir the same way the golden tests do, and skip when the
/// toxicity model is absent (CI without `charcoal download-model`).
fn model_dir_or_skip(case: &str) -> Option<PathBuf> {
    let base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

    if charcoal::toxicity::download::model_files_present(&base) {
        Some(base)
    } else {
        eprintln!(
            "SKIP {case}: toxicity model not present at {base:?} — \
             run `charcoal download-model` to enable this test"
        );
        None
    }
}

/// A full-length (300-grapheme) Bluesky post in CJK. Measured at 902 tokens,
/// comfortably past the 512 limit.
fn overlong_cjk_post() -> String {
    "漢".repeat(300)
}

#[tokio::test]
async fn scores_a_post_that_tokenizes_past_the_model_limit() {
    let Some(dir) = model_dir_or_skip("overlong single post") else {
        return;
    };
    let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
        .expect("toxicity model should load when files are present");

    // Before the fix this returned Err("invalid expand shape").
    let result = scorer.score_text(&overlong_cjk_post()).await;

    assert!(
        result.is_ok(),
        "over-long input must be truncated and scored, not error: {:?}",
        result.err()
    );
}

#[tokio::test]
async fn one_overlong_post_does_not_poison_the_rest_of_the_batch() {
    let Some(dir) = model_dir_or_skip("batch poisoning") else {
        return;
    };
    let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
        .expect("toxicity model should load when files are present");

    // This is the real gather shape: an account's posts scored as one batch.
    // A single over-long post used to fail the whole call, losing the account
    // entirely — including all of its perfectly scoreable short posts.
    let batch = vec![
        "you are a wonderful person".to_string(),
        overlong_cjk_post(),
        "thanks for sharing this".to_string(),
    ];

    let results = scorer
        .score_batch(&batch)
        .await
        .expect("a batch containing an over-long post must still score");

    assert_eq!(
        results.len(),
        batch.len(),
        "every input must get a result, including the truncated one"
    );

    // The benign neighbours must still score benign — truncation must not
    // corrupt the other rows' positions in the batch.
    assert!(
        results[0].toxicity < 0.5,
        "benign post before the long one scored {:.4}",
        results[0].toxicity
    );
    assert!(
        results[2].toxicity < 0.5,
        "benign post after the long one scored {:.4}",
        results[2].toxicity
    );
}

#[tokio::test]
async fn emoji_heavy_english_is_also_covered() {
    let Some(dir) = model_dir_or_skip("emoji-heavy english") else {
        return;
    };
    let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
        .expect("toxicity model should load when files are present");

    // This is NOT a non-English case: ZWJ family emoji cost ~14 tokens each, so
    // ordinary English text with enough of them clears 512 on its own. Guards
    // against "we only support English" being mistaken for "we are unaffected".
    let text = format!("great news for everyone {}", "👩‍👩‍👧‍👦".repeat(60));

    let result = scorer.score_text(&text).await;

    assert!(
        result.is_ok(),
        "emoji-heavy English must be truncated and scored, not error: {:?}",
        result.err()
    );
}

/// The subtle one: WHERE we truncate decides whether the fix is correct or
/// merely non-crashing.
///
/// Replies are scored as the envelope "[Parent post]: <p>\n\n[Reply]: <r>".
/// Default (right) truncation cuts from the END, which eats the reply — the
/// exact text being judged — while preserving the parent nobody is scoring.
/// That would be silently wrong in a way the crash at least wasn't: every
/// over-long reply would score as its own benign parent context.
///
/// This asserts the reply survives, by making the reply the ONLY thing that
/// differs between two otherwise-identical over-long envelopes.
#[tokio::test]
async fn truncation_preserves_the_reply_not_the_parent() {
    let Some(dir) = model_dir_or_skip("truncation direction") else {
        return;
    };
    let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
        .expect("toxicity model should load when files are present");

    // A parent long enough to force truncation on its own (~900 tokens), so the
    // envelope cannot fit and the truncation path definitely fires.
    let long_parent = overlong_cjk_post();

    let hostile = charcoal::toxicity::format_parent_reply(
        &long_parent,
        "you are a worthless idiot and everyone hates you",
    );
    let benign = charcoal::toxicity::format_parent_reply(
        &long_parent,
        "this is a lovely point, thank you for making it",
    );

    let hostile_score = scorer
        .score_text(&hostile)
        .await
        .expect("over-long hostile envelope must score")
        .toxicity;
    let benign_score = scorer
        .score_text(&benign)
        .await
        .expect("over-long benign envelope must score")
        .toxicity;

    // If truncation dropped the reply, both envelopes would collapse to the same
    // surviving parent text and score identically.
    assert!(
        hostile_score > benign_score,
        "the reply must survive truncation and drive the score: \
         hostile={hostile_score:.4} benign={benign_score:.4} — \
         near-identical scores mean the reply was truncated away"
    );
}

#[tokio::test]
async fn short_input_still_scores_normally() {
    let Some(dir) = model_dir_or_skip("short-input control") else {
        return;
    };
    let scorer = charcoal::toxicity::onnx::OnnxToxicityScorer::load(&dir)
        .expect("toxicity model should load when files are present");

    // Control: the truncation path must not change behaviour for ordinary input.
    let result = scorer
        .score_text("you are a wonderful person")
        .await
        .expect("short input must score");

    assert!(
        result.toxicity < 0.5,
        "benign short text scored {:.4}",
        result.toxicity
    );
}

/// The toxicity clean-pass runs BEFORE the embedder in gather, so the #220
/// crash was short-circuiting this call and masking whether MiniLM shares the
/// same missing-truncation bug — fixing toxicity alone would merely relocate the
/// crash if it did.
///
/// Verified empirically that it does NOT: MiniLM tolerates over-long input where
/// RoBERTa hard-errors, so no truncation config was added there. Kept as a
/// regression guard, since the toxicity fix is what removes the short-circuit
/// that has been hiding this path in production.
///
/// SCOPE: this asserts the call does not ERROR. It does NOT assert the resulting
/// embedding is meaningful for over-long input — that is unmeasured.
#[tokio::test]
async fn embedder_also_survives_overlong_input() {
    let base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());

    if !charcoal::toxicity::download::embedding_files_present(&base) {
        eprintln!("SKIP embedder over-long: embedding model not present at {base:?}");
        return;
    }

    // Unlike NliScorer::load, this one takes the already-appended subdirectory.
    let embed_dir = charcoal::toxicity::download::embedding_model_dir(&base);
    let embedder = charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir)
        .expect("embedding model should load when files are present");

    let result = embedder
        .embed_batch(&["a normal english sentence".to_string(), overlong_cjk_post()])
        .await;

    assert!(
        result.is_ok(),
        "embedder must handle over-long input — the toxicity fix removes the \
         short-circuit that was hiding this: {:?}",
        result.err()
    );
}
