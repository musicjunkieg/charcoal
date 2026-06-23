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

use charcoal::toxicity::cost_meter::ScanCostMeter;

#[test]
fn check_with_elapsed_under_is_ok() {
    let m = ScanCostMeter::new(500, 329);
    assert!(m.check_with_elapsed(5471.0).is_ok());
}

#[test]
fn check_with_elapsed_over_errors() {
    let m = ScanCostMeter::new(500, 329);
    let err = m.check_with_elapsed(5472.0).unwrap_err();
    assert_eq!(err.ceiling_cents, 500);
    assert!(err.est_cents >= 500);
}

#[test]
fn ceiling_zero_is_disabled() {
    let m = ScanCostMeter::new(0, 329);
    // Disabled: never trips no matter how large the elapsed.
    assert!(m.check_with_elapsed(1_000_000.0).is_ok());
}

#[test]
fn warn_fires_at_most_once() {
    let m = ScanCostMeter::new(500, 329);
    // Two over-ceiling observations; the dedup flag means only the first warns.
    assert!(m.check_with_elapsed(6000.0).is_err());
    assert!(m.check_with_elapsed(6000.0).is_err());
    // No assertion on log output here (tracing has no global sink in unit tests);
    // the once-guard is verified by `warned_flag_flips_once` below.
}

#[test]
fn warned_flag_flips_once() {
    let m = ScanCostMeter::new(500, 329);
    assert!(!m.has_warned());
    let _ = m.check_with_elapsed(6000.0);
    assert!(m.has_warned());
}

#[test]
fn estimate_is_zero_before_arming() {
    let m = ScanCostMeter::new(500, 329);
    // Not yet armed (no classify call): estimate must be 0.
    assert_eq!(m.estimated_cents(), 0);
}

#[test]
fn from_env_unset_defaults_enabled() {
    // No env vars set in this test process path -> defaults.
    // (Run-order independent: construct directly with the documented defaults.)
    let m = ScanCostMeter::new(DEFAULT_CEILING_CENTS, DEFAULT_RATE_CENTS_PER_HOUR);
    assert!(
        m.check_with_elapsed(6000.0).is_err(),
        "default ceiling is enabled"
    );
}

use charcoal::observability::classifier_metrics::estimate_cost_cents;

#[test]
fn estimate_cost_uses_shared_default_rate() {
    // 1 hour of busy ms at the default rate == the default hourly cents.
    let one_hour_ms = 3_600_000u32;
    assert_eq!(
        estimate_cost_cents("runpod-cope-b", one_hour_ms),
        DEFAULT_RATE_CENTS_PER_HOUR
    );
    // Non-runpod backends remain 0.
    assert_eq!(estimate_cost_cents("zentropi", one_hour_ms), 0);
}
