use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
use crate::config::prompt_needs_hyphen_guard;
use crate::config::QwenCodeCliConfig;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::wrap_command_string_with_runsc;
use crate::session::ProviderSession;
const STDERR_TAIL_MAX_LINES: usize = 120;
fn build_qwen_code_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
) -> String {
    let effective_query = build_query_with_memory(query, memory_context);

    // When resuming a session, qwen-code retains context — only send the query.
    let has_resume = session.and_then(|s| s.qwen_session_id.as_ref()).is_some();
    if has_resume {
        return effective_query;
    }

    // Without resume, build a prompt with system instructions + history + query.
    let mut sections: Vec<String> = Vec::new();

    if let Some(prompt) = system_prompt.filter(|p| !p.trim().is_empty()) {
        sections.push(format!("System instructions:\n{}", prompt.trim()));
    }

    if let Some(session) = session {
        let mut history = Vec::new();
        let last_index = session.messages.len().saturating_sub(1);
        for (idx, msg) in session.messages.iter().enumerate() {
            if idx == last_index && msg.is_user && msg.content.trim() == query.trim() {
                continue;
            }
            let role = if msg.is_user { "User" } else { "Assistant" };
            let content = if msg.is_user {
                msg.history_content.as_deref().unwrap_or(&msg.content)
            } else {
                &msg.content
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

    if effective_query.contains('\n') {
        sections.push(format!("User:\n{}", effective_query.trim()));
    } else {
        sections.push(format!("User: {}", effective_query.trim()));
    }
    sections.join("\n\n")
}
pub fn build_qwen_code_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &QwenCodeCliConfig,
) -> String {
    let session_id_to_resume = session
        .and_then(|s| s.qwen_session_id.as_ref())
        .or(config.resume_session_id.as_ref());

    let mut parts = vec![config.cli.clone()];
    parts.push(format!("--output-format {}", config.output_format));

    if config.yolo {
        parts.push("--yolo".to_string());
    }

    if let Some(model) = config.model.as_ref() {
        parts.push(format!("--model {}", model));
    }

    if let Some(session_id) = session_id_to_resume {
        parts.push(format!("--resume {}", session_id));
    }

    if let Some(auth_type) = config.auth_type.as_ref() {
        parts.push(format!("--auth-type {}", auth_type));
    }

    // System prompt: use --append-system-prompt only on new sessions.
    if session_id_to_resume.is_none() {
        let display_prompt = config.append_system_prompt.as_deref().or(system_prompt);
        if let Some(prompt) = display_prompt {
            parts.push(format!(
                "--append-system-prompt \"{}\"",
                truncate_for_display(prompt, 60)
            ));
        }
    }

    for dir in &config.include_directories {
        parts.push(format!("--include-directories {}", dir));
    }

    if !config.allowed_mcp_server_names.is_empty() {
        parts.push(format!(
            "--allowed-mcp-server-names {}",
            config.allowed_mcp_server_names.join(",")
        ));
    }

    let prompt = build_qwen_code_prompt(system_prompt, query, session, memory_context);
    if prompt_needs_hyphen_guard(&prompt) {
        parts.push("--".to_string());
    }
    parts.push(format!("-p \"{}\"", truncate_for_display(&prompt, 120)));
    wrap_command_string_with_runsc(
        parts.join(" "),
        session.and_then(|value| value.working_dir.as_deref()),
        config.runsc_image.as_deref(),
    )
}
fn truncate_for_display(s: &str, max_len: usize) -> String {
    let mut chars = s.chars();
    let truncated: String = chars.by_ref().take(max_len).collect();
    let escaped = if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        s.to_string()
    };
    escaped.replace('"', "\\\"")
}
