use charcoal::toxicity::cost_meter::{
    over_ceiling, DEFAULT_CEILING_CENTS, DEFAULT_RATE_CENTS_PER_HOUR,
};

// At ceiling=500c, rate=329c/hr the trip point is elapsed = 500*3600/329 = 5471.13s.
// `>=` semantics: false just under, true just over.
#[test]
fn over_ceiling_boundary() {
    assert!(!over_ceiling(5471.0, 329, 500), "just under must not trip");
    assert!(over_ceiling(5472.0, 329, 500), "just over must trip");
}

#[test]
fn over_ceiling_zero_elapsed_never_trips() {
    assert!(!over_ceiling(0.0, 329, 500));
}

#[test]
fn defaults_are_500_and_329() {
    assert_eq!(DEFAULT_CEILING_CENTS, 500);
    assert_eq!(DEFAULT_RATE_CENTS_PER_HOUR, 329);
}
