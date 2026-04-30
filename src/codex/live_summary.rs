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

#[cfg(test)]
mod tests {
    use super::{compose_live_response, generate_codex_line_summary};
    use serde_json::json;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn compose_live_response_places_tool_output_above_text() {
        let rendered = compose_live_response("GPT-5.4 | Grep `rg foo src`", "final answer");
        assert_eq!(rendered, "GPT-5.4 | Grep `rg foo src`\n\nfinal answer");
    }

    #[test]
    fn command_execution_summary_uses_specific_model_label() {
        let summary = generate_codex_line_summary(
            &json!({
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "command": "/bin/bash -lc 'git status --short --branch'"
                }
            }),
            Some("gpt-5.4"),
        )
        .expect("command summary");

        assert_eq!(summary, "GPT-5.4 | Bash `git status --short --branch`");
    }

    #[test]
    fn grep_command_execution_summary_gets_grep_label() {
        let summary = generate_codex_line_summary(
            &json!({
                "type": "item.completed",
                "item": {
                    "type": "command_execution",
                    "command": "/bin/bash -lc 'rg -n \"TODO|FIXME\" crates/src'"
                }
            }),
            Some("gpt-5.4"),
        )
        .expect("grep summary");

        assert_eq!(summary, "GPT-5.4 | Grep `rg -n \"TODO|FIXME\" crates/src`");
    }

    #[test]
    fn item_summary_formats_mcp_server_and_command_preview() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp = tempfile::tempdir().unwrap();
        let mcp_json = temp.path().join(".mcp.json");
        std::fs::write(
            &mcp_json,
            r#"{"mcpServers":{"lightweight_builds":{"command":"cargo","args":["run","--quiet","--manifest-path","/tmp/l/C.toml"]}}}"#,
        )
        .unwrap();
        let previous = std::env::var_os("RUST_CHATBOT_MCP_JSON_PATH");
        std::env::set_var("RUST_CHATBOT_MCP_JSON_PATH", &mcp_json);

        let summary = generate_codex_line_summary(
            &json!({
                "type": "item.completed",
                "item": {
                    "type": "mcp_tool_call",
                    "name": "mcp__lightweight_builds__run_build",
                    "arguments": {
                        "project_path": "/tmp/project",
                        "command_override": ["cargo", "test", "--lib"]
                    }
                }
            }),
            Some("gpt-5.4-high"),
        )
        .expect("tool summary");

        assert!(summary.contains("GPT-5.4-High | MCP lightweight_builds.run_build"));
        assert!(summary.contains("server=`cargo run --quiet --manifest-path /tmp/l/C.toml`"));
        assert!(summary.contains("project_path=`/tmp/project`"));
        assert!(summary.contains("command=`cargo test --lib`"));

        match previous {
            Some(path) => std::env::set_var("RUST_CHATBOT_MCP_JSON_PATH", path),
            None => std::env::remove_var("RUST_CHATBOT_MCP_JSON_PATH"),
        }
    }

    #[test]
    fn turn_summary_formats_cost_and_duration() {
        let summary = generate_codex_line_summary(
            &json!({
                "type": "turn.completed",
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 200,
                    "cache_creation_input_tokens": 50,
                    "cache_read_input_tokens": 25
                },
                "duration_ms": 1500,
                "num_turns": 2
            }),
            Some("gpt-5.4-high"),
        )
        .expect("turn summary");

        assert!(summary.starts_with("GPT-5.4-High complete | 1500ms | $"));
        assert!(summary.ends_with("| 2 turns"));
    }
}
