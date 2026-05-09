use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;

use crate::config::OpenRouterConfig;
use crate::memory::{build_query_with_memory, MemoryContext};
use crate::session::ProviderSession;
use crate::stream::{self, StreamMessage};
use crate::summary::{format_tool_input_preview, JsonSummary, ToolCallRecord};

use super::default_executor::default_executor;
use super::tool_exec::parse_tool_call;
pub use super::tool_exec::TextToolCall;

/// Maximum number of agent loop iterations (tool call → result → next response).
const MAX_TOOL_ITERATIONS: usize = 25;

const STDERR_TAIL_MAX_LINES: usize = 120;

/// Result of a single streaming turn.
#[derive(Debug)]
pub struct TurnResult {
    /// Full response text from this turn
    pub response_content: String,
    /// Summary of token usage etc.
    pub summary: JsonSummary,
    /// Whether an error occurred
    pub error: bool,
    /// Detected tool call, if any (caller executes this)
    pub tool_call: Option<(TextToolCall, String)>,
}

fn build_openrouter_prompt(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    default_system_prompt: Option<&str>,
) -> Vec<Value> {
    let effective_query = build_query_with_memory(query, memory_context);
    let mut messages = Vec::new();

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
        messages.push(serde_json::json!({
            "role": "system",
            "content": prompt
        }));
    }

    if let Some(session) = session {
        let last_index = session.messages.len().saturating_sub(1);
        for (idx, msg) in session.messages.iter().enumerate() {
            if idx == last_index && msg.is_user && msg.content.trim() == query.trim() {
                continue;
            }
            let content = if msg.is_user {
                msg.history_content.as_deref().unwrap_or(&msg.content)
            } else {
                &msg.content
            };
            if content.trim().is_empty() {
                continue;
            }
            messages.push(serde_json::json!({
                "role": if msg.is_user { "user" } else { "assistant" },
                "content": content.trim()
            }));
        }
    }

    messages.push(serde_json::json!({
        "role": "user",
        "content": effective_query.trim()
    }));

    messages
}

pub fn build_openrouter_command_string(
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
    config: &OpenRouterConfig,
) -> String {
    let messages = build_openrouter_prompt(
        system_prompt,
        query,
        session,
        memory_context,
        config.default_system_prompt.as_deref(),
    );
    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": messages,
    });
    format!(
        "curl -sN {}/chat/completions -H 'Content-Type: application/json' \
         -H 'Authorization: Bearer {}' \
         -d '{}'",
        config.base_url, "***", body
    )
}

/// Outcome of a single streaming API call.
struct StreamOutcome {
    response_content: String,
    summary: JsonSummary,
    error: bool,
}

