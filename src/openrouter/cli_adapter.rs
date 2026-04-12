use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;

use crate::config::prompt_needs_hyphen_guard;
use crate::config::OpenRouterCliConfig;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::JsonSummary;
use crate::workspace;

const STDERR_TAIL_MAX_LINES: usize = 120;

fn build_openrouter_cli_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    default_system_prompt: Option<&str>,
) -> String {
    let effective_query = build_query_with_memory(query, memory_context);
    let mut sections: Vec<String> = Vec::new();

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
        sections.push(format!("System instructions:\n{}", prompt));
    }

    if let Some(session) = session {
        let session_id_to_resume = session.qwen_session_id.as_deref();
        if session_id_to_resume.is_none() {
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
    }

    if effective_query.contains('\n') {
        sections.push(format!("User:\n{}", effective_query.trim()));
    } else {
        sections.push(format!("User: {}", effective_query.trim()));
    }
    sections.join("\n\n")
}

pub fn build_openrouter_cli_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &OpenRouterCliConfig,
) -> String {
    let session_id_to_resume = session.and_then(|value| value.qwen_session_id.as_ref());
    let prompt = build_openrouter_cli_prompt(
        system_prompt,
        query,
        session,
        memory_context,
        config.default_system_prompt.as_deref(),
    );
    let mut parts = vec![config.cli.clone(), "exec".to_string()];
    parts.push(format!("--model {}", config.model));
    if let Some(base_url) =
        (config.base_url != "https://openrouter.ai/api/v1").then_some(&config.base_url)
    {
        parts.push(format!("--base-url {}", base_url));
    }
    if let Some(session_id) = session_id_to_resume.or(config.resume_session_id.as_ref()) {
        parts.push(format!("--resume {}", session_id));
    }
    if let Some(workspace_root) = config.workspace_root.as_ref() {
        parts.push(format!("--workspace-root {}", workspace_root));
    }
    if config.context_window_size > 0 {
        parts.push(format!(
            "--context-window-size {}",
            config.context_window_size
        ));
    }
    if let Some(mcp_config) = config.mcp_config.as_deref() {
        parts.push(format!("--mcp-config {}", mcp_config));
    }
    parts.push("--sandbox workspace-write".to_string());
    parts.push("--json".to_string());
    if prompt_needs_hyphen_guard(&prompt) {
        parts.push("--".to_string());
    }
    parts.push(format!("\"{}\"", prompt));
    parts.join(" ")
}

