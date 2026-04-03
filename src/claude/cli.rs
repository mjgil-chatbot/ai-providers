use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{debug, error, info, warn};
use serde_json::Value;
use crate::config::ClaudeCliConfig;
use crate::session::ProviderSession;
const STDERR_TAIL_MAX_LINES: usize = 120;
fn claude_transport_base_url(config: &ClaudeCliConfig) -> Option<&str> {
    config
        .extra_env
        .iter()
        .find_map(|(key, value)| (key == "ANTHROPIC_BASE_URL").then_some(value.as_str()))
}
fn uses_minimax_transport(config: &ClaudeCliConfig) -> bool {
    claude_transport_base_url(config).is_some_and(|base_url| base_url.contains("minimax.io"))
        || config.model.to_ascii_lowercase().contains("minimax")
}
fn global_claude_md_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".claude").join("CLAUDE.md"))
}
fn workspace_instruction_paths_for_session(
    session: Option<&ProviderSession>,
) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();

    let workspace_root = session
        .and_then(|session| session.working_dir.as_deref())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::current_dir().ok());

    if let Some(workspace_root) = workspace_root {
        paths.push(workspace_root.join("CLAUDE.md"));
        paths.push(workspace_root.join(".claude").join("CLAUDE.md"));
    }

    paths
}
