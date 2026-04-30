use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;

use super::live_summary::{compose_live_response, generate_codex_line_summary};
use super::session_store::select_codex_home_for_resume;
use crate::claude::compute_cost;
use crate::config::{prompt_needs_hyphen_guard, CodexCliConfig};
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::{wrap_command_string_with_runsc, wrap_command_with_runsc};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::{format_tool_input_preview, JsonSummary, ToolCallRecord};
use crate::workspace;

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

pub fn build_codex_command_string(
    system_prompt: Option<&str>,
    session_id: Option<&str>,
    query: &str,
    working_dir: Option<&str>,
    config: &CodexCliConfig,
) -> String {
    let prompt = build_codex_prompt(system_prompt, query, session_id, None);
    let mut parts = vec![config.cli.clone(), "exec".to_string()];
    if let Some(mode) = config.sandbox_mode.as_deref() {
        parts.push(format!("--sandbox {}", mode));
    }
    if let Some(effort) = config.reasoning_effort.as_deref() {
        parts.push(format!("-c reasoning_effort=\"{}\"", effort));
    }
    if config.json_stream {
        parts.push("--json".to_string());
    }
    if let Some(model) = config.model.as_deref() {
        parts.push(format!("--model {}", model));
    }
    if let Some(schema_path) = config.output_schema.as_ref() {
        parts.push(format!("--output-schema {}", schema_path.display()));
    }
    parts.push("--skip-git-repo-check".to_string());
    if let Some(session_id) = session_id {
        parts.push("resume".to_string());
        parts.push(session_id.to_string());
    }
    if prompt_needs_hyphen_guard(&prompt) {
        parts.push("--".to_string());
    }
    parts.push(format!("\"{}\"", prompt));
    wrap_command_string_with_runsc(parts.join(" "), working_dir, config.runsc_image.as_deref())
}