pub fn call_openrouter_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &OpenRouterCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting openrouter-cli stream");

    let session_id_to_resume = session
        .as_ref()
        .and_then(|value| value.qwen_session_id.as_ref())
        .or(config.resume_session_id.as_ref());
    let prompt = build_openrouter_cli_prompt(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config.default_system_prompt.as_deref(),
    );

    let mut cmd = Command::new(&config.cli);
    workspace::apply_session_working_dir(&mut cmd, session.as_ref());
    // Forward API key to the CLI subprocess
    if !config.api_key.is_empty() {
        cmd.env("OPENROUTER_API_KEY", &config.api_key);
    }
    if let Some(base_url) =
        (config.base_url != "https://openrouter.ai/api/v1").then_some(&config.base_url)
    {
        cmd.env("OPENROUTER_BASE_URL", base_url);
    }
    for (key, value) in &config.extra_env {
        cmd.env(key, value);
    }
    cmd.arg("exec");
    cmd.arg("--model").arg(&config.model);
    if config.base_url != "https://openrouter.ai/api/v1" {
        cmd.arg("--base-url").arg(&config.base_url);
    }
    if let Some(session_id) = session_id_to_resume {
        cmd.arg("--resume").arg(session_id);
    }
    if let Some(workspace_root) = config.workspace_root.as_ref() {
        cmd.arg("--workspace-root").arg(workspace_root);
    }
    if config.context_window_size > 0 {
        cmd.arg("--context-window-size")
            .arg(config.context_window_size.to_string());
    }
    if let Some(mcp_config) = config.mcp_config.as_deref() {
        cmd.arg("--mcp-config").arg(mcp_config);
    }
    cmd.arg("--sandbox").arg("workspace-write");
    cmd.arg("--json");
    if prompt_needs_hyphen_guard(&prompt) {
        cmd.arg("--");
    }
    cmd.arg(&prompt);

    let command_str = build_openrouter_cli_command_string(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config,
    );
    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "openrouter",
        &command_str,
        &tx,
        stop_signal.clone(),
        STDERR_TAIL_MAX_LINES,
    ) {
        Some(pair) => pair,
        None => return Ok(()),
    };

    let mut response_content = String::new();
    let mut extracted_session_id: Option<String> = None;
    let mut error_message: Option<String> = None;
    let mut stdout_tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_MAX_LINES);
    let mut last_error_detail: Option<String> = None;
    let mut aggregated_summary = JsonSummary::new();
    aggregated_summary.model = Some(config.model.clone());
    aggregated_summary.context_window_size = config.context_window_size;

    for line in reader.lines() {
        if cli.is_stopped() {
            warn!("stop signal triggered during openrouter-cli stream read");
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
            Err(error) => {
                error!(error=%error, "error reading openrouter-cli stdout stream");
                cli.kill();
                let _ = tx.send(StreamMessage {
                    response: format!("Error reading stream: {}", error),
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

        // Parse the JSONL line as a StreamMessage-like object
        // openrouter-cli emits: { response, command, session_id, summary, is_final }
        match serde_json::from_str::<Value>(&line) {
            Ok(json) => {
                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail.clone());
                    error_message = Some(detail);
                }

                // Extract response text (accumulated partial or final)
                if let Some(response) = json.get("response").and_then(|v| v.as_str()) {
                    response_content = response.to_string();
                }

                // Extract session_id from the message
                if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                    if extracted_session_id.is_none() {
                        extracted_session_id = Some(sid.to_string());
                        aggregated_summary.session_id = extracted_session_id.clone();
                    }
                }

                // Extract summary if present — the CLI emits a full JsonSummary.
                if let Some(summary_value) = json.get("summary") {
                    if !summary_value.is_null() {
                        if let Ok(parsed) =
                            serde_json::from_value::<JsonSummary>(summary_value.clone())
                        {
                            // Preserve our model and context_window_size if the CLI didn't set them.
                            let model = aggregated_summary.model.clone();
                            let cws = aggregated_summary.context_window_size;
                            aggregated_summary = parsed;
                            if aggregated_summary.model.is_none() {
                                aggregated_summary.model = model;
                            }
                            if aggregated_summary.context_window_size == 0 && cws > 0 {
                                aggregated_summary.context_window_size = cws;
                            }
                        }
                    }
                }

                let is_final = json
                    .get("is_final")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let _ = tx.send(StreamMessage {
                    response: response_content.clone(),
                    command: command_str.clone(),
                    session_id: extracted_session_id.clone(),
                    summary: Some(aggregated_summary.clone()),
                    is_final,
                });

                if is_final {
                    return Ok(());
                }
            }
            Err(_) => {
                if stdout_tail.len() == STDERR_TAIL_MAX_LINES {
                    stdout_tail.pop_front();
                }
                stdout_tail.push_back(line.clone());
            }
        }
    }

    let finish = cli.finish("openrouter-cli");
    let process_ok = finish.wait_error.is_none()
        && finish
            .exit_status
            .as_ref()
            .map(|status| status.success())
            .unwrap_or(false);
    if !process_ok {
        let contexts = ErrorContexts::build(
            &finish.stderr_tail,
            &stdout_tail,
            &response_content,
            &last_error_detail,
            STDERR_TAIL_MAX_LINES,
        );
        let detail =
            contexts.best_failure_detail(finish.exit_status.as_ref(), finish.wait_error.as_deref());
        let response = match error_message {
            Some(message) => format!("openrouter-cli command failed: {}{}", message, detail),
            None => format!("openrouter-cli command failed.{}", detail),
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

    let _ = tx.send(StreamMessage {
        response: response_content,
        command: command_str,
        session_id: extracted_session_id,
        summary: Some(aggregated_summary),
        is_final: true,
    });
    Ok(())
}
