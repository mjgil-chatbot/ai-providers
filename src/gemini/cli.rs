use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::BufRead;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
use crate::live_summary_formatter::LiveSummaryFormatter;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::session::ProviderSession;
const STDERR_TAIL_MAX_LINES: usize = 120;
const LIVE_SUMMARY_FORMATTER: LiveSummaryFormatter = LiveSummaryFormatter::new("Gemini");
fn build_gemini_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    has_resume: bool,
    memory_context: Option<&MemoryContext>,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    let effective_query = build_query_with_memory(query, memory_context);

    if !has_resume {
        if let Some(prompt) = system_prompt {
            if !prompt.trim().is_empty() {
                sections.push(format!("System instructions:\n{}", prompt));
            }
        }
    }

    if !has_resume {
        if let Some(session) = session {
            let mut history = Vec::new();
            let last_index = session.messages.len().saturating_sub(1);
            for (idx, msg) in session.messages.iter().enumerate() {
                if idx == last_index && msg.is_user && msg.content.trim() == query.trim() {
                    continue;
                }
                let role = if msg.is_user { "User" } else { "Assistant" };
                let content: Cow<'_, str> = if msg.is_user {
                    Cow::Borrowed(msg.history_content.as_deref().unwrap_or(&msg.content))
                } else {
                    Cow::Borrowed(&msg.content)
                };
                if content.trim().is_empty() {
                    continue;
                }
                history.push(format!("{role}: {}", content.trim()));
            }
            if !history.is_empty() {
                sections.push(format!("Conversation so far:\n{}", history.join("\n")));
            }
        }
    }

    if effective_query.contains('\n') {
        sections.push(format!("User:\n{}", effective_query.trim()));
    } else {
        sections.push(format!("User: {}", effective_query.trim()));
    }
    sections.join("\n\n")
}
fn is_disallowed_gemini_include_dir(dir: &str) -> bool {
    let trimmed = dir.trim();
    trimmed.is_empty() || (!cfg!(windows) && Path::new(trimmed) == Path::new("/"))
}
