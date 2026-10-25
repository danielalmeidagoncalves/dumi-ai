//! Configuration: one small TOML file at `~/.config/dumi/config.toml`.
//!
//! Deliberately flat and tiny. If a required field is missing, dumi refuses
//! to start with a message that says exactly what to fix.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Model/provider settings. `[model]` section in the config file.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelConfig {
    /// Base URL of any OpenAI-compatible API, e.g. "https://api.openai.com/v1".
    pub base_url: String,
    pub api_key: String,
    /// Model name the provider understands, e.g. "gpt-4o-mini".
    pub model: String,
}

/// Behaviour of the agent itself. `[agent]` section — every field optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)] // missing fields are filled from Default below
pub struct AgentConfig {
    /// What the assistant calls itself.
    pub name: String,
    /// Where dumi keeps its files (memory, scheduled jobs). "~" is allowed.
    pub data_dir: PathBuf,
    /// How often the agent wakes up on its own to review its notes.
    /// A standard 5-field cron expression.
    pub heartbeat_cron: String,
    /// Jami account hash of the owner. Once the Jami channel exists, dumi
    /// only answers this account — important, because the agent can run
    /// shell commands.
    pub owner_jami_id: Option<String>,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "dumi".to_string(),
            data_dir: home().join(".local/share/dumi"),
            heartbeat_cron: "*/30 * * * *".to_string(),
            owner_jami_id: None,
        }
    }
}

/// The whole configuration file.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub model: ModelConfig,
    #[serde(default)]
    pub agent: AgentConfig,
}

impl Config {
    /// Default config path: `${XDG_CONFIG_HOME:-$HOME/.config}/dumi/config.toml`.
    pub fn default_path() -> PathBuf {
        config_home().join("dumi/config.toml")
    }

    /// Load from the default path.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::default_path())
    }

    /// Load from a specific file (kept separate so tests can pass any path).
    pub fn load_from(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("cannot read config file {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&raw)
            .with_context(|| format!("invalid config in {}", path.display()))?;
        cfg.agent.data_dir = expand_tilde(&cfg.agent.data_dir);
        Ok(cfg)
    }
}

/// The user's home directory. We avoid an extra crate for this: on the Linux
/// machines dumi targets, `$HOME` is always set.
fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Base directory for configuration, following the XDG convention.
fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home().join(".config"),
    }
}

/// Replace a leading `~` with the home directory so `data_dir = "~/.local/..."`
/// works in the config file.
fn expand_tilde(path: &Path) -> PathBuf {
    let Some(as_str) = path.to_str() else {
        return path.to_path_buf();
    };
    crate::util::expand_tilde(as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses_and_gets_defaults() {
        let raw = r#"
[model]
base_url = "https://api.example.com/v1"
api_key = "sk-test"
model = "test-model"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.model.model, "test-model");
        assert_eq!(cfg.agent.name, "dumi");
        assert!(cfg.agent.data_dir.ends_with(".local/share/dumi"));
        assert_eq!(cfg.agent.heartbeat_cron, "*/30 * * * *");
        assert!(cfg.agent.owner_jami_id.is_none());
    }

    #[test]
    fn agent_section_overrides_defaults() {
        let raw = r#"
[model]
base_url = "u"
api_key = "k"
model = "m"

[agent]
name = "judo"
heartbeat_cron = "0 9 * * *"
"#;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert_eq!(cfg.agent.name, "judo");
        assert_eq!(cfg.agent.heartbeat_cron, "0 9 * * *");
        // untouched fields keep their defaults
        assert!(cfg.agent.data_dir.ends_with(".local/share/dumi"));
    }

    #[test]
    fn missing_model_section_is_an_error() {
        let raw = "";
        assert!(toml::from_str::<Config>(raw).is_err());
    }
}
