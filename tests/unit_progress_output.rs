//! Tests for the progress-output gate (#226).
//!
//! Background: `src/pipeline/{amplification,sweep}.rs` emit human-facing
//! progress with `println!`. That is genuinely useful when a person runs
//! `charcoal scan` in a shell, and is pure noise when the SAME pipeline code
//! runs inside the web server — where it went to Railway's log ingest and blew
//! the 500 logs/sec limit ("Messages dropped: 1156"), silently discarding an
//! unknown number of WARN lines alongside it.
//!
//! Note `RUST_LOG` cannot solve this: these are `println!`, not `tracing`, so
//! no log filter reaches them. Hence an explicit gate.

use charcoal::output::should_show_progress;
use std::io::IsTerminal;

#[test]
fn defaults_to_terminal_detection() {
    // A shell (CLI) gets the progress display.
    assert!(should_show_progress(true, None));
    // A pipe or a container's captured stdout (the server) does not.
    assert!(!should_show_progress(false, None));
}

#[test]
fn override_can_force_progress_on_when_not_a_terminal() {
    // Escape hatch for piping CLI output to a file deliberately.
    for v in ["always", "1", "true", "TRUE", "  always  "] {
        assert!(
            should_show_progress(false, Some(v)),
            "{v:?} should force progress on"
        );
    }
}

#[test]
fn override_can_force_progress_off_in_a_terminal() {
    for v in ["never", "0", "false", "FALSE"] {
        assert!(
            !should_show_progress(true, Some(v)),
            "{v:?} should force progress off"
        );
    }
}

/// Verifies the actual runtime wiring, not just the policy function. Under
/// `cargo test` stdout is captured rather than a terminal — the same shape as
/// the server — so the gate must resolve to false. This is what proves the
/// pipeline's progress output is genuinely suppressed off-terminal.
#[test]
fn progress_is_disabled_when_stdout_is_not_a_terminal() {
    if std::env::var("CHARCOAL_PROGRESS").is_ok() {
        eprintln!("SKIP: CHARCOAL_PROGRESS is set, which overrides detection");
        return;
    }
    // Under `--nocapture`, or any run whose stdout is attached to a terminal,
    // the gate correctly resolves TRUE and this assertion would be wrong. Skip
    // rather than assert — the policy itself stays covered by the
    // should_show_progress tests, which inject the terminal flag directly.
    if std::io::stdout().is_terminal() {
        eprintln!("SKIP: stdout is a terminal, so the gate is correctly enabled");
        return;
    }
    assert!(
        !charcoal::output::progress_enabled(),
        "stdout is captured under cargo test, so progress must be off — \
         if this fails the pipeline would still flood the server logs"
    );
}

#[test]
fn unrecognized_override_falls_back_to_terminal_detection() {
    // A typo must not silently disable output that someone is relying on, nor
    // silently enable the flood in production. Fall back to the default.
    assert!(should_show_progress(true, Some("yes-please")));
    assert!(!should_show_progress(false, Some("yes-please")));
    assert!(!should_show_progress(false, Some("")));
}
