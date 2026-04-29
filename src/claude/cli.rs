use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{debug, error, info, warn};
use serde_json::Value;

use crate::claude::response::{generate_json_line_summary, update_summary_from_event};
use crate::config::{add_prompt_arg, has_mcp_servers, prompt_needs_hyphen_guard, ClaudeCliConfig};
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::{wrap_command_string_with_runsc, wrap_command_with_runsc};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::JsonSummary;
use crate::workspace;

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

pub fn get_subscription_type(config: &ClaudeCliConfig) -> SubscriptionType {
    if let Some(subscription) = get_subscription_from_credentials_json(config)
        .or_else(|| get_subscription_from_claude_json(config))
    {
        return subscription;
    }
    if config.config_dir.is_none() && std::env::var("ANTHROPIC_API_KEY").is_ok() {
        return SubscriptionType::Api;
    }
    SubscriptionType::Unknown
}

fn get_subscription_from_credentials_json(config: &ClaudeCliConfig) -> Option<SubscriptionType> {
    let path = if let Some(config_dir) = config.config_dir.as_ref() {
        config_dir.join(".credentials.json")
    } else {
        let home = std::env::var("HOME").ok()?;
        std::path::PathBuf::from(home).join(".claude/.credentials.json")
    };
    let content = std::fs::read_to_string(path).ok()?;
    parse_subscription_from_json(&content)
}

fn get_subscription_from_claude_json(config: &ClaudeCliConfig) -> Option<SubscriptionType> {
    let path = if let Some(config_dir) = config.config_dir.as_ref() {
        config_dir.join(".claude.json")
    } else {
        let home = std::env::var("HOME").ok()?;
        std::path::PathBuf::from(home).join(".claude.json")
    };
    let content = std::fs::read_to_string(path).ok()?;
    let json: Value = serde_json::from_str(&content).ok()?;
    parse_claude_json_subscription(&json)
}

fn parse_claude_json_subscription(json: &Value) -> Option<SubscriptionType> {
    let has_subscription = json
        .get("hasAvailableSubscription")
        .and_then(|value| value.as_bool())?;
    if has_subscription {
        let has_opus = json
            .get("hasOpusPlanDefault")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        if has_opus {
            Some(SubscriptionType::Max)
        } else {
            Some(SubscriptionType::Pro)
        }
    } else if json.get("oauthAccount").is_some() {
        Some(SubscriptionType::Api)
    } else {
        None
    }
}

fn parse_subscription_from_json(json_str: &str) -> Option<SubscriptionType> {
    let json: Value = serde_json::from_str(json_str).ok()?;
    let subscription_type = json
        .get("claudeAiOauth")
        .and_then(|oauth| oauth.get("subscriptionType"))
        .and_then(|value| value.as_str())?;
    match subscription_type.to_lowercase().as_str() {
        "max" => Some(SubscriptionType::Max),
        "pro" => Some(SubscriptionType::Pro),
        "api" => Some(SubscriptionType::Api),
        _ => None,
    }
}

