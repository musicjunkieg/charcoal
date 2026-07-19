// Output formatting — terminal display and report generation.

pub mod markdown;
pub mod terminal;

use std::io::IsTerminal;
use std::sync::LazyLock;

/// Decide whether human-facing progress output should be emitted (#226).
///
/// Split from the IO so the policy is unit-testable; `progress_enabled()` does
/// the actual terminal detection and env lookup.
///
/// `override_var` is the raw `CHARCOAL_PROGRESS` value. An unrecognized value
/// deliberately falls back to terminal detection rather than picking a side — a
/// typo must not silently mute output someone relies on, nor silently re-enable
/// the flood in production.
pub fn should_show_progress(stdout_is_terminal: bool, override_var: Option<&str>) -> bool {
    match override_var
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("always") | Some("1") | Some("true") => true,
        Some("never") | Some("0") | Some("false") => false,
        _ => stdout_is_terminal,
    }
}

/// Whether to emit the progress display. Evaluated once — neither stdout's
/// terminal-ness nor the env var changes during a run.
pub fn progress_enabled() -> bool {
    static ENABLED: LazyLock<bool> = LazyLock::new(|| {
        should_show_progress(
            std::io::stdout().is_terminal(),
            std::env::var("CHARCOAL_PROGRESS").ok().as_deref(),
        )
    });
    *ENABLED
}

/// `println!` for human-facing progress, suppressed when stdout is not a
/// terminal.
///
/// Exists because the scan pipeline is shared between the CLI and the web
/// server. In a shell this is useful progress; in the server it went to
/// Railway's log ingest and blew the 500 logs/sec limit, taking an unknown
/// number of WARN lines down with it (#226). `RUST_LOG` cannot filter these —
/// they are `println!`, not `tracing`.
///
/// Use for display only. Anything diagnostic must stay a real `tracing` event.
#[macro_export]
macro_rules! progress {
    ($($arg:tt)*) => {
        if $crate::output::progress_enabled() {
            println!($($arg)*);
        }
    };
}

/// Truncate a string to at most `max_chars` characters, appending "..." if truncated.
///
/// Unlike byte slicing (`&text[..120]`), this respects UTF-8 character boundaries
/// and will never panic on multi-byte characters like emoji or accented letters.
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_chars).collect();
        format!("{truncated}...")
    }
}
