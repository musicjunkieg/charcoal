use charcoal::pipeline::scan_phases::staging::{
    AccountInput, ScanPhase, ACCOUNT_INPUT_SCHEMA_VERSION,
};

#[test]
fn scan_phase_roundtrips_through_str() {
    for p in [
        ScanPhase::Gather,
        ScanPhase::Burst,
        ScanPhase::Finalize,
        ScanPhase::Done,
    ] {
        assert_eq!(ScanPhase::from_value(p.as_str()), Some(p));
    }
    assert_eq!(ScanPhase::from_value("nonsense"), None);
}

#[test]
fn account_input_is_versioned_and_roundtrips() {
    let blob = AccountInput::new_for_test();
    let json = serde_json::to_string(&blob).unwrap();
    let back: AccountInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
    assert_eq!(back, blob);
}
