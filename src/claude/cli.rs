use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{debug, error, info, warn};
use serde_json::Value;
use crate::config::prompt_needs_hyphen_guard;
use crate::config::ClaudeCliConfig;
use crate::sandbox::wrap_command_string_with_runsc;
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
fn load_claude_md_instructions(session: Option<&ProviderSession>) -> Option<String> {
    let mut sections = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if let Some(path) = global_claude_md_path() {
        if let Ok(canonical) = path.canonicalize() {
            if seen.insert(canonical.clone()) {
                if let Ok(content) = std::fs::read_to_string(&canonical) {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        sections.push(trimmed.to_string());
                    }
                }
            }
        }
    }

    for path in workspace_instruction_paths_for_session(session) {
        let Ok(canonical) = path.canonicalize() else {
            continue;
        };
        if !seen.insert(canonical.clone()) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&canonical) else {
            continue;
        };
        let trimmed = content.trim();
        if !trimmed.is_empty() {
            sections.push(trimmed.to_string());
        }
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}
pub fn effective_claude_system_prompt(
    config: &ClaudeCliConfig,
    session: Option<&ProviderSession>,
    system_prompt: Option<&str>,
    session_is_resumed: bool,
) -> Option<String> {
    let mut sections = Vec::new();

    if uses_minimax_transport(config) {
        if let Some(instructions) = load_claude_md_instructions(session) {
            sections.push(instructions);
        }
    }

    if !session_is_resumed {
        if let Some(prompt) = system_prompt
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        {
            sections.push(prompt.to_string());
        }
    }

    (!sections.is_empty()).then(|| sections.join("\n\n"))
}
pub fn build_claude_command_string(
    system_prompt: Option<&str>,
    session: Option<&ProviderSession>,
    session_id: Option<&str>,
    query: &str,
    config: &ClaudeCliConfig,
) -> String {
    let mut parts = vec![config.cli.clone()];
    if let Some(prompt) =
        effective_claude_system_prompt(config, session, system_prompt, session_id.is_some())
    {
        parts.push(format!("--append-system-prompt \"{}\"", prompt));
    }
    if config.skip_permissions {
        parts.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(session_id) = session_id {
        parts.push(format!("--resume {}", session_id));
    }
    if config.verbose_output {
        parts.push("--verbose".to_string());
    }
    parts.push(format!("--model {}", config.model));
    if let Some(schema) = config.json_schema.as_deref() {
        let display_schema =
            serde_json::to_string(schema).unwrap_or_else(|_| "\"<json-schema>\"".to_string());
        parts.push(format!("--json-schema {}", display_schema));
    }
    parts.push(format!("--output-format {}", config.output_format));
    if prompt_needs_hyphen_guard(query) {
        parts.push(format!("-p -- \"{}\"", query));
    } else {
        parts.push(format!("-p \"{}\"", query));
    }
    wrap_command_string_with_runsc(parts.join(" "), None, config.runsc_image.as_deref())
}
#[derive(Debug, Clone, PartialEq)]
pub enum SubscriptionType {
    Max,
    Pro,
    Api,
    Unknown,
}
impl SubscriptionType {
    pub fn is_subscription(&self) -> bool {
        matches!(self, SubscriptionType::Max | SubscriptionType::Pro)
    }
}
