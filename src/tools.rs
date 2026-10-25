//! The agent's hands: functions the model can ask us to run.
//!
//! Flow: the model replies with a "tool call" (name + JSON arguments).
//! [`execute`] matches the name, runs the matching Rust function, and
//! returns plain text — the output, or an ERROR line the model can read
//! and react to. Tool errors never crash dumi.

use crate::jobs::{self, Job};
use crate::llm::ToolDef;
use crate::memory;
use crate::timeparse::{self, ParsedTime};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Commands must finish within this time, so a hung command cannot block
/// dumi forever.
const COMMAND_TIMEOUT_SECS: u64 = 60;
/// Anything we send back to the model is capped (models have input limits).
const OUTPUT_CAP: usize = 4_000;
/// File contents get a little more room than command output.
const FILE_CAP: usize = 8_000;

/// Everything a tool may need while running.
pub struct ToolContext {
    /// dumi's data directory (memory and jobs live here).
    pub data_dir: PathBuf,
}

/// Advertise our tools to the model. The JSON Schema objects describe each
/// tool's arguments; the model fills them in when it calls.
pub fn definitions() -> Vec<ToolDef> {
    vec![
        ToolDef::new(
            "run_command",
            "Run a shell command on the owner's Linux machine. Returns stdout, stderr and the exit code.",
            json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The shell command to run"}
                },
                "required": ["command"]
            }),
        ),
        ToolDef::new(
            "read_file",
            "Read a text file from disk.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or ~/..."}
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            "write_file",
            "Create or overwrite a text file.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or ~/..."},
                    "content": {"type": "string", "description": "Full new content of the file"}
                },
                "required": ["path", "content"]
            }),
        ),
        ToolDef::new(
            "list_dir",
            "List the entries of a directory.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Absolute path or ~/..."}
                },
                "required": ["path"]
            }),
        ),
        ToolDef::new(
            "set_reminder",
            "Schedule a reminder or a recurring job for the owner.",
            json!({
                "type": "object",
                "properties": {
                    "title": {"type": "string", "description": "What should happen when it fires"},
                    "when": {"type": "string", "description": "When to fire: 'in 20m', 'in 2 hours', 'at 18:30', 'tomorrow 09:00', 'daily 08:00', 'every 30m', a 5-field cron like '*/10 * * * *', or RFC 3339"}
                },
                "required": ["title", "when"]
            }),
        ),
        ToolDef::new(
            "list_reminders",
            "List all scheduled reminders and recurring jobs, with their ids.",
            json!({"type": "object", "properties": {}}),
        ),
        ToolDef::new(
            "cancel_reminder",
            "Cancel a reminder or recurring job by its id (see list_reminders).",
            json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"}
                },
                "required": ["id"]
            }),
        ),
        ToolDef::new(
            "remember",
            "Store a lasting fact or preference about the owner in long-term memory.",
            json!({
                "type": "object",
                "properties": {
                    "fact": {"type": "string"}
                },
                "required": ["fact"]
            }),
        ),
        ToolDef::new(
            "recall",
            "Search long-term memory. Without a query, returns everything remembered.",
            json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                }
            }),
        ),
    ]
}

/// Run one tool call; the returned String goes back into the conversation.
pub async fn execute(name: &str, arguments: &str, ctx: &ToolContext) -> String {
    // Some providers send "" instead of "{}" when a tool takes no arguments.
    let args: Value = match serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(v) => v,
        Err(e) => return format!("ERROR: could not parse tool arguments as JSON: {e}"),
    };
    let outcome: Result<String> = match name {
        "run_command" => run_command(&args).await,
        "read_file" => read_file(&args),
        "write_file" => write_file(&args),
        "list_dir" => list_dir(&args),
        "set_reminder" => set_reminder(&args, ctx),
        "list_reminders" => list_reminders(ctx),
        "cancel_reminder" => cancel_reminder(&args, ctx),
        "remember" => remember(&args, ctx),
        "recall" => recall(&args, ctx),
        other => Err(anyhow!("unknown tool '{other}'")),
    };
    match outcome {
        Ok(text) => text,
        Err(e) => format!("ERROR: {e:#}"),
    }
}

async fn run_command(args: &Value) -> Result<String> {
    let command = req_str(args, "command")?;
    tracing::info!(command, "running shell command");
    // `sh -c` so pipes, globs and env vars work like in a terminal.
    let running = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .output();
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(COMMAND_TIMEOUT_SECS),
        running,
    )
    .await
    .map_err(|_| anyhow!("command did not finish within {COMMAND_TIMEOUT_SECS} seconds"))?
    .context("failed to run command")?;

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&stderr);
    }
    text.push_str(&format!(
        "\n[exit code: {}]",
        output.status.code().unwrap_or(-1)
    ));
    Ok(crate::util::truncate(&text, OUTPUT_CAP))
}

fn read_file(args: &Value) -> Result<String> {
    let path = req_str(args, "path")?;
    let path = crate::util::expand_tilde(path);
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok(crate::util::truncate(&content, FILE_CAP))
}

fn write_file(args: &Value) -> Result<String> {
    let path = req_str(args, "path")?;
    let content = req_str(args, "content")?;
    let path = crate::util::expand_tilde(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("cannot write {}", path.display()))?;
    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

fn list_dir(args: &Value) -> Result<String> {
    let path = req_str(args, "path")?;
    let path = crate::util::expand_tilde(path);
    let entries =
        std::fs::read_dir(&path).with_context(|| format!("cannot list {}", path.display()))?;
    let mut lines: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.context("directory entry vanished while listing")?;
        let kind = if entry.file_type().context("cannot inspect entry")?.is_dir() {
            "dir "
        } else {
            "file"
        };
        lines.push(format!("{kind} {}", entry.file_name().to_string_lossy()));
    }
    if lines.is_empty() {
        return Ok("(empty directory)".to_string());
    }
    lines.sort();
    Ok(crate::util::truncate(&lines.join("\n"), OUTPUT_CAP))
}