pub fn call_claude_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &ClaudeCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting claude cli stream");

    if !config.allow_api_billing {
        let subscription_type = get_subscription_type(config);
        debug!(?subscription_type, "subscription type detected");
        if !subscription_type.is_subscription() {
            let login_cmd = if let Some(config_dir) = config.config_dir.as_ref() {
                format!("CLAUDE_CONFIG_DIR={} claude login", config_dir.display())
            } else {
                "claude login".to_string()
            };
            let error_msg = match subscription_type {
                SubscriptionType::Api => {
                    format!("API billing detected. Please login with a subscription account using '{login_cmd}' or enable allow_api_billing.")
                }
                SubscriptionType::Unknown => {
                    format!("Unable to detect subscription type. Please ensure you are logged in using '{login_cmd}'.")
                }
                _ => unreachable!(),
            };
            let _ = tx.send(StreamMessage {
                response: error_msg,
                command: String::new(),
                session_id: None,
                summary: None,
                is_final: true,
            });
            return Ok(());
        }
    }

    if let Some(config_dir) = config.config_dir.as_ref() {
        if !config_dir.is_dir() {
            let _ = tx.send(StreamMessage {
                response: format!(
                    "Claude config directory not found: {}",
                    config_dir.display()
                ),
                command: String::new(),
                session_id: None,
                summary: None,
                is_final: true,
            });
            return Ok(());
        }
    }

    if config.require_mcp_servers && !has_mcp_servers(config.config_dir.as_deref()) {
        let _ = tx.send(StreamMessage {
            response: "No MCP servers found in provider config.".to_string(),
            command: String::new(),
            session_id: None,
            summary: None,
            is_final: true,
        });
        return Ok(());
    }

    let mut cmd = Command::new(&config.cli);
    workspace::apply_session_working_dir(&mut cmd, session.as_ref());
    let effective_query = build_query_with_memory(query, memory_context);

    if let Some(config_dir) = config.config_dir.as_ref() {
        cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    }
    for (key, value) in &config.extra_env {
        cmd.env(key, value);
    }

    let session_id_to_resume = session
        .as_ref()
        .and_then(|value| value.claude_session_id.as_ref());
    if let Some(prompt) = effective_claude_system_prompt(
        config,
        session.as_ref(),
        system_prompt,
        session_id_to_resume.is_some(),
    ) {
        cmd.arg("--append-system-prompt").arg(prompt);
    }
    if config.skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    if let Some(session_id) = session_id_to_resume {
        cmd.arg("--resume").arg(session_id);
    }
    if config.verbose_output {
        cmd.arg("--verbose");
    }
    cmd.arg("--model").arg(&config.model);
    if let Some(schema) = config.json_schema.as_deref() {
        cmd.arg("--json-schema").arg(schema);
    }
    cmd.arg("--output-format").arg(&config.output_format);
    add_prompt_arg(&mut cmd, &effective_query);
    let runsc_workspace_sync = wrap_command_with_runsc(
        &mut cmd,
        config.runsc_image.as_deref(),
        &config.runsc_workspace_includes,
        &config.runsc_mounts,
    )?;

    let command_str = build_claude_command_string(
        system_prompt,
        session.as_ref(),
        session_id_to_resume.map(|value| value.as_str()),
        &effective_query,
        config,
    );

    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "claude",
        &command_str,
        &tx,
        stop_signal.clone(),
        STDERR_TAIL_MAX_LINES,
    ) {
        Some(pair) => pair,
        None => return Ok(()),
    };

    let mut response_content = String::new();
    let mut json_summaries = String::new();
    let mut actual_text_content = String::new();
    let mut extracted_session_id: Option<String> = None;
    let mut error_occurred = false;
    let mut error_message = String::new();
    let mut last_error_detail: Option<String> = None;
    let mut stdout_tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_MAX_LINES);
    let mut last_stream_payload = String::new();
    let mut aggregated_summary = JsonSummary::new();

    for line in reader.lines() {
        if cli.is_stopped() {
            warn!("stop signal triggered during stream read");
            cli.kill();
            let _ = tx.send(StreamMessage {
                response: "Process stopped by user.".to_string(),
                command: command_str.clone(),
                session_id: None,
                summary: None,
                is_final: true,
            });
            return Ok(());
        }

        let line = match line {
            Ok(line) => line,
            Err(e) => {
                error!(error=%e, "error reading stdout stream");
                cli.kill();
                let _ = tx.send(StreamMessage {
                    response: format!("Error reading stream: {}", e),
                    command: command_str.clone(),
                    session_id: None,
                    summary: None,
                    is_final: true,
                });
                return Ok(());
            }
        };

        if line.trim().is_empty() {
            continue;
        }

        match serde_json::from_str::<Value>(&line) {
            Ok(json) => {
                let line_summary =
                    generate_json_line_summary(&line, aggregated_summary.model.as_deref());
                if !json_summaries.is_empty() {
                    json_summaries.push('\n');
                }
                json_summaries.push_str(&line_summary);
                response_content = if actual_text_content.is_empty() {
                    json_summaries.clone()
                } else {
                    format!("{}\n\n\n{}", json_summaries, actual_text_content)
                };
                last_stream_payload = response_content.clone();
                let _ = tx.send(StreamMessage {
                    response: response_content.clone(),
                    command: command_str.clone(),
                    session_id: None,
                    summary: None,
                    is_final: false,
                });

                update_summary_from_event(&mut aggregated_summary, &json);
                if extracted_session_id.is_none() {
                    extracted_session_id = json
                        .get("session_id")
                        .and_then(|value| value.as_str())
                        .map(|value| value.to_string());
                }

                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail);
                }

                match json.get("type").and_then(|value| value.as_str()) {
                    Some("assistant") => {
                        if let Some(message) = json.get("message") {
                            if let Some(content) =
                                message.get("content").and_then(|value| value.as_array())
                            {
                                let text = content
                                    .iter()
                                    .filter(|item| {
                                        item.get("type").and_then(|value| value.as_str())
                                            == Some("text")
                                    })
                                    .filter_map(|item| {
                                        item.get("text").and_then(|value| value.as_str())
                                    })
                                    .collect::<Vec<_>>()
                                    .join("");
                                if !text.is_empty() {
                                    actual_text_content = text;
                                }
                            }
                        }
                    }
                    Some("result") => {
                        error_occurred = json
                            .get("is_error")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false);
                        if error_occurred {
                            error_message = json
                                .get("result")
                                .and_then(|value| value.as_str())
                                .or_else(|| json.get("error").and_then(|value| value.as_str()))
                                .unwrap_or("unknown error")
                                .to_string();
                        }
                    }
                    _ => {}
                }
            }
            Err(_) => {
                if !line.starts_with('{') && !line.trim().is_empty() {
                    if stdout_tail.len() == STDERR_TAIL_MAX_LINES {
                        stdout_tail.pop_front();
                    }
                    stdout_tail.push_back(line.clone());
                    response_content.push_str(&line);
                    response_content.push('\n');
                }
            }
        }
    }

    info!(
        model = %config.model,
        response_len = response_content.len(),
        actual_text_len = actual_text_content.len(),
        has_session_id = extracted_session_id.is_some(),
        summary_response_count = aggregated_summary.response_count,
        stdout_tail_lines = stdout_tail.len(),
        error_occurred,
        "claude-compatible stdout stream closed; waiting for cli process exit"
    );
    let finish = cli.finish("claude");
    runsc_workspace_sync.sync_back()?;
    let process_ok = finish.wait_error.is_none()
        && finish
            .exit_status
            .as_ref()
            .map(|status| status.success())
            .unwrap_or(false);
    info!(
        model = %config.model,
        process_ok,
        exit_status = ?finish.exit_status,
        wait_error = ?finish.wait_error,
        stderr_tail_len = finish.stderr_tail.len(),
        response_len = response_content.len(),
        actual_text_len = actual_text_content.len(),
        has_session_id = extracted_session_id.is_some(),
        summary_response_count = aggregated_summary.response_count,
        error_occurred,
        "claude-compatible cli process exit observed"
    );
    if !process_ok || error_occurred {
        let contexts = ErrorContexts::build(
            &finish.stderr_tail,
            &stdout_tail,
            &last_stream_payload,
            &last_error_detail,
            STDERR_TAIL_MAX_LINES,
        );
        let detail =
            contexts.best_failure_detail(finish.exit_status.as_ref(), finish.wait_error.as_deref());
        let response = if error_message.is_empty() {
            format!("Claude command failed.{}", detail)
        } else {
            format!("Claude command failed: {}{}", error_message, detail)
        };
        let _ = tx.send(StreamMessage {
            response,
            command: command_str,
            session_id: extracted_session_id,
            summary: Some(aggregated_summary),
            is_final: true,
        });
        return Ok(());
    }

    let final_response = if actual_text_content.is_empty() {
        response_content
    } else {
        actual_text_content
    };
    let _ = tx.send(StreamMessage {
        response: final_response,
        command: command_str,
        session_id: extracted_session_id,
        summary: Some(aggregated_summary),
        is_final: true,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_claude_command_string_includes_json_schema_flag() {
        let config = ClaudeCliConfig {
            cli: "claude".to_string(),
            model: "claude-opus-4-7".to_string(),
            json_schema: Some(
                "{\"type\":\"object\",\"properties\":{\"summary\":{\"type\":\"string\"}}}"
                    .to_string(),
            ),
            output_format: "stream-json".to_string(),
            skip_permissions: true,
            verbose_output: true,
            allow_api_billing: false,
            config_dir: None,
            require_mcp_servers: false,
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        };

        let command =
            build_claude_command_string(Some("system"), None, None, "Return JSON only", &config);

        assert!(command.contains("--json-schema"));
        assert!(command.contains("summary"));
    }

    #[test]
    fn effective_claude_system_prompt_adds_claude_md_for_minimax_resume() {
        let temp_home = tempfile::tempdir().unwrap();
        let claude_dir = temp_home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "global rule").unwrap();

        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp_home.path());

        let prompt = effective_claude_system_prompt(
            &ClaudeCliConfig {
                extra_env: vec![(
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.minimax.io/anthropic".to_string(),
                )],
                ..ClaudeCliConfig::default()
            },
            None,
            Some("session rule"),
            true,
        );

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(prompt.as_deref(), Some("global rule"));
    }

    #[test]
    fn effective_claude_system_prompt_appends_workspace_instructions_after_global() {
        let temp_home = tempfile::tempdir().unwrap();
        let claude_dir = temp_home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "global rule").unwrap();

        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("CLAUDE.md"), "workspace rule").unwrap();
        let workspace_claude_dir = workspace.path().join(".claude");
        std::fs::create_dir_all(&workspace_claude_dir).unwrap();
        std::fs::write(
            workspace_claude_dir.join("CLAUDE.md"),
            "workspace nested rule",
        )
        .unwrap();

        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp_home.path());

        let session = ProviderSession {
            working_dir: Some(workspace.path().to_string_lossy().into_owned()),
            claude_session_id: None,
            glm_session_id: None,
            minimax_session_id: None,
            qwen_session_id: None,
            codex_session_id: None,
            nemotron_session_id: None,
            gemini_session_id: None,
            openrouter_session_id: None,
            omniforge_session_id: None,
            kimi_session_id: None,
            messages: Vec::new(),
        };

        let prompt = effective_claude_system_prompt(
            &ClaudeCliConfig {
                extra_env: vec![(
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.minimax.io/anthropic".to_string(),
                )],
                ..ClaudeCliConfig::default()
            },
            Some(&session),
            Some("session rule"),
            true,
        );

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(
            prompt.as_deref(),
            Some("global rule\n\nworkspace rule\n\nworkspace nested rule")
        );
    }

    #[test]
    fn build_claude_command_string_keeps_workspace_instructions_on_minimax_resume() {
        let temp_home = tempfile::tempdir().unwrap();
        let claude_dir = temp_home.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(claude_dir.join("CLAUDE.md"), "global rule").unwrap();

        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("CLAUDE.md"), "workspace rule").unwrap();
        let workspace_claude_dir = workspace.path().join(".claude");
        std::fs::create_dir_all(&workspace_claude_dir).unwrap();
        std::fs::write(
            workspace_claude_dir.join("CLAUDE.md"),
            "workspace nested rule",
        )
        .unwrap();

        let previous_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp_home.path());
        let session = ProviderSession {
            working_dir: Some(workspace.path().to_string_lossy().into_owned()),
            claude_session_id: None,
            glm_session_id: None,
            minimax_session_id: Some("resume-1".to_string()),
            qwen_session_id: None,
            codex_session_id: None,
            nemotron_session_id: None,
            gemini_session_id: None,
            openrouter_session_id: None,
            omniforge_session_id: None,
            kimi_session_id: None,
            messages: Vec::new(),
        };

        let command = build_claude_command_string(
            Some("session rule"),
            Some(&session),
            Some("resume-1"),
            "continue",
            &ClaudeCliConfig {
                cli: "claude".to_string(),
                model: "MiniMax-M2.7".to_string(),
                output_format: "stream-json".to_string(),
                extra_env: vec![(
                    "ANTHROPIC_BASE_URL".to_string(),
                    "https://api.minimax.io/anthropic".to_string(),
                )],
                ..ClaudeCliConfig::default()
            },
        );

        match previous_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }

        assert!(command.contains(
            "--append-system-prompt \"global rule\n\nworkspace rule\n\nworkspace nested rule\""
        ));
        assert!(!command.contains("session rule"));
    }
}
