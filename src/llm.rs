//! A very small OpenAI-compatible chat client.
//!
//! One method does the talking: [`LlmClient::chat`] sends the whole
//! conversation (plus our tool definitions) to `${base_url}/chat/completions`
//! and returns the model's reply message — which may be plain text *or*
//! requests to run tools (see `tools.rs` for what happens next).

use crate::config::ModelConfig;
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

/// One line of conversation. The same struct is used for what we send and
/// for what the provider answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// "system" | "user" | "assistant" | "tool".
    pub role: String,
    /// Text of the message. Some providers send `"content": null` when the
    /// model decides to call tools instead of answering — `null_to_empty`
    /// turns that into "" so this field can stay a plain `String`.
    #[serde(default, deserialize_with = "null_to_empty")]
    pub content: String,
    /// Tools the model wants us to run (only on assistant messages).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Which tool call this result answers (only on "tool" messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    /// Result of running a tool, addressed to the tool call that asked for it.
    pub fn tool_result(call_id: &str, text: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: text.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

/// Providers may legally answer with `"content": null`; we store "" instead.
fn null_to_empty<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let as_option: Option<String> = Option::deserialize(deserializer)?;
    Ok(as_option.unwrap_or_default())
}

/// A tool call the model made: "run function X with these JSON arguments".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Part of the wire format; always "function" for us.
    #[serde(rename = "type", default = "default_tool_kind")]
    pub kind: String,
    /// Provider-generated id; our tool result message must quote it back.
    pub id: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON arguments as a raw string exactly as the model produced them
    /// (parsing happens in `tools.rs`, so broken JSON can be reported to
    /// the model instead of crashing dumi).
    pub arguments: String,
}

fn default_tool_kind() -> String {
    "function".into()
}

/// A tool we advertise to the model in the request.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    #[serde(rename = "type")]
    kind: String,
    function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
struct ToolFunction {
    name: String,
    description: String,
    /// JSON Schema for the arguments, built with `serde_json::json!`.
    parameters: serde_json::Value,
}

impl ToolDef {
    pub fn new(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            kind: "function".into(),
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

/// Request body (only what we actually use).
#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    // Some providers reject an empty tools array, so we omit it entirely
    // when we have no tools to offer.
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDef]>,
}

/// Response body (only what we actually use).
#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

/// The HTTP client for one model provider.
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(cfg: &ModelConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            // A chatty model with several tool rounds can take a while.
            .timeout(Duration::from_secs(120))
            .build()
            .context("could not build HTTP client")?;
        // Trim a trailing "/" so we never produce ".../v1//chat/completions".
        Ok(Self {
            http,
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            api_key: cfg.api_key.clone(),
            model: cfg.model.clone(),
        })
    }

    /// Send the conversation and return the model's reply message.
    /// The reply either has `content` (final answer) or `tool_calls`.
    pub async fn chat(&self, messages: &[Message], tools: &[ToolDef]) -> Result<Message> {
        let url = format!("{}/chat/completions", self.base_url);
        let body = ChatRequest {
            model: &self.model,
            messages,
            tools: if tools.is_empty() { None } else { Some(tools) },
        };

        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("request to {url} failed"))?;

        // Read the body as text first: if the provider answered with an
        // error, we want its message, not a generic "bad status" error.
        let status = response.status();
        let text = response
            .text()
            .await
            .context("could not read provider response body")?;
        if !status.is_success() {
            bail!(
                "model provider answered {status}: {}",
                crate::util::truncate(&text, 2000)
            );
        }

        let parsed: ChatResponse = serde_json::from_str(&text)
            .context("could not parse provider response as chat completions JSON")?;
        let message = parsed
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("model provider returned an empty choices array"))?
            .message;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_message_with_null_content_parses() {
        let raw = r#"{
            "role": "assistant",
            "content": null,
            "tool_calls": [
                {"id": "call-1", "type": "function",
                 "function": {"name": "run_command", "arguments": "{\"command\": \"ls\"}"}}
            ]
        }"#;
        let msg: Message = serde_json::from_str(raw).unwrap();
        assert_eq!(msg.content, "");
        assert_eq!(msg.tool_calls.len(), 1);
        assert_eq!(msg.tool_calls[0].function.name, "run_command");
        assert_eq!(msg.tool_calls[0].kind, "function");
    }

    #[test]
    fn request_omits_tools_when_none() {
        let messages = [Message::user("hi")];
        let body = ChatRequest {
            model: "m",
            messages: &messages,
            tools: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("tools").is_none());
        assert_eq!(json["messages"][0]["role"], "user");

        let tools = [ToolDef::new(
            "t",
            "d",
            serde_json::json!({"type": "object"}),
        )];
        let body = ChatRequest {
            model: "m",
            messages: &messages,
            tools: Some(&tools),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
    }

    #[test]
    fn tool_result_message_serializes_with_call_id() {
        let msg = Message::tool_result("call-1", "done");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call-1");
        // Empty tool_calls must not appear on the wire at all.
        assert!(json.get("tool_calls").is_none());
    }
}
