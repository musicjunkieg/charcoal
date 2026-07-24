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
