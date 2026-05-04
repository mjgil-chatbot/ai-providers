use std::borrow::Cow;
use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;

use crate::config::{add_prompt_arg, format_prompt_arg, KimiCliConfig};
use crate::live_summary_formatter::LiveSummaryFormatter;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::sandbox::{wrap_command_string_with_runsc, wrap_command_with_runsc};
use crate::session::ProviderSession;
use crate::stream::{self, ErrorContexts, StreamMessage};
use crate::summary::{JsonSummary, ToolCallRecord};
use crate::workspace;

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

pub fn call_kimi_cli_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &KimiCliConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    info!(query_len = query.len(), "starting kimi cli stream");

    let session_id_to_resume = session
        .as_ref()
        .and_then(|value| value.kimi_session_id.as_ref());
    let prompt = build_kimi_prompt(
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
    cmd.arg("--print");
    cmd.arg("--output-format").arg(&config.output_format);
    if config.yolo {
        cmd.arg("--yolo");
    }
    if let Some(model) = config.model.as_deref() {
        cmd.arg("--model").arg(model);
    }
    if let Some(session_id) = session_id_to_resume {
        cmd.arg("--resume").arg(session_id);
    }
    add_prompt_arg(&mut cmd, &prompt);

    let runsc_workspace_sync = wrap_command_with_runsc(
        &mut cmd,
        config.runsc_image.as_deref(),
        &config.runsc_workspace_includes,
        &config.runsc_mounts,
    )?;

    let command_str = build_kimi_command_string(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config,
    );
    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "kimi",
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
                let line_summary = generate_kimi_line_summary(&json);
                if !line_summary.is_empty() {
                    if !json_summaries.is_empty() {
                        json_summaries.push('\n');
                    }
                    json_summaries.push_str(&line_summary);
                }

                if let Some(content) = extract_kimi_text_content(&json) {
                    response_content.push_str(&content);
                }

                if let Some(tool) = extract_kimi_tool_call(&json) {
                    if !aggregated_summary.tools_used.contains(&tool.name) {
                        aggregated_summary.tools_used.push(tool.name.clone());
                    }
                    aggregated_summary.pending_tool_calls.push(tool);
                }

                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail);
                }

                let stream_view =
                    LIVE_SUMMARY_FORMATTER.compose_response(&json_summaries, &response_content);

                let _ = tx.send(StreamMessage {
                    response: stream_view,
                    command: command_str.clone(),
                    session_id: None,
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

    let finish = cli.finish("kimi");
    runsc_workspace_sync.sync_back()?;

    // Extract session ID from stderr tail (e.g. "To resume this session: kimi -r <uuid>")
    let extracted_session_id = extract_kimi_session_id_from_stderr(&finish.stderr_tail);
    if let Some(ref sid) = extracted_session_id {
        aggregated_summary.session_id = Some(sid.clone());
    }

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
            response: format!("Kimi command failed.{}", detail),
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

fn extract_kimi_text_content(json: &Value) -> Option<String> {
    let role = json.get("role")?.as_str()?;
    if role != "assistant" {
        return None;
    }
    if json
        .get("tool_calls")
        .and_then(|value| value.as_array())
        .is_some_and(|tool_calls| !tool_calls.is_empty())
    {
        return None;
    }
    let content = json.get("content")?;
    let parts = content.as_array()?;
    let mut out = String::new();
    for part in parts {
        if part.get("type").and_then(|v| v.as_str()) != Some("text") {
            continue;
        }
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            out.push_str(text);
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn extract_kimi_tool_call(json: &Value) -> Option<ToolCallRecord> {
    let role = json.get("role")?.as_str()?;
    if role != "assistant" {
        return None;
    }
    let tool_calls = json.get("tool_calls")?.as_array()?;
    let first = tool_calls.first()?;
    let function = first.get("function")?;
    let name = function.get("name")?.as_str()?;
    Some(ToolCallRecord {
        name: name.to_string(),
        input_preview: crate::summary::format_tool_input_preview(name, function),
        ..Default::default()
    })
}

fn extract_kimi_tool_call_details(json: &Value) -> Vec<String> {
    let role = json.get("role").and_then(|v| v.as_str());
    if role != Some("assistant") {
        return Vec::new();
    }
    let tool_calls = match json.get("tool_calls").and_then(|v| v.as_array()) {
        Some(tc) => tc,
        None => return Vec::new(),
    };
    tool_calls
        .iter()
        .filter_map(|tc| {
            let function = tc.get("function")?;
            let name = function.get("name")?.as_str()?;
            let input = function
                .get("arguments")
                .map(parse_kimi_tool_arguments)
                .unwrap_or_else(|| function.clone());
            Some(LIVE_SUMMARY_FORMATTER.tool_detail(name, &input))
        })
        .collect()
}

fn parse_kimi_tool_arguments(arguments: &Value) -> Value {
    match arguments {
        Value::String(text) => serde_json::from_str::<Value>(text).unwrap_or_else(|_| {
            serde_json::json!({
                "command": text
            })
        }),
        _ => arguments.clone(),
    }
}

/// Format a Kimi tool name into a human-readable label.
/// e.g. "ReadFile" -> "Read File", "StrReplaceFile" -> "Str Replace File"
fn format_kimi_tool_name(name: &str) -> String {
    // Convert CamelCase/snake_case to space-separated words
    let formatted = name
        .chars()
        .enumerate()
        .map(|(i, c)| {
            if c.is_uppercase() && i > 0 {
                format!(" {}", c)
            } else if c == '_' {
                " ".to_string()
            } else {
                c.to_string()
            }
        })
        .collect::<String>();
    formatted.trim().to_string()
}

fn generate_kimi_line_summary(json: &Value) -> String {
    let Some(role) = json.get("role").and_then(|v| v.as_str()) else {
        return String::new();
    };
    // Extract model from the JSON if present
    let model = json.get("model").and_then(|v| v.as_str());
    match role {
        "assistant" => {
            let tool_details = extract_kimi_tool_call_details(json);
            if tool_details.is_empty() {
                return String::new();
            }
            // Deduplicate repeated tool details when Kimi emits duplicate tool entries.
            use std::collections::HashSet;
            let mut seen = HashSet::new();
            let unique_details: Vec<String> = tool_details
                .iter()
                .filter_map(|detail| {
                    if seen.contains(detail) {
                        None
                    } else {
                        seen.insert(detail.clone());
                        Some(detail.clone())
                    }
                })
                .collect();
            if unique_details.is_empty() {
                return String::new();
            }
            let summary = if unique_details.len() == 1 {
                unique_details[0].clone()
            } else {
                unique_details.join(", ")
            };
            LIVE_SUMMARY_FORMATTER.detail_line(model, &summary)
        }
        "tool" => {
            // Tool result: extract the tool name and format nicely
            // Try to find the tool name - it might be in "name" field directly
            let tool_name = json
                .get("name")
                .and_then(|v| v.as_str())
                .map(format_kimi_tool_name)
                .unwrap_or_else(|| "tool result".to_string());
            let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.trim().is_empty() {
                return String::new();
            }
            let preview = if content.len() > 60 {
                format!("{}...", &content[..60])
            } else {
                content.to_string()
            };
            LIVE_SUMMARY_FORMATTER.detail_line(model, &format!("{} → {}", tool_name, preview))
        }
        _ => String::new(),
    }
}

fn extract_kimi_session_id_from_stderr(stderr: &str) -> Option<String> {
    for line in stderr.lines() {
        if let Some(rest) = line.strip_prefix("To resume this session: kimi -r ") {
            let sid = rest.trim();
            if !sid.is_empty() {
                return Some(sid.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{
        build_kimi_prompt, extract_kimi_session_id_from_stderr, extract_kimi_text_content,
        generate_kimi_line_summary,
    };
    use crate::session::ProviderSession;
    use serde_json::json;

    #[test]
    fn parses_session_id_from_stderr() {
        let stderr =
            "Some log\nTo resume this session: kimi -r 0cb1ccdd-9b6b-4b2e-a11f-de9f4ba4aaaa\n";
        assert_eq!(
            extract_kimi_session_id_from_stderr(stderr),
            Some("0cb1ccdd-9b6b-4b2e-a11f-de9f4ba4aaaa".to_string())
        );
    }

    #[test]
    fn returns_none_when_no_session_line() {
        assert_eq!(extract_kimi_session_id_from_stderr("just logs"), None);
    }

    #[test]
    fn suppresses_assistant_text_during_tool_turns() {
        let json = json!({
            "role": "assistant",
            "content": [
                {"type": "text", "text": "The background agents were lost. Let me explore directly."}
            ],
            "tool_calls": [
                {
                    "function": {
                        "name": "Shell",
                        "arguments": "{\"command\":\"pwd\"}"
                    }
                }
            ]
        });
        assert_eq!(extract_kimi_text_content(&json), None);
    }

    #[test]
    fn renders_tool_detail_with_command_preview() {
        let json = json!({
            "role": "assistant",
            "tool_calls": [
                {
                    "function": {
                        "name": "Shell",
                        "arguments": "{\"command\":\"pwd\"}"
                    }
                }
            ]
        });
        assert_eq!(generate_kimi_line_summary(&json), "Kimi | Bash `pwd`");
    }

    #[test]
    fn suppresses_empty_tool_result_lines() {
        let json = json!({
            "role": "tool",
            "name": "Shell",
            "content": ""
        });
        assert!(generate_kimi_line_summary(&json).is_empty());
    }

    #[test]
    fn resumed_prompts_still_include_system_instructions() {
        let session = ProviderSession {
            kimi_session_id: Some("resume-session".to_string()),
            ..Default::default()
        };

        let prompt = build_kimi_prompt(
            Some("Do not use background agents."),
            "Inspect the repo",
            Some(&session),
            true,
            None,
        );

        assert!(prompt.contains("System instructions:\nDo not use background agents."));
        assert!(prompt.contains("User: Inspect the repo"));
        assert!(!prompt.contains("Conversation so far:"));
    }
}