fn set_reminder(args: &Value, ctx: &ToolContext) -> Result<String> {
    let title = req_str(args, "title")?;
    let when = req_str(args, "when")?;
    let parsed = timeparse::parse(when, chrono::Local::now())
        .with_context(|| format!("could not understand the time '{when}'"))?;

    let mut jobs = jobs::load(&ctx.data_dir);
    let job = match parsed {
        ParsedTime::Once(at) => Job {
            id: jobs::new_id(),
            title: title.to_string(),
            at,
            recur: None,
        },
        ParsedTime::Recurring(cron) => Job {
            id: jobs::new_id(),
            title: title.to_string(),
            at: timeparse::next_run(&cron)?,
            recur: Some(cron),
        },
    };
    let description = job.describe();
    jobs.push(job);
    jobs::save(&ctx.data_dir, &jobs)?;
    Ok(format!("scheduled: {description}"))
}

fn list_reminders(ctx: &ToolContext) -> Result<String> {
    let jobs = jobs::load(&ctx.data_dir);
    if jobs.is_empty() {
        return Ok("no reminders scheduled".to_string());
    }
    Ok(jobs
        .iter()
        .map(Job::describe)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn cancel_reminder(args: &Value, ctx: &ToolContext) -> Result<String> {
    let id = req_str(args, "id")?;
    let mut jobs = jobs::load(&ctx.data_dir);
    let before = jobs.len();
    jobs.retain(|job| job.id != id);
    if jobs.len() == before {
        bail!("no reminder with id '{id}'");
    }
    jobs::save(&ctx.data_dir, &jobs)?;
    Ok(format!("cancelled {id}"))
}

fn remember(args: &Value, ctx: &ToolContext) -> Result<String> {
    let fact = req_str(args, "fact")?;
    memory::append(&ctx.data_dir, fact)?;
    Ok("saved to long-term memory".to_string())
}

fn recall(args: &Value, ctx: &ToolContext) -> Result<String> {
    Ok(memory::recall(&ctx.data_dir, opt_str(args, "query")))
}

/// A required string argument from the model's JSON, or a friendly error.
fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing or non-string argument '{key}'"))
}

/// An optional string argument.
fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
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

    #[tokio::test]
    async fn file_tools_round_trip() {
        let ctx = ToolContext {
            data_dir: tmp("files"),
        };
        let file = ctx.data_dir.join("hello.txt");
        let path = file.to_string_lossy().into_owned();

        let out = execute(
            "write_file",
            &json!({"path": path, "content": "hi"}).to_string(),
            &ctx,
        )
        .await;
        assert!(!out.contains("ERROR"), "write_file failed: {out}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "hi");

        let out = execute("read_file", &json!({"path": path}).to_string(), &ctx).await;
        assert_eq!(out, "hi");

        let out = execute(
            "list_dir",
            &json!({"path": ctx.data_dir.to_string_lossy()}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.contains("hello.txt"), "list_dir missed the file: {out}");

        let out = execute(
            "read_file",
            &json!({"path": "/no/such/file/here"}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.starts_with("ERROR"));
        std::fs::remove_dir_all(&ctx.data_dir).ok();
    }

    #[tokio::test]
    async fn reminder_round_trip() {
        let ctx = ToolContext {
            data_dir: tmp("reminders"),
        };

        let out = execute(
            "set_reminder",
            &json!({"title": "stretch", "when": "in 20m"}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.starts_with("scheduled: "), "unexpected reply: {out}");
        assert!(out.contains("stretch"));

        execute(
            "set_reminder",
            &json!({"title": "standup", "when": "daily 08:00"}).to_string(),
            &ctx,
        )
        .await;
        let listed = execute("list_reminders", "{}", &ctx).await;
        assert!(listed.contains("stretch"));
        assert!(listed.contains("recurring: 0 8 * * *"));

        // Grab the first id ("[job-...]") and cancel it.
        let id = &listed[listed.find('[').unwrap() + 1..listed.find(']').unwrap()];
        let out = execute("cancel_reminder", &json!({"id": id}).to_string(), &ctx).await;
        assert_eq!(out, format!("cancelled {id}"));
        let listed_after = execute("list_reminders", "{}", &ctx).await;
        assert!(listed_after.contains("standup"));
        assert!(!listed_after.contains("stretch"));

        // Times the parser does not understand come back as ERROR text.
        let out = execute(
            "set_reminder",
            &json!({"title": "x", "when": "someday"}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.starts_with("ERROR"), "expected an error, got: {out}");
        std::fs::remove_dir_all(&ctx.data_dir).ok();
    }

    #[tokio::test]
    async fn run_command_captures_output_and_exit_code() {
        let ctx = ToolContext {
            data_dir: tmp("cmd"),
        };
        let out = execute(
            "run_command",
            &json!({"command": "echo hello && echo oops >&2"}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.contains("hello"));
        assert!(out.contains("oops"));
        assert!(out.contains("[exit code: 0]"));

        let out = execute(
            "run_command",
            &json!({"command": "false"}).to_string(),
            &ctx,
        )
        .await;
        assert!(out.contains("[exit code: 1]"));

        let out = execute("no_such_tool", "{}", &ctx).await;
        assert!(out.starts_with("ERROR"));
        std::fs::remove_dir_all(&ctx.data_dir).ok();
    }
}
