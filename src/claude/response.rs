use crate::live_summary_formatter::LiveSummaryFormatter;
use crate::summary::{format_tool_input_preview, JsonSummary, ToolCallRecord};
use rust_debug::tracing::info;
use serde_json::Value;
const LIVE_SUMMARY_FORMATTER: LiveSummaryFormatter = LiveSummaryFormatter::new("Agent");
pub fn compute_cost(
    model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    let normalized = model.unwrap_or_default().to_ascii_lowercase();
    let (base_input_rate, output_rate, cache_read_rate, cache_write_rate) =
        if normalized.contains("minimax-m2.7-highspeed") {
            (0.6, 2.4, 0.06, 0.375)
        } else if normalized.contains("minimax-m2.5-highspeed")
            || normalized.contains("minimax-m2.1-highspeed")
        {
            (0.6, 2.4, 0.03, 0.375)
        } else if normalized.contains("minimax-m2.7") {
            (0.3, 1.2, 0.06, 0.375)
        } else if normalized.contains("minimax-m2.5")
            || normalized.contains("minimax-m2.1")
            || normalized.contains("minimax-m2")
        {
            (0.3, 1.2, 0.03, 0.375)
        } else {
            let (base_input_rate, output_rate) = if normalized.contains("opus") {
                (15.0, 75.0)
            } else if normalized.contains("haiku") {
                (0.80, 4.0)
            } else {
                (3.0, 15.0)
            };
            (
                base_input_rate,
                output_rate,
                base_input_rate * 0.10,
                base_input_rate * 1.25,
            )
        };

    let million = 1_000_000.0;
    (input_tokens as f64 / million) * base_input_rate
        + (cache_creation_tokens as f64 / million) * cache_write_rate
        + (cache_read_tokens as f64 / million) * cache_read_rate
        + (output_tokens as f64 / million) * output_rate
}
fn merge_tools(tools_used: &mut Vec<String>, json: &Value) {
    let Some(content) = json
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
    else {
        return;
    };

    for item in content {
        let Some(tool_name) = item
            .get("name")
            .and_then(|name| name.as_str())
            .filter(|_| item.get("type").and_then(|value| value.as_str()) == Some("tool_use"))
        else {
            continue;
        };

        if !tools_used.iter().any(|tool| tool == tool_name) {
            tools_used.push(tool_name.to_string());
        }
    }
}
fn tool_input_preview(name: &str, input: &Value) -> String {
    let _ = name;
    format_tool_input_preview(name, input)
}
fn extract_tool_calls(tool_calls: &mut Vec<ToolCallRecord>, json: &Value) {
    let Some(message) = json.get("message") else {
        return;
    };
    let usage = message.get("usage");
    let input_tokens = usage
        .and_then(|value| value.get("input_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|value| value.get("output_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let cache_creation_tokens = usage
        .and_then(|value| value.get("cache_creation_input_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let cache_read_tokens = usage
        .and_then(|value| value.get("cache_read_input_tokens"))
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    let Some(content) = message.get("content").and_then(|value| value.as_array()) else {
        return;
    };

    let tool_use_count = content
        .iter()
        .filter(|item| item.get("type").and_then(|value| value.as_str()) == Some("tool_use"))
        .count();
    for item in content {
        if item.get("type").and_then(|value| value.as_str()) != Some("tool_use") {
            continue;
        }
        let name = item
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("?")
            .to_string();
        let input_preview = item
            .get("input")
            .map(|value| tool_input_preview(&name, value))
            .unwrap_or_default();
        // Usage is reported at the assistant-message level. If one message contains multiple
        // tool calls, attributing the full usage block to each tool would overcount.
        let (input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens) =
            if tool_use_count == 1 {
                (
                    input_tokens,
                    output_tokens,
                    cache_creation_tokens,
                    cache_read_tokens,
                )
            } else {
                (0, 0, 0, 0)
            };
        tool_calls.push(ToolCallRecord {
            name,
            input_preview,
            input_tokens,
            output_tokens,
            cache_creation_tokens,
            cache_read_tokens,
        });
    }
}
pub fn generate_json_line_summary(json_line: &str, current_model: Option<&str>) -> String {
    if let Ok(json) = serde_json::from_str::<Value>(json_line) {
        if let Some(msg_type) = json.get("type").and_then(|value| value.as_str()) {
            match msg_type {
                "system" => {
                    let model_raw = json
                        .get("model")
                        .and_then(|value| value.as_str())
                        .or(current_model);
                    LIVE_SUMMARY_FORMATTER.started_line(model_raw)
                }
                "assistant" => {
                    let model_raw = json
                        .get("message")
                        .and_then(|message| message.get("model"))
                        .and_then(|value| value.as_str())
                        .or(current_model);
                    let tokens = json
                        .get("message")
                        .and_then(|value| value.get("usage"))
                        .and_then(|value| value.get("output_tokens"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let tool_summary = summarize_tool_calls(&json);
                    if let Some(tools) = tool_summary {
                        LIVE_SUMMARY_FORMATTER.detail_line(model_raw, &tools)
                    } else {
                        LIVE_SUMMARY_FORMATTER.detail_line(model_raw, &format!("{tokens} tokens"))
                    }
                }
                "result" => {
                    let duration = json
                        .get("duration_ms")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let turns = json
                        .get("num_turns")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let model = json
                        .get("model")
                        .and_then(|value| value.as_str())
                        .or(current_model);
                    let usage = json.get("usage");
                    let input_tokens = usage
                        .and_then(|value| value.get("input_tokens"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let output_tokens = usage
                        .and_then(|value| value.get("output_tokens"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let cache_creation = usage
                        .and_then(|value| value.get("cache_creation_input_tokens"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let cache_read = usage
                        .and_then(|value| value.get("cache_read_input_tokens"))
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0);
                    let cost = compute_cost(
                        model,
                        input_tokens,
                        output_tokens,
                        cache_creation,
                        cache_read,
                    );
                    info!(cost, turns, "result cost");
                    LIVE_SUMMARY_FORMATTER.completion_line(model, duration, cost, turns)
                }
                "user" => LIVE_SUMMARY_FORMATTER.user_message_line(),
                other => LIVE_SUMMARY_FORMATTER.bullet_line(other),
            }
        } else {
            LIVE_SUMMARY_FORMATTER.bullet_line("JSON response received")
        }
    } else {
        LIVE_SUMMARY_FORMATTER.bullet_line("Non-JSON response")
    }
}
fn summarize_tool_calls(json: &Value) -> Option<String> {
    let content = json.get("message")?.get("content")?.as_array()?;

    let calls: Vec<String> = content
        .iter()
        .filter(|item| item.get("type").and_then(|v| v.as_str()) == Some("tool_use"))
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?;
            Some(
                LIVE_SUMMARY_FORMATTER.tool_detail(name, item.get("input").unwrap_or(&Value::Null)),
            )
        })
        .collect();

    if calls.is_empty() {
        None
    } else {
        Some(calls.join(", "))
    }
}
pub fn update_summary_from_event(summary: &mut JsonSummary, json: &Value) {
    if summary.session_id.is_none() {
        summary.session_id = json
            .get("session_id")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string());
    }

    let Some(msg_type) = json.get("type").and_then(|value| value.as_str()) else {
        return;
    };
    match msg_type {
        "system" => {
            if summary.model.is_none() {
                summary.model = json
                    .get("model")
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
            }
        }
        "assistant" => {
            if summary.model.is_none() {
                summary.model = json
                    .get("message")
                    .and_then(|message| message.get("model"))
                    .and_then(|value| value.as_str())
                    .map(|value| value.to_string());
            }
            merge_tools(&mut summary.tools_used, json);
            extract_tool_calls(&mut summary.pending_tool_calls, json);
        }
        "result" => {
            summary.total_duration_ms = json
                .get("duration_ms")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            summary.response_count = json
                .get("num_turns")
                .and_then(|value| value.as_u64())
                .unwrap_or(0) as u32;

            if let Some(usage) = json.get("usage") {
                summary.total_input_tokens = usage
                    .get("input_tokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                summary.total_output_tokens = usage
                    .get("output_tokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                summary.cache_creation_tokens = usage
                    .get("cache_creation_input_tokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                summary.cache_read_tokens = usage
                    .get("cache_read_input_tokens")
                    .and_then(|value| value.as_u64())
                    .unwrap_or(0);
                summary.context_window_tokens = summary.total_input_tokens
                    + summary.cache_creation_tokens
                    + summary.cache_read_tokens;
            }

            summary.total_cost_usd = compute_cost(
                summary.model.as_deref(),
                summary.total_input_tokens,
                summary.total_output_tokens,
                summary.cache_creation_tokens,
                summary.cache_read_tokens,
            );

            if let Some(model_usage) = json.get("modelUsage").and_then(|value| value.as_object()) {
                if let Some(model_info) = model_usage.values().next() {
                    if let Some(size) = model_info
                        .get("contextWindow")
                        .and_then(|value| value.as_u64())
                    {
                        summary.context_window_size = size;
                    }
                }
            }
        }
        _ => {}
    }
}
