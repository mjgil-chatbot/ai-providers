use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
use crate::config::CodexCliConfig;
use crate::memory::{build_query_with_memory, MemoryContext};
const STDERR_TAIL_MAX_LINES: usize = 120;
fn configured_codex_home(config: &CodexCliConfig) -> Option<std::path::PathBuf> {
    config
        .extra_env
        .iter()
        .find(|(key, _)| key == "CODEX_HOME")
        .map(|(_, value)| std::path::PathBuf::from(value))
}
fn build_codex_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session_id: Option<&str>,
    memory_context: Option<&MemoryContext>,
) -> String {
    let effective_query = build_query_with_memory(query, memory_context);
    if session_id.is_some() {
        return effective_query;
    }
    match system_prompt {
        Some(prompt) if !prompt.trim().is_empty() => {
            format!(
                "System instructions:\n{}\n\nUser: {}",
                prompt, effective_query
            )
        }
        _ => effective_query,
    }
}