/// Make a single streaming API call with the given messages and collect the full
/// response. Intermediate chunks are sent through `tx` so the UI can update in
/// real time.
fn stream_one_turn(
    messages: &[Value],
    config: &OpenRouterConfig,
    tx: &mpsc::Sender<StreamMessage>,
    stop_signal: &Arc<AtomicBool>,
    command_str: &str,
    // Content already accumulated from previous turns (displayed before this
    // turn's content).
    prior_content: &str,
    prior_summary: &JsonSummary,
) -> Result<StreamOutcome> {
    let body = serde_json::json!({
        "model": config.model,
        "max_tokens": config.max_tokens,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": messages,
    });
    let body_str = serde_json::to_string(&body)?;
    let url = format!("{}/chat/completions", config.base_url);

    let mut cmd = Command::new("curl");
    cmd.args([
        "-sN",
        &url,
        "-H",
        "Content-Type: application/json",
        "-H",
        &format!("Authorization: Bearer {}", config.api_key),
        "-d",
        &body_str,
    ]);

    let (cli, reader) = match stream::CliProcess::spawn(
        &mut cmd,
        "openrouter",
        command_str,
        tx,
        stop_signal.clone(),
        STDERR_TAIL_MAX_LINES,
    ) {
        Some(pair) => pair,
        None => {
            return Ok(StreamOutcome {
                response_content: String::new(),
                summary: prior_summary.clone(),
                error: true,
            });
        }
    };

    let mut response_content = String::new();
    let mut aggregated_summary = prior_summary.clone();
    let mut stdout_tail: VecDeque<String> = VecDeque::with_capacity(STDERR_TAIL_MAX_LINES);
    let mut last_error_detail: Option<String> = None;

    for line in reader.lines() {
        if cli.is_stopped() {
            warn!("stop signal triggered during openrouter stream read");
            cli.kill();
            let _ = tx.send(StreamMessage {
                response: "Process stopped by user.".to_string(),
                command: command_str.to_string(),
                session_id: None,
                summary: None,
                is_final: true,
            });
            return Ok(StreamOutcome {
                response_content,
                summary: aggregated_summary,
                error: true,
            });
        }

        let line = match line {
            Ok(line) => line,
            Err(err) => {
                error!(error=%err, "error reading openrouter stdout");
                cli.kill();
                let _ = tx.send(StreamMessage {
                    response: format!("Error reading stream: {}", err),
                    command: command_str.to_string(),
                    session_id: None,
                    summary: None,
                    is_final: true,
                });
                return Ok(StreamOutcome {
                    response_content,
                    summary: aggregated_summary,
                    error: true,
                });
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("event:") || trimmed.starts_with(':') {
            continue;
        }

        let json_str = sse_payload(trimmed);

        if json_str == "[DONE]" {
            continue;
        }

        match serde_json::from_str::<Value>(json_str) {
            Ok(json) => {
                if let Some(detail) = stream::extract_error_detail(&json) {
                    last_error_detail = Some(detail);
                } else if let Some(err) = json.get("error") {
                    // Fallback: bare error.message when extract_error_detail found nothing
                    if let Some(msg) = err.get("message").and_then(|v| v.as_str()) {
                        last_error_detail = Some(msg.to_string());
                    }
                }

                // Extract streamed content from choices[0].delta.content
                if let Some(choices) = json.get("choices").and_then(|v| v.as_array()) {
                    for choice in choices {
                        if let Some(text) = choice
                            .get("delta")
                            .and_then(|d| d.get("content"))
                            .and_then(|v| v.as_str())
                        {
                            response_content.push_str(text);
                        }
                        if let Some(tool_calls) = choice
                            .get("delta")
                            .and_then(|d| d.get("tool_calls"))
                            .and_then(|v| v.as_array())
                        {
                            for tc in tool_calls {
                                let name = tc
                                    .get("function")
                                    .and_then(|f| f.get("name"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let args = tc
                                    .get("function")
                                    .and_then(|f| f.get("arguments"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if !name.is_empty() {
                                    if !aggregated_summary.tools_used.contains(&name) {
                                        aggregated_summary.tools_used.push(name.clone());
                                    }
                                    aggregated_summary.pending_tool_calls.push(ToolCallRecord {
                                        input_preview: format_tool_input_preview(
                                            &name,
                                            &Value::String(args),
                                        ),
                                        name,
                                        ..Default::default()
                                    });
                                }
                            }
                        }
                        if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
                            if reason == "stop" || reason == "end_turn" {
                                aggregated_summary.response_count += 1;
                            }
                        }
                    }
                }

                // Extract usage from final chunk (stream_options.include_usage)
                if let Some(usage) = json.get("usage") {
                    if let Some(input) = usage.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        aggregated_summary.total_input_tokens += input;
                    }
                    if let Some(output) = usage.get("completion_tokens").and_then(|v| v.as_u64()) {
                        aggregated_summary.total_output_tokens += output;
                    }
                    if let Some(cost) = usage.get("cost").and_then(|v| v.as_f64()) {
                        aggregated_summary.total_cost_usd += cost;
                    }
                }

                // Send combined content (prior turns + current turn) to UI
                let _ = tx.send(StreamMessage {
                    response: format!("{}{}", prior_content, response_content),
                    command: command_str.to_string(),
                    session_id: None,
                    summary: Some(aggregated_summary.clone()),
                    is_final: false,
                });
            }
            Err(_) => {
                // Non-JSON line (e.g. HTML error page or raw text)
                if stdout_tail.len() == STDERR_TAIL_MAX_LINES {
                    stdout_tail.pop_front();
                }
                stdout_tail.push_back(line.clone());
            }
        }
    }

    let finish = cli.finish("openrouter");
    let process_ok = finish.wait_error.is_none()
        && finish
            .exit_status
            .as_ref()
            .map(|status| status.success())
            .unwrap_or(false);

    if !process_ok
        || (response_content.is_empty() && last_error_detail.is_some())
        || (response_content.is_empty() && !stdout_tail.is_empty())
    {
        let contexts = stream::ErrorContexts::build(
            &finish.stderr_tail,
            &stdout_tail,
            &response_content,
            &last_error_detail,
            STDERR_TAIL_MAX_LINES,
        );
        let detail =
            contexts.best_failure_detail(finish.exit_status.as_ref(), finish.wait_error.as_deref());
        let _ = tx.send(StreamMessage {
            response: format!("OpenRouter request failed.{}", detail),
            command: command_str.to_string(),
            session_id: None,
            summary: Some(aggregated_summary.clone()),
            is_final: true,
        });
        return Ok(StreamOutcome {
            response_content,
            summary: aggregated_summary,
            error: true,
        });
    }

    Ok(StreamOutcome {
        response_content,
        summary: aggregated_summary,
        error: false,
    })
}

/// Execute a single streaming turn and return.
///
/// This is a single-turn function: it makes one API call and returns the result.
/// If a tool call is detected in the response, it is returned to the caller
/// for execution (permission enforcement happens in the caller).
/// The caller is responsible for looping if needed.
pub fn stream_openrouter_turn(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &OpenRouterConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<TurnResult> {
    stream_openrouter_turn_impl(
        query,
        system_prompt,
        memory_context,
        session.as_ref(),
        config,
        tx,
        &stop_signal,
        None,
    )
}

/// Result of tool execution (returned to the model for next turn).
#[derive(Debug)]
pub struct ToolExecResult {
    pub output: String,
    pub success: bool,
}

/// Callback for tool execution. The caller provides this to handle tool calls
/// with their own permission enforcement. Returns the tool result to feed back
/// to the model.
pub type ToolExecutor = Box<dyn Fn(TextToolCall) -> ToolExecResult + Send + Sync>;

fn sse_payload(line: &str) -> &str {
    let trimmed = line.trim();
    trimmed
        .strip_prefix("data:")
        .map(str::trim_start)
        .unwrap_or(trimmed)
}

fn stream_openrouter_turn_impl(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<&ProviderSession>,
    config: &OpenRouterConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: &Arc<AtomicBool>,
    _executor: Option<&ToolExecutor>,
) -> Result<TurnResult> {
    info!(
        model = %config.model,
        query_len = query.len(),
        "starting openrouter turn"
    );

    let messages = build_openrouter_prompt(
        system_prompt,
        query,
        session,
        memory_context,
        config.default_system_prompt.as_deref(),
    );

    let command_str =
        build_openrouter_command_string(system_prompt, query, session, memory_context, config);

    let mut summary = JsonSummary::new();
    summary.model = Some(config.model.clone());
    summary.context_window_size = config.context_window_size;

    let outcome = stream_one_turn(
        &messages,
        config,
        &tx,
        &stop_signal,
        &command_str,
        "",
        &summary,
    )?;

    summary = outcome.summary;

    if outcome.error {
        return Ok(TurnResult {
            response_content: outcome.response_content,
            summary,
            error: true,
            tool_call: None,
        });
    }

    // Check if the response contains a tool call - return it to caller for execution
    // Clone response_content since parse_tool_call borrows it
    if let Some((tool_call, preamble)) = parse_tool_call(&outcome.response_content.clone()) {
        info!(
            tool = %tool_call.tool,
            "detected tool call in openrouter response, returning to caller"
        );

        Ok(TurnResult {
            response_content: outcome.response_content,
            summary,
            error: false,
            tool_call: Some((tool_call, preamble.trim_end().to_string())),
        })
    } else {
        // No tool call - this is the final response
        Ok(TurnResult {
            response_content: outcome.response_content,
            summary,
            error: false,
            tool_call: None,
        })
    }
}

/// Stream OpenRouter with tool execution using the provided executor.
/// The executor callback handles tool execution - callers should provide their own
/// for permission enforcement. For backward compatibility, use this with the
/// default_tool_executor.
pub fn call_openrouter_stream_with_executor(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &OpenRouterConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
    executor: ToolExecutor,
) -> Result<()> {
    info!(
        model = %config.model,
        query_len = query.len(),
        "starting openrouter stream with custom executor"
    );

    let mut messages = build_openrouter_prompt(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config.default_system_prompt.as_deref(),
    );

    let command_str = build_openrouter_command_string(
        system_prompt,
        query,
        session.as_ref(),
        memory_context,
        config,
    );

    let mut full_response = String::new();
    let mut summary = JsonSummary::new();
    summary.model = Some(config.model.clone());
    summary.context_window_size = config.context_window_size;

    let session_ref = session.as_ref();

    for iteration in 0..MAX_TOOL_ITERATIONS {
        if stop_signal.load(Ordering::Acquire) {
            break;
        }

        let outcome = stream_openrouter_turn_impl(
            query,
            system_prompt,
            memory_context,
            session_ref,
            config,
            tx.clone(),
            &stop_signal,
            None,
        )?;

        summary = outcome.summary;

        if outcome.error {
            return Ok(());
        }

        // Check if the response contains a tool call
        if let Some((tool_call, preamble)) = parse_tool_call(&outcome.response_content) {
            info!(
                tool = %tool_call.tool,
                iteration = iteration,
                "detected tool call in openrouter response, executing via callback"
            );

            // Build up the full response shown to the user
            let preamble = preamble.trim_end();
            if !preamble.is_empty() {
                if !full_response.is_empty() {
                    full_response.push('\n');
                }
                full_response.push_str(preamble);
            }

            // Execute the tool via the provided executor
            let tool_name = tool_call.tool.clone();
            let exec_result = executor(tool_call);

            // Show tool execution in the streamed response
            let tool_display = format!(
                "\n\n**[Tool: {}]** {}\n\n<details>\n<summary>Tool output</summary>\n\n```\n{}\n```\n</details>\n\n",
                tool_name,
                if exec_result.success { "✓" } else { "✗" },
                exec_result.output.trim()
            );
            full_response.push_str(&tool_display);

            // Send intermediate update showing tool execution
            let _ = tx.send(StreamMessage {
                response: full_response.clone(),
                command: command_str.clone(),
                session_id: None,
                summary: Some(summary.clone()),
                is_final: false,
            });

            // Append the assistant message and tool result to conversation for
            // the next API call.
            messages.push(serde_json::json!({
                "role": "assistant",
                "content": outcome.response_content.trim()
            }));
            messages.push(serde_json::json!({
                "role": "user",
                "content": format!(
                    "Tool result for {}:\n```\n{}\n```\nContinue with the task. If you need to use another tool, output the JSON tool call. If you are done, provide your final answer.",
                    tool_name,
                    exec_result.output.trim()
                )
            }));
        } else {
            // No tool call — this is the final response
            if !full_response.is_empty() {
                full_response.push_str(&outcome.response_content);
            } else {
                full_response = outcome.response_content;
            }
            break;
        }
    }

    let _ = tx.send(StreamMessage {
        response: full_response,
        command: command_str,
        session_id: None,
        summary: Some(summary),
        is_final: true,
    });
    Ok(())
}

/// Stream OpenRouter with the default tool executor (no permission enforcement).
/// For permission enforcement, use stream_openrouter_turn with a custom executor
/// in the calling code.
pub fn call_openrouter_stream(
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    config: &OpenRouterConfig,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    call_openrouter_stream_with_executor(
        query,
        system_prompt,
        memory_context,
        session,
        config,
        tx,
        stop_signal,
        default_executor(),
    )
}

#[cfg(test)]
mod tests {
    use super::{build_openrouter_prompt, sse_payload};
    use crate::session::{ProviderMessage, ProviderSession};

    #[test]
    fn prompt_includes_system_and_history() {
        let session = ProviderSession {
            messages: vec![
                ProviderMessage {
                    content: "First question".to_string(),
                    is_user: true,
                    history_content: None,
                },
                ProviderMessage {
                    content: "First answer.".to_string(),
                    is_user: false,
                    history_content: None,
                },
            ],
            ..Default::default()
        };

        let messages =
            build_openrouter_prompt(Some("Be helpful"), "Follow-up", Some(&session), None, None);

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be helpful");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "First question");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[2]["content"], "First answer.");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"], "Follow-up");
    }

    #[test]
    fn prompt_without_system_or_history() {
        let messages = build_openrouter_prompt(None, "Hello", None, None, None);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Hello");
    }

    #[test]
    fn sse_payload_accepts_data_prefix_with_or_without_space() {
        assert_eq!(sse_payload("data: {\"x\":1}"), "{\"x\":1}");
        assert_eq!(sse_payload("data:{\"x\":1}"), "{\"x\":1}");
        assert_eq!(sse_payload("{\"x\":1}"), "{\"x\":1}");
    }
}
