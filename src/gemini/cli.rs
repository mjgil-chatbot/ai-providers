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

use crate::config::{add_prompt_arg, format_prompt_arg, GeminiCliConfig};
use crate::live_summary_formatter::LiveSummaryFormatter;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::{wrap_command_string_with_runsc, wrap_command_with_runsc};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::{format_tool_input_preview, JsonSummary, ToolCallRecord};
use crate::workspace;

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

fn fallback_gemini_include_directory(session: Option<&ProviderSession>) -> Option<String> {
    workspace::session_working_dir(session)
        .filter(|dir| !is_disallowed_gemini_include_dir(dir))
        .map(ToString::to_string)
        .or_else(|| {
            std::env::current_dir().ok().and_then(|path| {
                if !cfg!(windows) && path == Path::new("/") {
                    None
                } else {
                    Some(path.to_string_lossy().into_owned())
                }
            })
        })
}

fn gemini_include_directories(
    session: Option<&ProviderSession>,
    config: &GeminiCliConfig,
) -> Vec<String> {
    let configured = if !config.include_directories.is_empty() {
        config.include_directories.clone()
    } else {
        match std::env::var("GEMINI_INCLUDE_DIRECTORIES") {
            Ok(value) if !value.trim().is_empty() => value
                .split(',')
                .map(|part| part.trim())
                .filter(|part| !part.is_empty())
                .map(|part| part.to_string())
                .collect(),
            _ => Vec::new(),
        }
    };

    let filtered: Vec<String> = configured
        .into_iter()
        .filter(|dir| !is_disallowed_gemini_include_dir(dir))
        .collect();
    if !filtered.is_empty() {
        filtered
    } else {
        fallback_gemini_include_directory(session)
            .into_iter()
            .collect()
    }
}

pub fn build_gemini_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &GeminiCliConfig,
) -> String {
    let session_id_to_resume = session.and_then(|value| value.gemini_session_id.as_ref());
    let prompt = build_gemini_prompt(
        system_prompt,
        query,
        session,
        session_id_to_resume.is_some(),
        memory_context,
    );
    let mut parts = vec![config.cli.clone()];
    parts.push(format!("--output-format {}", config.output_format));
    for dir in gemini_include_directories(session, config) {
        parts.push(format!("--include-directories {}", dir));
    }
    if let Some(model) = config.model.as_deref() {
        parts.push(format!("--model {}", model));
    }
    if let Some(mode) = config.approval_mode.as_deref() {
        parts.push(format!("--approval-mode {}", mode));
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

pub fn call_gemini_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &GeminiCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting gemini cli stream");

    let session_id_to_resume = session
        .as_ref()
        .and_then(|value| value.gemini_session_id.as_ref());
    let prompt = build_gemini_prompt(
        system_prompt,
        query,
        session.as_ref(),
        session_id_to_resume.is_some(),
        memory_context,
    );

    let mut cmd = Command::new(&config.cli);
    workspace::apply_session_working_dir(&mut cmd, session.as_ref());
    for (key, value) in &config.extra_env {
        cmd.env(key, value);
    }
    cmd.arg("--output-format").arg(&config.output_format);
    for dir in gemini_include_directories(session.as_ref(), config) {
        cmd.arg("--include-directories").arg(dir);
    }
    if let Some(model) = config.model.as_deref() {
        cmd.arg("--model").arg(model);
    }
    if let Some(mode) = config.approval_mode.as_deref() {
        cmd.arg("--approval-mode").arg(mode);
    }
    if let Some(session_id) = session_id_to_resume {
        cmd.arg("--resume").arg(session_id);
    }
    add_prompt_arg(&mut cmd, &prompt);
    if let Some(path) = config.project_settings_path.as_ref() {
        if path.is_file() {
            cmd.env("GEMINI_CLI_PROJECT_SETTINGS_PATH", path);
        }
    }
    let runsc_workspace_sync = wrap_command_with_runsc(
        &mut cmd,
        config.runsc_image.as_deref(),
        &config.runsc_workspace_includes,
        &config.runsc_mounts,
    )?;

    let command_str = build_gemini_command_string(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config,
    );
    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "gemini",
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
    let mut extracted_session_id: Option<String> = None;
    let mut last_error_detail: Option<String> = None;
    let mut stdout_tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_MAX_LINES);
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
                let line_summary =
                    generate_gemini_line_summary(&json, aggregated_summary.model.as_deref());
                if !line_summary.is_empty() {
                    if !json_summaries.is_empty() {
                        json_summaries.push('\n');
                    }
                    json_summaries.push_str(&line_summary);
                }
                match json.get("type").and_then(|value| value.as_str()) {
                    Some("init") => {
                        extracted_session_id = json
                            .get("session_id")
                            .and_then(|value| value.as_str())
                            .map(|value| value.to_string());
                        aggregated_summary.session_id = extracted_session_id.clone();
                        aggregated_summary.model = json
                            .get("model")
                            .and_then(|value| value.as_str())
                            .map(|value| value.to_string())
                            .or_else(|| aggregated_summary.model.clone());
                    }
                    Some("message") => {
                        if json.get("role").and_then(|value| value.as_str()) == Some("assistant") {
                            if let Some(content) = extract_gemini_message_content(&json) {
                                let is_delta = json
                                    .get("delta")
                                    .and_then(|value| value.as_bool())
                                    .unwrap_or(false);
                                if is_delta {
                                    response_content.push_str(&content);
                                } else {
                                    response_content = content;
                                }
                            }
                        }
                    }
                    Some("tool_use") => {
                        record_gemini_tool_call(&mut aggregated_summary, &json);
                    }
                    Some("result") => {
                        aggregated_summary.response_count += 1;
                        if let Some(stats) = json.get("stats") {
                            aggregated_summary.total_input_tokens = extract_u64(
                                stats,
                                &[
                                    "input_tokens",
                                    "inputTokens",
                                    "prompt_tokens",
                                    "promptTokens",
                                ],
                            )
                            .unwrap_or(aggregated_summary.total_input_tokens);
                            aggregated_summary.total_output_tokens = extract_u64(
                                stats,
                                &[
                                    "output_tokens",
                                    "outputTokens",
                                    "completion_tokens",
                                    "completionTokens",
                                ],
                            )
                            .unwrap_or(aggregated_summary.total_output_tokens);
                        }
                    }
                    _ => {}
                }

                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail);
                }

                let stream_view =
                    LIVE_SUMMARY_FORMATTER.compose_response(&json_summaries, &response_content);

                let _ = tx.send(StreamMessage {
                    response: stream_view,
                    command: command_str.clone(),
                    session_id: extracted_session_id.clone(),
                    summary: None,
                    is_final: false,
                });
            }
            Err(_) => {
                if stdout_tail.len() == STDERR_TAIL_MAX_LINES {
                    stdout_tail.pop_front();
                }
                stdout_tail.push_back(line.clone());
            }
        }
    }

    let finish = cli.finish("gemini");
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
        let _ = tx.send(StreamMessage {
            response: format!("Gemini command failed.{}", detail),
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

fn generate_gemini_line_summary(json: &Value, current_model: Option<&str>) -> String {
    match json.get("type").and_then(|value| value.as_str()) {
        Some("init") => LIVE_SUMMARY_FORMATTER.started_line(
            json.get("model")
                .and_then(|value| value.as_str())
                .or(current_model),
        ),
        Some("tool_use") => {
            let Some(name) = json.get("name").and_then(|value| value.as_str()) else {
                return LIVE_SUMMARY_FORMATTER.detail_line(current_model, "tool call");
            };
            let detail = LIVE_SUMMARY_FORMATTER.tool_detail(name, json);
            LIVE_SUMMARY_FORMATTER.detail_line(current_model, &detail)
        }
        Some("result") => LIVE_SUMMARY_FORMATTER.detail_line(current_model, "response complete"),
        _ => String::new(),
    }
}

fn extract_gemini_message_content(json: &Value) -> Option<String> {
    if let Some(content) = json.get("content").and_then(|value| value.as_str()) {
        return Some(content.to_string());
    }
    if let Some(parts) = json.get("content").and_then(|value| value.as_array()) {
        let out = parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|value| value.as_str())
                    .or_else(|| part.as_str())
            })
            .collect::<Vec<_>>()
            .join("");
        if !out.is_empty() {
            return Some(out);
        }
    }
    None
}

