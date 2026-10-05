//! #400 — a batched toxicity score must equal the same text scored alone.
//!
//! The Detoxify `unbiased-toxic-roberta` export emits `logits[batch, 16]`:
//! the 7 toxicity heads Charcoal reads, then 9 identity heads. `score_batch`
//! used to step through that flat buffer 7 values per row, so row 0 was right
//! and every later row was read out of the middle of its neighbours' logits —
//! noise. Stage 1 and the Stage-2 clean pass both batch an account's posts, so
//! a hostile post anywhere but first could read as clean.
//!
//! No test compared a batch with solo scores before: the session-pool test
//! compares batch to batch, which is equally wrong on both sides and so agreed.
//!
//! Model-gated: prints the `SKIP:` sentinel and asserts nothing when the
//! model is absent — run with `CHARCOAL_MODEL_DIR=./models`.

use charcoal::toxicity::onnx::OnnxToxicityScorer;
use charcoal::toxicity::traits::ToxicityScorer;
use std::path::PathBuf;

fn model_dir() -> Option<PathBuf> {
    let base = std::env::var("CHARCOAL_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| charcoal::toxicity::download::default_model_dir());
    charcoal::toxicity::download::model_files_present(&base).then_some(base)
}

const HOSTILE: &str = "I hope you die, you stupid bitch";
const BENIGN: &str = "I spent the afternoon repotting my tomato plants in the garden.";
const BENIGN_LONG: &str = "We walked along the river after lunch, watched the herons for a \
     while, and then stopped at the little bakery near the bridge for coffee and bread.";

/// Every text in the batch must score what it scores on its own — the
/// toxicity value AND each attribute the result carries.
async fn assert_batch_matches_solo(scorer: &OnnxToxicityScorer, texts: &[&str]) {
    let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
    let batch = scorer.score_batch(&owned).await.unwrap();
    assert_eq!(batch.len(), texts.len());
    for (i, (text, got)) in texts.iter().zip(&batch).enumerate() {
        let solo = scorer.score_text(text).await.unwrap();
        // Toxicity — the score both clean-pass gates compare to 0.10 — must
        // match to 1e-3; the stride bug moved it by ~0.9 (0.999 -> 0.110).
        let close = |a: f64, b: f64| (a - b).abs() < 1e-3;
        // The attribute heads get a looser bound, measured, not guessed: the
        // RAW quantized model (python onnxruntime 1.30, same file, outside
        // Charcoal) moves one attribute head by up to 0.078 for a padded row
        // in a mixed-length batch, while its toxicity head stays identical.
        // That is the dynamic-quantization batch coupling #231 found in the NLI
        // model, not this bug, and it is tracked separately. 0.1 still catches
        // the stride bug, which moved attributes by far more.
        let near = |a: f64, b: f64| (a - b).abs() < 0.1;
        assert!(
            close(got.toxicity, solo.toxicity),
            "row {i} ({text:?}): batched toxicity {} != solo {}",
            got.toxicity,
            solo.toxicity
        );
        for (name, g, s) in [
            (
                "severe_toxicity",
                got.attributes.severe_toxicity,
                solo.attributes.severe_toxicity,
            ),
            (
                "identity_attack",
                got.attributes.identity_attack,
                solo.attributes.identity_attack,
            ),
            ("insult", got.attributes.insult, solo.attributes.insult),
            ("threat", got.attributes.threat, solo.attributes.threat),
        ] {
            let (g, s) = (g.unwrap_or_default(), s.unwrap_or_default());
            assert!(
                near(g, s),
                "row {i} ({text:?}): batched {name} {g} != solo {s}"
            );
        }
    }
}

#[tokio::test]
async fn identical_texts_score_identically_at_every_batch_position() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: identical_texts_score_identically — toxicity model not present; THIS TEST ASSERTED NOTHING");
        return;
    };
    let scorer = OnnxToxicityScorer::load(&dir).unwrap();
    // No padding at all: if these diverge, the rows are being read wrongly.
    assert_batch_matches_solo(&scorer, &[HOSTILE, HOSTILE, HOSTILE, HOSTILE]).await;
}

#[tokio::test]
async fn a_hostile_post_after_row_zero_is_not_read_as_clean() {
    let Some(dir) = model_dir() else {
        eprintln!("SKIP: a_hostile_post_after_row_zero — toxicity model not present; THIS TEST ASSERTED NOTHING");
        return;
    };
    let scorer = OnnxToxicityScorer::load(&dir).unwrap();
    let texts = [BENIGN, BENIGN_LONG, HOSTILE, BENIGN, HOSTILE];
    assert_batch_matches_solo(&scorer, &texts).await;

    // The behaviour that matters downstream: the 0.10 clean-pass threshold.
    let owned: Vec<String> = texts.iter().map(|t| t.to_string()).collect();
    let batch = scorer.score_batch(&owned).await.unwrap();
    assert!(
        batch[2].toxicity > 0.5,
        "hostile row 2 read as {}",
        batch[2].toxicity
    );
    assert!(
        batch[4].toxicity > 0.5,
        "hostile row 4 read as {}",
        batch[4].toxicity
    );
    assert!(
        batch[1].toxicity < 0.10,
        "benign row 1 read as {}",
        batch[1].toxicity
    );
}
