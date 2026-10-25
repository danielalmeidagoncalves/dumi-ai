//! Scheduled jobs (reminders, recurring tasks), persisted as one JSON file:
//! `jobs.json` in the data directory.
//!
//! Design: read the whole file, change it in memory, write it back. For a
//! single-user personal assistant this is all the "database" we need, and it
//! survives restarts.

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    /// Unique id (timestamp-based — plenty unique for one user and one process).
    pub id: String,
    /// What the agent should do when the job fires.
    pub title: String,
    /// When the job fires next (RFC 3339, UTC).
    pub at: DateTime<Utc>,
    /// If set: a cron expression — the job re-schedules itself after firing.
    pub recur: Option<String>,
}

impl Job {
    /// One human-readable line, used by `list_reminders` and logs.
    pub fn describe(&self) -> String {
        let local: DateTime<Local> = self.at.into();
        match &self.recur {
            Some(cron) => format!(
                "[{}] {} (recurring: {cron}, next {})",
                self.id,
                self.title,
                local.format("%Y-%m-%d %H:%M")
            ),
            None => format!(
                "[{}] {} (at {})",
                self.id,
                self.title,
                local.format("%Y-%m-%d %H:%M")
            ),
        }
    }
}

pub fn file_path(data_dir: &Path) -> PathBuf {
    data_dir.join("jobs.json")
}

/// All stored jobs; an empty list if the file is missing or unreadable
/// (dumi should still start and work if its jobs file got corrupted).
pub fn load(data_dir: &Path) -> Vec<Job> {
    match std::fs::read_to_string(file_path(data_dir)) {
        Ok(raw) => match serde_json::from_str(&raw) {
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::warn!("ignoring unreadable {}: {e}", file_path(data_dir).display());
                Vec::new()
            }
        },
        Err(_) => Vec::new(),
    }
}

pub fn save(data_dir: &Path, jobs: &[Job]) -> Result<()> {
    let path = file_path(data_dir);
    let raw = serde_json::to_string_pretty(jobs).context("cannot encode jobs as JSON")?;
    std::fs::write(&path, raw).with_context(|| format!("cannot write {}", path.display()))
}

/// A new unique job id.
pub fn new_id() -> String {
    format!("job-{}", Utc::now().timestamp_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn save_load_round_trip() {
        let dir = tmp("jobs");
        let jobs = vec![
            Job {
                id: "job-1".into(),
                title: "stretch".into(),
                at: DateTime::parse_from_rfc3339("2026-10-26T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                recur: None,
            },
            Job {
                id: "job-2".into(),
                title: "standup".into(),
                at: DateTime::parse_from_rfc3339("2026-10-27T08:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
                recur: Some("0 8 * * *".into()),
            },
        ];
        save(&dir, &jobs).unwrap();
        let loaded = load(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].recur.as_deref(), Some("0 8 * * *"));
        assert!(loaded[0].describe().contains("stretch"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_empty_list() {
        let dir = tmp("jobs-missing");
        assert!(load(&dir).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
