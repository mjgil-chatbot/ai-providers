use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
use crate::config::format_prompt_arg;
use crate::config::KimiCliConfig;
use crate::live_summary_formatter::LiveSummaryFormatter;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::wrap_command_string_with_runsc;
use crate::session::ProviderSession;
const STDERR_TAIL_MAX_LINES: usize = 120;
const LIVE_SUMMARY_FORMATTER: LiveSummaryFormatter = LiveSummaryFormatter::new("Kimi");
fn build_kimi_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    has_resume: bool,
    memory_context: Option<&MemoryContext>,
) -> String {
    let mut sections: Vec<String> = Vec::new();
    let effective_query = build_query_with_memory(query, memory_context);

    if let Some(prompt) = system_prompt {
        if !prompt.trim().is_empty() {
            sections.push(format!("System instructions:\n{}", prompt));
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
pub fn build_kimi_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &KimiCliConfig,
) -> String {
    let session_id_to_resume = session.and_then(|value| value.kimi_session_id.as_ref());
    let prompt = build_kimi_prompt(
        system_prompt,
        query,
        session,
        session_id_to_resume.is_some(),
        memory_context,
    );
    let mut parts = vec![config.cli.clone()];
    parts.push("--print".to_string());
    parts.push(format!("--output-format {}", config.output_format));
    if config.yolo {
        parts.push("--yolo".to_string());
    }
    if let Some(model) = config.model.as_deref() {
        parts.push(format!("--model {}", model));
    }
    if let Some(session_id) = session_id_to_resume {
        parts.push(format!("--resume {}", session_id));
    }
    parts.push(format_prompt_arg(&prompt));
    wrap_command_string_with_runsc(
        parts.join(" "),
        session.and_then(|value| value.working_dir.as_deref()),
        config.runsc_image.as_deref(),
    )
}
