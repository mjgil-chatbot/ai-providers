use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;

use crate::config::{add_prompt_arg, prompt_needs_hyphen_guard, QwenCodeCliConfig};
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::{wrap_command_string_with_runsc, wrap_command_with_runsc};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::{format_tool_input_preview, JsonSummary, ToolCallRecord};
use crate::workspace;

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

pub fn call_qwen_code_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &QwenCodeCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting qwen-code stream");

    let session_id_to_resume = session
        .as_ref()
        .and_then(|s| s.qwen_session_id.as_ref())
        .or(config.resume_session_id.as_ref());

    let prompt = build_qwen_code_prompt(system_prompt, query, session.as_ref(), memory_context);

    let mut cmd = Command::new(&config.cli);
    workspace::apply_session_working_dir(&mut cmd, session.as_ref());

    for (key, value) in &config.extra_env {
        cmd.env(key, value);
    }

    cmd.arg("--output-format").arg(&config.output_format);

    if config.yolo {
        cmd.arg("--yolo");
    }

    if let Some(model) = config.model.as_ref() {
        cmd.arg("--model").arg(model);
    }

    if let Some(session_id) = session_id_to_resume {
        cmd.arg("--resume").arg(session_id);
    }

    if let Some(auth_type) = config.auth_type.as_ref() {
        cmd.arg("--auth-type").arg(auth_type);
    }

    // Pass system prompt via --append-system-prompt only on new sessions.
    if session_id_to_resume.is_none() {
        let effective_system_prompt = match (config.append_system_prompt.as_deref(), system_prompt)
        {
            (Some(default), Some(session))
                if !default.trim().is_empty() && !session.trim().is_empty() =>
            {
                Some(format!("{}\n\n{}", default.trim(), session.trim()))
            }
            (Some(default), _) if !default.trim().is_empty() => Some(default.trim().to_string()),
            (_, Some(session)) if !session.trim().is_empty() => Some(session.trim().to_string()),
            _ => None,
        };
        if let Some(sys_prompt) = effective_system_prompt.as_deref() {
            cmd.arg("--append-system-prompt").arg(sys_prompt);
        }
    }

    for dir in &config.include_directories {
        cmd.arg("--include-directories").arg(dir);
    }

    if !config.allowed_mcp_server_names.is_empty() {
        cmd.arg("--allowed-mcp-server-names")
            .arg(config.allowed_mcp_server_names.join(","));
    }

    add_prompt_arg(&mut cmd, &prompt);
    let runsc_workspace_sync = wrap_command_with_runsc(
        &mut cmd,
        config.runsc_image.as_deref(),
        &config.runsc_workspace_includes,
        &config.runsc_mounts,
    )?;

    let command_str = build_qwen_code_command_string(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config,
    );
    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "qwen-code",
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
    if let Some(model) = config.model.as_ref() {
        aggregated_summary.model = Some(model.clone());
    }

    for line in reader.lines() {
        if cli.is_stopped() {
            warn!("stop signal triggered during qwen-code stream read");
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
                error!(error=%e, "error reading qwen-code stdout stream");
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
                // Extract session_id from any event (it's in the outer wrapper).
                if extracted_session_id.is_none() {
                    if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                        extracted_session_id = Some(sid.to_string());
                        aggregated_summary.session_id = extracted_session_id.clone();
                    }
                }

                let event_type = json
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                match event_type {
                    "system" => {
                        // Initial system message — session_id already extracted above.
                    }
                    "stream_event" => {
                        if let Some(event) = json.get("event") {
                            let inner_type = event
                                .get("type")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default();

                            match inner_type {
                                "content_block_delta" => {
                                    if let Some(delta) = event.get("delta") {
                                        let delta_type = delta
                                            .get("type")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default();
                                        if delta_type == "text_delta" {
                                            if let Some(text) =
                                                delta.get("text").and_then(|v| v.as_str())
                                            {
                                                response_content.push_str(text);
                                                let _ = tx.send(StreamMessage {
                                                    response: response_content.clone(),
                                                    command: command_str.clone(),
                                                    session_id: extracted_session_id.clone(),
                                                    summary: Some(aggregated_summary.clone()),
                                                    is_final: false,
                                                });
                                            }
                                        }
                                    }
                                }
                                "content_block_start" => {
                                    if let Some(block) = event.get("content_block") {
                                        if block.get("type").and_then(|v| v.as_str())
                                            == Some("tool_use")
                                        {
                                            let name = block
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            if !name.is_empty()
                                                && !aggregated_summary.tools_used.contains(&name)
                                            {
                                                aggregated_summary.tools_used.push(name.clone());
                                            }
                                            let input_preview = block
                                                .get("input")
                                                .map(|value| {
                                                    format_tool_input_preview(&name, value)
                                                })
                                                .unwrap_or_default();
                                            aggregated_summary.pending_tool_calls.push(
                                                ToolCallRecord {
                                                    name,
                                                    input_preview,
                                                    ..Default::default()
                                                },
                                            );
                                        }
                                    }
                                }
                                "message_start" => {
                                    // Extract model from message if available.
                                    if let Some(message) = event.get("message") {
                                        if let Some(model) =
                                            message.get("model").and_then(|v| v.as_str())
                                        {
                                            aggregated_summary.model = Some(model.to_string());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    "result" => {
                        // Final result event with usage stats.
                        let is_error = json
                            .get("is_error")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);

                        if is_error {
                            if let Some(err) = json.get("error") {
                                error_message = err
                                    .get("message")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                            }
                            if error_message.is_none() {
                                error_message = json
                                    .get("result")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string);
                            }
                        }

                        // Also capture the result text if present and we have no response yet.
                        if response_content.is_empty() {
                            if let Some(result) = json.get("result").and_then(|v| v.as_str()) {
                                if !is_error {
                                    response_content = result.to_string();
                                }
                            }
                        }

                        // Extract session_id from result as well.
                        if let Some(sid) = json.get("session_id").and_then(|v| v.as_str()) {
                            extracted_session_id = Some(sid.to_string());
                            aggregated_summary.session_id = extracted_session_id.clone();
                        }

                        // Extract usage statistics.
                        if let Some(usage) = json.get("usage") {
                            if let Some(input) = usage.get("input_tokens").and_then(|v| v.as_u64())
                            {
                                aggregated_summary.total_input_tokens = input;
                            }
                            if let Some(output) =
                                usage.get("output_tokens").and_then(|v| v.as_u64())
                            {
                                aggregated_summary.total_output_tokens = output;
                            }
                            if let Some(cache_create) = usage
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                            {
                                aggregated_summary.cache_creation_tokens = cache_create;
                            }
                            if let Some(cache_read) = usage
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                            {
                                aggregated_summary.cache_read_tokens = cache_read;
                            }
                        }

                        // Extract duration.
                        if let Some(duration) = json.get("duration_ms").and_then(|v| v.as_u64()) {
                            aggregated_summary.total_duration_ms = duration;
                        }

                        // Extract num_turns as response_count.
                        if let Some(turns) = json.get("num_turns").and_then(|v| v.as_u64()) {
                            aggregated_summary.response_count = turns as u32;
                        }

                        // Extract context window from modelUsage if available.
                        if let Some(model_usage) = json.get("modelUsage") {
                            if let Some(obj) = model_usage.as_object() {
                                for (_model, stats) in obj {
                                    if let Some(cw) =
                                        stats.get("contextWindow").and_then(|v| v.as_u64())
                                    {
                                        aggregated_summary.context_window_size = cw;
                                    }
                                }
                            }
                        }

                        let response = if is_error {
                            error_message
                                .clone()
                                .unwrap_or_else(|| "qwen-code returned an error.".to_string())
                        } else {
                            response_content.clone()
                        };

                        let _ = tx.send(StreamMessage {
                            response,
                            command: command_str.clone(),
                            session_id: extracted_session_id.clone(),
                            summary: Some(aggregated_summary.clone()),
                            is_final: true,
                        });
                        let _ = cli.finish("qwen-code");
                        runsc_workspace_sync.sync_back()?;
                        return Ok(());
                    }
                    _ => {
                        // Check for error details in unknown event types.
                        if let Some(detail) = stream::extract_error_detail(&json) {
                            last_error_detail = Some(detail.clone());
                            error_message = Some(detail);
                        }
                    }
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

    // Stream ended without a result event — check process exit.
    let finish = cli.finish("qwen-code");
    runsc_workspace_sync.sync_back()?;
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
            Some(message) => format!("qwen-code command failed: {}{}", message, detail),
            None => format!("qwen-code command failed.{}", detail),
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
