use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
use crate::config::OpenRouterConfig;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::session::ProviderSession;
use crate::summary::JsonSummary;
pub use super::tool_exec::TextToolCall;
/// Maximum number of agent loop iterations (tool call → result → next response).
const MAX_TOOL_ITERATIONS: usize = 25;
const STDERR_TAIL_MAX_LINES: usize = 120;
/// Result of a single streaming turn.
#[derive(Debug)]
pub struct TurnResult {
    /// Full response text from this turn
    pub response_content: String,
    /// Summary of token usage etc.
    pub summary: JsonSummary,
    /// Whether an error occurred
    pub error: bool,
    /// Detected tool call, if any (caller executes this)
    pub tool_call: Option<(TextToolCall, String)>,
}
fn build_openrouter_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    default_system_prompt: Option<&str>,
) -> Vec<Value> {
    let effective_query = build_query_with_memory(query, memory_context);
    let mut messages = Vec::new();

    // Merge provider-level default system prompt with session system prompt.
    let merged_prompt = match (default_system_prompt, system_prompt) {
        (Some(default), Some(session))
            if !default.trim().is_empty() && !session.trim().is_empty() =>
        {
            Some(format!("{}\n\n{}", default.trim(), session.trim()))
        }
        (Some(default), _) if !default.trim().is_empty() => Some(default.trim().to_string()),
        (_, Some(session)) if !session.trim().is_empty() => Some(session.trim().to_string()),
        _ => None,
    };

    if let Some(prompt) = merged_prompt.as_deref() {
        messages.push(serde_json::json!({
            "role": "system",
            "content": prompt
        }));
    }

    if let Some(session) = session {
        let last_index = session.messages.len().saturating_sub(1);
        for (idx, msg) in session.messages.iter().enumerate() {
            if idx == last_index && msg.is_user && msg.content.trim() == query.trim() {
                continue;
            }
            let content = if msg.is_user {
                msg.history_content.as_deref().unwrap_or(&msg.content)
            } else {
                &msg.content
            };
            if content.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::json!({
                "role": if msg.is_user { "user" } else { "assistant" },
                "content": content.trim()
            }));
        }
    }

    messages.push(serde_json::json!({
        "role": "user",
        "content": effective_query.trim()
    }));

    messages
}
pub fn build_openrouter_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &OpenRouterConfig,
) -> String {
    let messages = build_openrouter_prompt(
        system_prompt,
        query,
        session,
        memory_context,
        config.default_system_prompt.as_deref(),
    );
    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": messages,
    });
    format!(
        "curl -sN {}/chat/completions -H 'Content-Type: application/json' \
         -H 'Authorization: Bearer {}' \
         -d '{}'",
        config.base_url, "***", body
    )
}
/// Outcome of a single streaming API call.
struct StreamOutcome {
    response_content: String,
    summary: JsonSummary,
    error: bool,
}
