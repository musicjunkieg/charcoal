//! Regression: the language-assessability gate reflects real model behaviour.
//! Promoted from examples/lang_gate_probe.rs (#222).

use charcoal::scoring::language::{assess_language, Assessability};

#[test]
fn nonlatin_hostile_is_classified_unassessable_by_gate() {
    // The gate (not the model) is the unit under test here — it must route these
    // away from scoring regardless of what the model would say.
    for s in [
        "แกมันโง่ชิบหาย ไปตายซะ",
        "お前は本当に馬鹿だ、死ね",
        "Ты чёртов идиот, иди убей себя",
    ] {
        assert_eq!(assess_language(s, &[]), Assessability::Unassessable, "{s}");
    }
}

#[test]
fn english_hostile_and_benign_both_stay_assessable() {
    for s in [
        "You're a fucking idiot, go kill yourself",
        "Happy birthday! Hope you have a wonderful day",
    ] {
        assert_eq!(
            assess_language(s, &["en".to_string()]),
            Assessability::Assessable,
            "{s}"
        );
    }
}

// --- #230: the gate inside score_pair, against the real model ---

/// Requires the NLI model to be readable at `CHARCOAL_MODEL_DIR` (or the
/// platform default). If it is not, this SKIPS and asserts nothing — a green
/// run is then not evidence the gate works. Under the Safehouse sandbox the
/// models live outside the project directory and need an explicit read grant;
/// as of 2026-07-26 that grant is in place, so this test should genuinely run.
/// Check with `-- --show-output`: libtest swallows the SKIP notice on a plain
/// run.
#[tokio::test]
async fn score_pair_abstains_on_nonlatin_but_still_scores_english() {
    let base = charcoal::toxicity::download::resolve_model_dir();
    if !charcoal::toxicity::download::nli_files_present(&base) {
        eprintln!(
            "SKIP: NLI model not reachable at {} — THIS TEST ASSERTED NOTHING. \
             Run `charcoal download-model`, and ensure the path is readable \
             from the sandbox.",
            base.display()
        );
        return;
    }
    let scorer = charcoal::scoring::nli::NliScorer::load(&base).expect("load NLI model");

    // Non-Latin response → abstain.
    let abstained = scorer
        .score_pair(
            "fat people deserve healthcare too",
            "お前は本当に馬鹿だ、死ね",
        )
        .await
        .expect("abstention is not an error");
    assert!(
        abstained.is_none(),
        "non-Latin response must abstain, got {abstained:?}"
    );

    // Non-Latin original → abstain. The protected user's own text is never
    // language-filtered upstream, so this side must be gated too.
    let abstained_orig = scorer
        .score_pair("แกมันโง่ชิบหาย ไปตายซะ", "that is a terrible take")
        .await
        .expect("abstention is not an error");
    assert!(
        abstained_orig.is_none(),
        "non-Latin original must abstain, got {abstained_orig:?}"
    );

    // CONTROL — an all-English pair MUST still score. Without this assertion a
    // gate that abstained on everything would pass silently, which is exactly
    // the failure mode that motivated writing it down.
    let scored = scorer
        .score_pair(
            "fat people deserve healthcare too",
            "lol imagine being that big",
        )
        .await
        .expect("English pair must not error");
    let (score, _hypotheses) = scored.expect("English pair must score, not abstain");
    assert!(
        (0.0..=1.0).contains(&score),
        "hostility score out of range: {score}"
    );
}
