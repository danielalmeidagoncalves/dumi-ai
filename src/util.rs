//! Small helpers shared by several modules.

use std::path::{Path, PathBuf};

/// Shorten `text` to at most `max` characters, with a note about what was cut.
/// Used for anything we send back to the model (command output, big files...),
/// because models have input size limits.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    format!(
        "{cut}\n…[truncated {} more characters]",
        text.chars().count() - max
    )
}

/// Like [`truncate`], but keeps the *end* of the text (used for memory: the
/// newest facts are at the bottom of the file). Returns a slice of the
/// original string when possible.
pub fn tail_chars(text: &str, max: usize) -> &str {
    if text.chars().count() <= max {
        return text;
    }
    let skip = text.chars().count() - max;
    // `char_indices` gives us byte offsets, so we never cut in the middle of
    // a multi-byte character (which would panic).
    match text.char_indices().nth(skip) {
        Some((byte_offset, _)) => &text[byte_offset..],
        None => text,
    }
}

/// Expand a leading `~/` to the user's home directory. Paths the model sends
/// us often look like `~/notes.txt`, so file tools run this first.
pub fn expand_tilde(path: &str) -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => expand_tilde_with(path, Path::new(&home)),
        // No HOME at all: leave the path untouched, like a shell would.
        None => PathBuf::from(path),
    }
}

/// The testable core of [`expand_tilde`]: the home directory is passed in
/// instead of read from $HOME. (Mutating environment variables is unsafe in
/// Rust 2024 because tests run in parallel threads, so we keep tests away
/// from the real environment.)
fn expand_tilde_with(path: &str, home: &Path) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_leaves_short_text_alone() {
        assert_eq!(truncate("hello", 10), "hello");
        assert!(truncate("hello", 3).starts_with("hel"));
        // Unicode is counted in characters, not bytes.
        let emoji = "🙂".repeat(10);
        assert!(truncate(&emoji, 3).starts_with("🙂🙂🙂"));
    }

    #[test]
    fn tail_chars_keeps_the_end() {
        assert_eq!(tail_chars("abcdef", 3), "def");
        assert_eq!(tail_chars("abc", 10), "abc");
        // Cutting a multi-byte character must not panic and must not split it.
        let text = "ab🙂cd";
        assert!(tail_chars(text, 2).starts_with("c"));
    }

    #[test]
    fn expand_tilde_with_prefix() {
        let home = Path::new("/home/tester");
        assert_eq!(
            expand_tilde_with("~/x/y.txt", home),
            PathBuf::from("/home/tester/x/y.txt")
        );
        assert_eq!(
            expand_tilde_with("/tmp/abs.txt", home),
            PathBuf::from("/tmp/abs.txt")
        );
        // Bare "~" (no slash) is left alone.
        assert_eq!(expand_tilde_with("~", home), PathBuf::from("~"));
    }
}
