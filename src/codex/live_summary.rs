use crate::claude::compute_cost;
use crate::live_summary_formatter::LiveSummaryFormatter;
use serde_json::Value;
const LIVE_SUMMARY_FORMATTER: LiveSummaryFormatter = LiveSummaryFormatter::new("Codex");
pub(super) fn compose_live_response(streamed_summary: &str, actual_text_content: &str) -> String {
    LIVE_SUMMARY_FORMATTER.compose_response(streamed_summary, actual_text_content)
}
pub(super) fn generate_codex_line_summary(
    json: &Value,
    current_model: Option<&str>,
) -> Option<String> {
    match json.get("type").and_then(|value| value.as_str()) {
        Some("thread.started") => Some(LIVE_SUMMARY_FORMATTER.started_line(current_model)),
        Some("item.completed") => json
            .get("item")
            .and_then(|item| generate_codex_item_summary(item, current_model)),
        Some("turn.completed") => Some(generate_codex_turn_summary(json, current_model)),
        _ => None,
    }
}
fn generate_codex_item_summary(item: &Value, current_model: Option<&str>) -> Option<String> {
    let item_type = item.get("type").and_then(|value| value.as_str())?;
    match item_type {
        "command_execution" => codex_command_execution_command(item)
            .map(|command| LIVE_SUMMARY_FORMATTER.command_execution_line(current_model, &command)),
        "mcp_tool_call" => {
            let name = codex_tool_name(item);
            Some(LIVE_SUMMARY_FORMATTER.mcp_tool_line(current_model, &name, item))
        }
        "context_compaction" => {
            Some(LIVE_SUMMARY_FORMATTER.detail_line(current_model, "context compaction"))
        }
        _ => None,
    }
}
fn generate_codex_turn_summary(json: &Value, current_model: Option<&str>) -> String {
    let duration_ms = json
        .get("duration_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let usage = json.get("usage");
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
    let cost = compute_cost(
        current_model,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
    );
    let turns = json
        .get("response_count")
        .or_else(|| json.get("num_turns"))
        .and_then(|value| value.as_u64())
        .unwrap_or(1);

    LIVE_SUMMARY_FORMATTER.completion_line(current_model, duration_ms, cost, turns)
}
fn codex_tool_name(item: &Value) -> String {
    item.get("call")
        .and_then(|value| value.get("name"))
        .or_else(|| item.get("name"))
        .and_then(|value| value.as_str())
        .unwrap_or("tool")
        .to_string()
}
fn codex_command_execution_command(item: &Value) -> Option<String> {
    if let Some(command) = item.get("command").and_then(|value| value.as_str()) {
        return Some(command.to_string());
    }
    let arguments = codex_item_arguments(item)?;
    if let Some(command_line) = arguments
        .get("command_line")
        .and_then(|value| value.as_str())
    {
        return Some(command_line.to_string());
    }
    if let Some(command) = arguments.get("command").and_then(|value| value.as_str()) {
        let args = arguments
            .get("args")
            .and_then(|value| value.as_array())
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| value.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        return if args.is_empty() {
            Some(command.to_string())
        } else {
            Some(format!("{command} {args}"))
        };
    }
    None
}
fn codex_item_arguments(item: &Value) -> Option<Value> {
    for candidate in [
        item.get("input"),
        item.get("arguments"),
        item.get("args"),
        item.get("params"),
        item.get("call").and_then(|value| value.get("arguments")),
        item.get("call").and_then(|value| value.get("args")),
        item.get("call").and_then(|value| value.get("input")),
        item.get("function")
            .and_then(|value| value.get("arguments")),
        item.get("functionCall").and_then(|value| value.get("args")),
    ] {
        if let Some(value) = candidate {
            return Some(parse_codex_embedded_json(value));
        }
    }
    None
}
fn parse_codex_embedded_json(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| value.clone())
        }
        _ => value.clone(),
    }
}
