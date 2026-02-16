// Output formatting — terminal display and report generation.

pub mod markdown;
pub mod terminal;

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
