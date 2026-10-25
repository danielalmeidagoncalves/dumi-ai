//! dumi — a tiny personal AI agent (a Rust learning project).
//!
//! Module map:
//!   config.rs    — reads ~/.config/dumi/config.toml
//!   llm.rs       — talks to any OpenAI-compatible chat API
//!   agent.rs     — conversation + tool-calling loop (the "brain")
//!   tools.rs     — what the agent can DO (shell, files, memory, reminders)
//!   memory.rs    — long-term memory (memory.md)
//!   jobs.rs      — scheduled jobs stored in jobs.json
//!   timeparse.rs — "in 20m" / "tomorrow 09:00" / cron -> timestamps
//!   channel/     — how humans talk to dumi (console today, Jami planned)

mod agent;
mod channel;
mod config;
mod jobs;
mod llm;
mod memory;
mod timeparse;
mod tools;
mod util;

use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    // RUST_LOG=dumi=debug (or "info" by default) controls how chatty dumi is.
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cfg = config::Config::load()?;
    // Never log the api_key; only where dumi keeps its files.
    tracing::info!(data_dir = %cfg.agent.data_dir.display(), "starting dumi");

    // The agent is shared: the console channel talks to it, and the
    // scheduler will fire reminders into it, too.
    let agent = Arc::new(Mutex::new(agent::Agent::new(&cfg)?));
    channel::console::run(agent).await
}
