//! Long-term memory: one plain markdown file of facts, `memory.md` in the
//! data directory.
//!
//! Nothing clever here on purpose: the model "remembers" by appending a line
//! with the `remember` tool, and the whole file is injected into the system
//! prompt on every conversation turn. Good enough for years of personal facts.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How much of the memory file may go into the system prompt. If the file
/// grows beyond this, only the newest facts are kept (older ones stay on
/// disk, just not in the prompt).
const PROMPT_CAP: usize = 6000;

pub fn file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("memory.md")
}

/// The whole memory file, or a fresh header if it does not exist yet.
pub fn load(data_dir: &Path) -> String {
    std::fs::read_to_string(file_path(data_dir))
        .unwrap_or_else(|_| "# Facts dumi remembers about its owner\n\n".to_string())
}

/// Append one fact with a timestamp. Called by the `remember` tool.
pub fn append(data_dir: &Path, fact: &str) -> Result<()> {
    let path = file_path(data_dir);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M");
    writeln!(file, "- [{stamp}] {}", fact.trim())
        .with_context(|| format!("cannot write to {}", path.display()))
}

/// Facts for the `recall` tool. With a query, only matching lines are
/// returned (simple substring match, case-insensitive).
pub fn recall(data_dir: &Path, query: Option<&str>) -> String {
    let all = load(data_dir);
    let Some(query) = query else {
        return crate::util::truncate(&all, PROMPT_CAP);
    };
    let query = query.to_lowercase();
    let hits: Vec<&str> = all
        .lines()
        .filter(|line| line.to_lowercase().contains(&query))
        .collect();
    if hits.is_empty() {
        format!("no memories matching '{query}'")
    } else {
        hits.join("\n")
    }
}

/// The memory block for the system prompt (capped, newest facts win).
pub fn for_prompt(data_dir: &Path) -> String {
    let all = load(data_dir);
    if all.chars().count() <= PROMPT_CAP {
        return all;
    }
    format!(
        "(older memories trimmed)\n{}",
        crate::util::tail_chars(&all, PROMPT_CAP)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh unique temp directory for one test (tests run in parallel).
    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dumi-test-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn append_then_load_round_trip() {
        let dir = tmp("memory");
        append(&dir, "owner prefers plain text").unwrap();
        append(&dir, "owner's cat is called Pixel").unwrap();
        let loaded = load(&dir);
        assert!(loaded.contains("owner prefers plain text"));
        assert!(loaded.contains("Pixel"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recall_filters_by_substring() {
        let dir = tmp("recall");
        append(&dir, "owner prefers plain text").unwrap();
        append(&dir, "owner's cat is called Pixel").unwrap();
        let hits = recall(&dir, Some("cat"));
        assert!(hits.contains("Pixel"));
        assert!(!hits.contains("plain text"));
        assert!(recall(&dir, Some("nothing")).starts_with("no memories"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