pub fn call_codex_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &CodexCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting codex cli stream");

    let session_id_to_resume = session
        .as_ref()
        .and_then(|value| value.codex_session_id.as_ref());
    let prompt = build_codex_prompt(
        system_prompt,
        query,
        session_id_to_resume.map(|value| value.as_str()),
        memory_context,
    );

    let mut cmd = Command::new(&config.cli);
    workspace::apply_session_working_dir(&mut cmd, session.as_ref());
    let effective_codex_home = select_codex_home_for_resume(
        configured_codex_home(config).as_deref(),
        session_id_to_resume.map(|value| value.as_str()),
    );
    for (key, value) in &config.extra_env {
        if key == "CODEX_HOME" && effective_codex_home.is_some() {
            continue;
        }
        cmd.env(key, value);
    }
    if let Some(codex_home) = effective_codex_home.as_ref() {
        cmd.env("CODEX_HOME", codex_home);
    }
    cmd.arg("exec");
    if let Some(mode) = config.sandbox_mode.as_deref() {
        cmd.arg("--sandbox").arg(mode);
    }
    if let Some(effort) = config.reasoning_effort.as_deref() {
        cmd.arg("-c")
            .arg(format!("reasoning_effort=\"{}\"", effort));
    }
    if config.json_stream {
        cmd.arg("--json");
    }
    if let Some(model) = config.model.as_deref() {
        cmd.arg("--model").arg(model);
    }
    if let Some(schema_path) = config.output_schema.as_ref() {
        cmd.arg("--output-schema").arg(schema_path);
    }
    cmd.arg("--skip-git-repo-check");
    if let Some(session_id) = session_id_to_resume {
        cmd.arg("resume").arg(session_id);
    }
    if prompt_needs_hyphen_guard(&prompt) {
        cmd.arg("--");
    }
    cmd.arg(&prompt);
    let runsc_workspace_sync = wrap_command_with_runsc(
        &mut cmd,
        config.runsc_image.as_deref(),
        &config.runsc_workspace_includes,
        &config.runsc_mounts,
    )?;

    let command_str = build_codex_command_string(
        system_prompt,
        session_id_to_resume.map(|value| value.as_str()),
        query,
        session
            .as_ref()
            .and_then(|value| value.working_dir.as_deref()),
        config,
    );

    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "codex",
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
    let mut streamed_summary = String::new();
    let mut actual_text_content = String::new();
    let mut last_stream_payload = String::new();
    let mut aggregated_summary = JsonSummary::new();
    if let Some(model) = config.model.as_deref() {
        aggregated_summary.model = Some(model.to_string());
    }

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
            Err(error) => {
                error!(error=%error, "error reading stdout stream");
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

        match serde_json::from_str::<Value>(&line) {
            Ok(json) => {
                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail.clone());
                    error_message = Some(detail);
                }
                if let Some(summary_line) =
                    generate_codex_line_summary(&json, aggregated_summary.model.as_deref())
                {
                    if !streamed_summary.is_empty() {
                        streamed_summary.push('\n');
                    }
                    streamed_summary.push_str(&summary_line);
                }
                match json.get("type").and_then(|value| value.as_str()) {
                    Some("thread.started") => {
                        extracted_session_id = json
                            .get("thread_id")
                            .and_then(|value| value.as_str())
                            .map(|value| value.to_string());
                        aggregated_summary.session_id = extracted_session_id.clone();
                    }
                    Some("item.completed") => {
                        if let Some(item) = json.get("item") {
                            handle_completed_item(
                                &mut aggregated_summary,
                                item,
                                &mut actual_text_content,
                            );
                        }
                    }
                    Some("turn.completed") => {
                        update_summary_from_turn(&mut aggregated_summary, &json);
                    }
                    _ => {}
                }

                response_content = compose_live_response(&streamed_summary, &actual_text_content);
                if response_content != last_stream_payload {
                    last_stream_payload = response_content.clone();
                    let _ = tx.send(StreamMessage {
                        response: response_content.clone(),
                        command: command_str.clone(),
                        session_id: extracted_session_id.clone(),
                        summary: Some(aggregated_summary.clone()),
                        is_final: false,
                    });
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

    info!(
        response_len = response_content.len(),
        has_session_id = extracted_session_id.is_some(),
        summary_response_count = aggregated_summary.response_count,
        pending_tool_calls = aggregated_summary.pending_tool_calls.len(),
        stdout_tail_lines = stdout_tail.len(),
        "codex stdout stream closed; waiting for cli process exit"
    );
    let finish = cli.finish("codex");
    runsc_workspace_sync.sync_back()?;
    let process_ok = finish.wait_error.is_none()
        && finish
            .exit_status
            .as_ref()
            .map(|status| status.success())
            .unwrap_or(false);
    info!(
        process_ok,
        exit_status = ?finish.exit_status,
        wait_error = ?finish.wait_error,
        stderr_tail_len = finish.stderr_tail.len(),
        response_len = response_content.len(),
        has_session_id = extracted_session_id.is_some(),
        summary_response_count = aggregated_summary.response_count,
        pending_tool_calls = aggregated_summary.pending_tool_calls.len(),
        "codex cli process exit observed"
    );
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
            Some(message) => format!("Codex command failed: {}{}", message, detail),
            None => format!("Codex command failed.{}", detail),
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
        response: if actual_text_content.is_empty() {
            response_content
        } else {
            actual_text_content
        },
        command: command_str,
        session_id: extracted_session_id,
        summary: Some(aggregated_summary),
        is_final: true,
    });
    Ok(())
}

fn extract_codex_item_text(item: &Value) -> Option<String> {
    item.get("text")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string())
        .or_else(|| {
            item.get("content")
                .and_then(|value| value.as_array())
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|part| part.get("text").and_then(|value| value.as_str()))
                        .collect::<Vec<_>>()
                        .join("")
                })
                .filter(|value| !value.is_empty())
        })
}

