//! The brain: keeps the conversation, talks to the model, runs tools.

use crate::config::Config;
use crate::llm::{LlmClient, Message};
use crate::memory;
use crate::tools::{self, ToolContext};
use anyhow::{Context, Result};

/// How many assistant→tools→assistant rounds we allow before forcing the
/// model to produce a final answer without tools.
const MAX_TOOL_ROUNDS: usize = 8;
/// How many conversation messages we keep (the system prompt does not count).
/// Older ones are dropped to keep requests small and cheap.
const MAX_HISTORY: usize = 60;

pub struct Agent {
    llm: LlmClient,
    /// Everything the model sees: the system prompt first, then the
    /// conversation so far.
    history: Vec<Message>,
    tools: ToolContext,
}

impl Agent {
    pub fn new(cfg: &Config) -> Result<Self> {
        std::fs::create_dir_all(&cfg.agent.data_dir)
            .with_context(|| format!("cannot create data dir {}", cfg.agent.data_dir.display()))?;
        Ok(Self {
            llm: LlmClient::new(&cfg.model)?,
            history: vec![Message::system(system_prompt(cfg))],
            tools: ToolContext {
                data_dir: cfg.agent.data_dir.clone(),
            },
        })
    }

    /// Handle one user message; returns the assistant's final text.
    /// Errors bubble up to the channel, which shows them and carries on.
    pub async fn handle(&mut self, user_text: &str) -> Result<String> {
        self.history.push(Message::user(user_text));
        let reply = self.think_with_tools().await?;
        self.trim_history();
        Ok(reply)
    }

    /// The core loop: ask the model; if it wants tools, run them and ask
    /// again; repeat until it answers in plain text.
    async fn think_with_tools(&mut self) -> Result<String> {
        let definitions = tools::definitions();
        for _round in 0..MAX_TOOL_ROUNDS {
            let reply = self.llm.chat(&self.history, &definitions).await?;
            if reply.tool_calls.is_empty() {
                let text = reply.content.clone();
                self.history.push(reply);
                return Ok(text);
            }
            // Push the assistant's tool-call message BEFORE the tool results:
            // providers require exactly this order on the wire.
            let calls = reply.tool_calls.clone();
            self.history.push(reply);
            for call in calls {
                tracing::info!(tool = %call.function.name, "running tool");
                let output =
                    tools::execute(&call.function.name, &call.function.arguments, &self.tools)
                        .await;
                self.history.push(Message::tool_result(&call.id, output));
            }
        }
        // The model kept calling tools past our limit: ask once more WITHOUT
        // tools so it has to answer in text.
        tracing::warn!("model hit the tool-round limit; forcing a final answer");
        let reply = self.llm.chat(&self.history, &[]).await?;
        let text = reply.content.clone();
        self.history.push(reply);
        Ok(text)
    }

    /// Keep the system prompt plus the newest MAX_HISTORY messages. After
    /// trimming, also drop any leading "tool" messages: a tool result whose
    /// matching assistant tool-call message was cut would make providers
    /// reject the request.
    fn trim_history(&mut self) {
        let excess = self.history.len().saturating_sub(MAX_HISTORY + 1);
        if excess > 0 {
            self.history.drain(1..1 + excess);
            while self.history.get(1).is_some_and(|m| m.role == "tool") {
                self.history.remove(1);
            }
        }
    }
}

/// The system prompt tells the model who it is, what it can do, and what it
/// knows. Built once at startup; long-term memory is included at the bottom.
fn system_prompt(cfg: &Config) -> String {
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M %Z");
    format!(
        r#"You are {name}, a personal AI agent that lives on your owner's Linux machine and chats over text messages.

Current local time: {now}.

How you talk:
- Short, plain-text answers. No markdown walls, no emoji spam — your messages arrive in a terminal (and soon on Jami chat).
- The owner may write in any language; answer in the owner's language.

What you can do (through tools):
- run_command: run shell commands on the machine. Do whatever the owner asks and explain what you ran. Never delete anything beyond the owner's explicit request.
- read_file / write_file / list_dir: work with the owner's files.
- remember / recall: keep lasting facts about the owner and their preferences.
- set_reminder / list_reminders / cancel_reminder: schedule reminders ("remind me in 20m to ...") and recurring jobs.

Habits:
- When the owner wants something to happen later or regularly, set a reminder.
- Use remember for durable preferences and facts, not for things you can check with a tool.
- If a tool answers ERROR, read it, fix what you can, and tell the owner what went wrong.

Long-term memory:
{memory}"#,
        name = cfg.agent.name,
        now = now,
        memory = memory::for_prompt(&cfg.agent.data_dir),
    )
}