fn extract_u64(value: &Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(value) = value.get(*key).and_then(|value| value.as_u64()) {
            return Some(value);
        }
    }
    None
}

fn record_gemini_tool_call(summary: &mut JsonSummary, json: &Value) {
    let Some(record) = extract_gemini_tool_call(json) else {
        return;
    };
    if !summary.tools_used.contains(&record.name) {
        summary.tools_used.push(record.name.clone());
    }
    summary.pending_tool_calls.push(record);
}

fn extract_gemini_tool_call(json: &Value) -> Option<ToolCallRecord> {
    let name = json.get("name").and_then(|value| value.as_str())?;
    Some(ToolCallRecord {
        name: name.to_string(),
        input_preview: format_tool_input_preview(name, json),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::{build_gemini_prompt, gemini_include_directories, generate_gemini_line_summary};
    use crate::config::GeminiCliConfig;
    use crate::session::{ProviderMessage, ProviderSession};
    use serde_json::json;

    #[test]
    fn prompt_replay_keeps_full_assistant_message() {
        let session = ProviderSession {
            messages: vec![
                ProviderMessage {
                    content: "Initial question".to_string(),
                    is_user: true,
                    history_content: None,
                },
                ProviderMessage {
                    content: "First paragraph.\n\n```rust\nfn main() {}\n```\n\nFinal paragraph."
                        .to_string(),
                    is_user: false,
                    history_content: None,
                },
            ],
            ..Default::default()
        };

        let prompt = build_gemini_prompt(None, "Follow-up", Some(&session), false, None);

        assert!(prompt.contains("First paragraph."));
        assert!(prompt.contains("fn main() {}"));
        assert!(prompt.contains("Final paragraph."));
    }

    #[test]
    fn include_directories_filters_root_and_falls_back_to_session_workspace() {
        let session = ProviderSession {
            working_dir: Some("/tmp/gemini-workspace".to_string()),
            ..Default::default()
        };
        let config = GeminiCliConfig {
            include_directories: vec!["/".to_string()],
            extra_env: Vec::new(),
            ..Default::default()
        };

        assert_eq!(
            gemini_include_directories(Some(&session), &config),
            vec!["/tmp/gemini-workspace".to_string()]
        );
    }

    #[test]
    fn gemini_line_summary_uses_shared_model_labeling() {
        let summary = generate_gemini_line_summary(
            &json!({
                "type": "init",
                "model": "gpt-5.4-high"
            }),
            None,
        );
        assert_eq!(summary, "GPT-5.4-High agent started");
    }
}