fn record_codex_tool_call(summary: &mut JsonSummary, item: &Value) {
    let name = item
        .get("call")
        .and_then(|value| value.get("name"))
        .or_else(|| item.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or("tool")
        .to_string();
    if !summary.tools_used.contains(&name) {
        summary.tools_used.push(name.clone());
    }
    summary.pending_tool_calls.push(ToolCallRecord {
        input_preview: format_tool_input_preview(&name, item),
        name,
        ..Default::default()
    });
}

fn handle_completed_item(summary: &mut JsonSummary, item: &Value, response_content: &mut String) {
    let item_type = item.get("type").and_then(|value| value.as_str());
    if item_type == Some("agent_message") {
        if let Some(text) = extract_codex_item_text(item) {
            *response_content = text;
        }
    }
    if item_type == Some("command_execution") || item_type == Some("mcp_tool_call") {
        record_codex_tool_call(summary, item);
    }
    if item_type == Some("context_compaction") {
        summary.num_compactions += 1;
    }
}

fn update_summary_from_turn(summary: &mut JsonSummary, json: &Value) {
    summary.response_count += 1;
    let mut input_tokens = summary.total_input_tokens;
    let mut output_tokens = summary.total_output_tokens;
    let mut cache_creation_tokens = summary.cache_creation_tokens;
    let mut cache_read_tokens = summary.cache_read_tokens;

    if let Some(usage) = json.get("usage") {
        input_tokens = usage
            .get("input_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(input_tokens);
        output_tokens = usage
            .get("output_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(output_tokens);
        cache_creation_tokens = usage
            .get("cache_creation_input_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(cache_creation_tokens);
        cache_read_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(|value| value.as_u64())
            .unwrap_or(cache_read_tokens);
    }
    summary.total_input_tokens = input_tokens;
    summary.total_output_tokens = output_tokens;
    summary.cache_creation_tokens = cache_creation_tokens;
    summary.cache_read_tokens = cache_read_tokens;

    // Compute cost from accumulated tokens
    summary.total_cost_usd = compute_cost(
        summary.model.as_deref(),
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
    );

    // Track context window
    if let Some(usage) = json.get("usage") {
        if let Some(ctx_tokens) = usage.get("context_window_tokens").and_then(|v| v.as_u64()) {
            summary.context_window_tokens = ctx_tokens;
        }
        if let Some(ctx_size) = usage.get("context_window_size").and_then(|v| v.as_u64()) {
            summary.context_window_size = ctx_size;
        }
    }
    if let Some(estimated_tokens) = json.get("estimated_token_count").and_then(|v| v.as_u64()) {
        summary.context_window_tokens = estimated_tokens;
    }
    if let Some(model_ctx) = json.get("model_context_window").and_then(|v| v.as_u64()) {
        summary.context_window_size = model_ctx;
    } else if summary.context_window_size == 0 {
        if let Some(limit) = json.get("auto_compact_limit").and_then(|v| v.as_u64()) {
            // auto_compact_limit is ~95% of the model context window.
            summary.context_window_size = (limit as f64 / 0.95).round() as u64;
        }
    }

    summary.total_duration_ms += json
        .get("duration_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn completed_context_compaction_items_increment_compaction_count() {
        let mut summary = JsonSummary::new();
        let mut response_content = String::new();

        handle_completed_item(
            &mut summary,
            &json!({
                "type": "context_compaction"
            }),
            &mut response_content,
        );

        assert_eq!(summary.num_compactions, 1);
        assert!(response_content.is_empty());
    }

    #[test]
    fn turn_completed_uses_estimated_token_count_as_current_context() {
        let mut summary = JsonSummary::new();
        summary.model = Some("gpt-5.4".to_string());

        update_summary_from_turn(
            &mut summary,
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 1200,
                    "output_tokens": 300,
                    "cache_creation_input_tokens": 40,
                    "cache_read_input_tokens": 100
                },
                "estimated_token_count": 171879,
                "auto_compact_limit": 244800,
                "token_limit_reached": false,
                "duration_ms": 1500
            }),
        );

        assert_eq!(summary.total_input_tokens, 1200);
        assert_eq!(summary.total_output_tokens, 300);
        assert_eq!(summary.cache_creation_tokens, 40);
        assert_eq!(summary.cache_read_tokens, 100);
        assert_eq!(summary.context_window_tokens, 171_879);
        // auto_compact_limit 244800 / 0.95 ≈ 257684
        assert_eq!(summary.context_window_size, 257_684);
        assert_eq!(summary.response_count, 1);
        assert_eq!(summary.total_duration_ms, 1500);
    }

    #[test]
    fn turn_completed_prefers_model_context_window_over_auto_compact_limit() {
        let mut summary = JsonSummary::new();

        update_summary_from_turn(
            &mut summary,
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 500,
                    "output_tokens": 100
                },
                "estimated_token_count": 6000,
                "model_context_window": 258400,
                "auto_compact_limit": 244800,
                "duration_ms": 200
            }),
        );

        assert_eq!(summary.context_window_tokens, 6_000);
        assert_eq!(summary.context_window_size, 258_400);
    }

    #[test]
    fn turn_completed_without_estimated_token_count_preserves_existing_context() {
        let mut summary = JsonSummary::new();
        summary.context_window_tokens = 50_000;

        update_summary_from_turn(
            &mut summary,
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 200
                },
                "model_context_window": 258400,
                "duration_ms": 100
            }),
        );

        // Without estimated_token_count, existing value should be preserved
        assert_eq!(summary.context_window_tokens, 50_000);
        assert_eq!(summary.context_window_size, 258_400);
    }

    #[test]
    fn turn_completed_accumulates_context_across_multiple_turns() {
        let mut summary = JsonSummary::new();

        // First turn
        update_summary_from_turn(
            &mut summary,
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 200
                },
                "estimated_token_count": 9152,
                "auto_compact_limit": 244800,
                "duration_ms": 500
            }),
        );

        assert_eq!(summary.context_window_tokens, 9_152);
        assert_eq!(summary.context_window_size, 257_684);

        // Second turn — context grows
        update_summary_from_turn(
            &mut summary,
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 2000,
                    "output_tokens": 400
                },
                "estimated_token_count": 18341,
                "auto_compact_limit": 244800,
                "duration_ms": 600
            }),
        );

        assert_eq!(summary.context_window_tokens, 18_341);
        assert_eq!(summary.response_count, 2);
        assert_eq!(summary.total_duration_ms, 1100);
    }

    #[test]
    fn build_codex_command_string_skips_git_repo_check_in_runsc_mode() {
        let config = CodexCliConfig {
            cli: "codex".to_string(),
            model: Some("gpt-5.4".to_string()),
            output_schema: None,
            json_stream: true,
            reasoning_effort: None,
            sandbox_mode: None,
            force_skip_git_repo_check: false,
            skip_git_repo_check_prefixes: Vec::new(),
            extra_env: Vec::new(),
            runsc_image: Some("runsc-image:latest".to_string()),
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        };

        let command =
            build_codex_command_string(None, None, "draft prompt", Some("/tmp/workspace"), &config);

        assert!(command.contains("--skip-git-repo-check"));
    }

    #[test]
    fn build_codex_command_string_skips_git_repo_check_when_forced() {
        let config = CodexCliConfig {
            cli: "codex".to_string(),
            model: Some("gpt-5.4".to_string()),
            output_schema: None,
            json_stream: true,
            reasoning_effort: None,
            sandbox_mode: None,
            force_skip_git_repo_check: true,
            skip_git_repo_check_prefixes: Vec::new(),
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        };

        let command =
            build_codex_command_string(None, None, "draft prompt", Some("/tmp/non-repo"), &config);

        assert!(command.contains("--skip-git-repo-check"));
    }

    #[test]
    fn build_codex_command_string_skips_git_repo_check_by_default() {
        let config = CodexCliConfig {
            cli: "codex".to_string(),
            model: Some("gpt-5.4".to_string()),
            output_schema: None,
            json_stream: true,
            reasoning_effort: None,
            sandbox_mode: None,
            force_skip_git_repo_check: false,
            skip_git_repo_check_prefixes: Vec::new(),
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        };

        let command =
            build_codex_command_string(None, None, "draft prompt", Some("/tmp/workspace"), &config);

        assert!(command.contains("--skip-git-repo-check"));
    }

    #[test]
    fn build_codex_command_string_includes_output_schema_flag() {
        let config = CodexCliConfig {
            cli: "codex".to_string(),
            model: Some("gpt-5.4".to_string()),
            output_schema: Some(std::path::PathBuf::from("/tmp/contract-schema.json")),
            json_stream: true,
            reasoning_effort: None,
            sandbox_mode: None,
            force_skip_git_repo_check: false,
            skip_git_repo_check_prefixes: Vec::new(),
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        };

        let command =
            build_codex_command_string(None, None, "draft prompt", Some("/tmp/workspace"), &config);

        assert!(command.contains("--output-schema /tmp/contract-schema.json"));
    }
}
