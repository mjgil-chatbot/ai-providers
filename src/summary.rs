use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
const TOOL_INPUT_PREVIEW_MAX_CHARS: usize = 120;
static MCP_SERVER_COMMANDS: Mutex<Option<McpServerCommandCache>> = Mutex::new(None);
#[derive(Clone, Debug)]
struct McpServerCommandCache {
    path: PathBuf,
    commands: HashMap<String, String>,
}
pub fn format_tool_input_preview(name: &str, input: &Value) -> String {
    let normalized = resolve_tool_input_value(input).or_else(|| match input {
        Value::String(_) => Some(parse_embedded_json(input)),
        _ => None,
    });
    let mut preview = match normalized {
        Some(value) => format_preview_value(name, &value),
        None => format_preview_value(name, input),
    };
    if let Some(server_command) = mcp_server_command_for_tool(name) {
        preview = if preview.is_empty() {
            format!("server={server_command}")
        } else {
            format!("server={server_command} | {preview}")
        };
    }
    truncate_with_ellipsis(&preview, TOOL_INPUT_PREVIEW_MAX_CHARS)
}
fn mcp_server_command_for_tool(tool_name: &str) -> Option<String> {
    let server_name = tool_name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
        .map(|(server_name, _)| normalize_mcp_server_name(server_name))?;
    cached_mcp_server_commands().get(&server_name).cloned()
}
fn cached_mcp_server_commands() -> HashMap<String, String> {
    let path = mcp_json_path();
    let mut cache = MCP_SERVER_COMMANDS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.as_ref().is_none_or(|cache| cache.path != path) {
        *cache = Some(McpServerCommandCache {
            path: path.clone(),
            commands: load_mcp_server_commands(&path),
        });
    }
    cache
        .as_ref()
        .map(|cache| cache.commands.clone())
        .unwrap_or_default()
}
fn mcp_json_path() -> PathBuf {
    if let Some(path) =
        std::env::var_os("RUST_CHATBOT_MCP_JSON_PATH").filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }

    dirs::home_dir()
        .map(|home| home.join(".rust-chatbot").join(".mcp.json"))
        .unwrap_or_else(|| PathBuf::from(".mcp.json"))
}
fn load_mcp_server_commands(path: &std::path::Path) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return HashMap::new(),
    };
    let parsed = match serde_json::from_str::<Value>(&content) {
        Ok(parsed) => parsed,
        Err(_) => return HashMap::new(),
    };
    let mut commands = HashMap::new();

    if let Some(servers) = parsed.get("mcpServers").and_then(|value| value.as_object()) {
        extend_mcp_server_commands(&mut commands, servers);
    }
    if let Some(providers) = parsed.get("providers").and_then(|value| value.as_object()) {
        for provider in providers.values().filter_map(|value| value.as_object()) {
            if let Some(servers) = provider
                .get("mcpServers")
                .and_then(|value| value.as_object())
            {
                extend_mcp_server_commands(&mut commands, servers);
            }
        }
    }

    commands
}
fn extend_mcp_server_commands(
    commands: &mut HashMap<String, String>,
    servers: &serde_json::Map<String, Value>,
) {
    for (name, entry) in servers {
        if let Some(command) = format_server_command(entry) {
            commands.insert(normalize_mcp_server_name(name), command);
        }
    }
}
fn normalize_mcp_server_name(name: &str) -> String {
    name.replace('-', "_")
}
fn format_server_command(entry: &Value) -> Option<String> {
    let object = entry.as_object()?;
    let command = object.get("command").and_then(|value| value.as_str())?;
    let mut parts = vec![command.to_string()];
    if let Some(args) = object.get("args").and_then(|value| value.as_array()) {
        parts.extend(
            args.iter()
                .filter_map(|value| value.as_str())
                .map(ToString::to_string),
        );
    }
    Some(parts.join(" "))
}
fn resolve_tool_input_value(input: &Value) -> Option<Value> {
    for candidate in [
        input.get("input"),
        input.get("arguments"),
        input.get("args"),
        input.get("params"),
        input.get("call").and_then(|value| value.get("arguments")),
        input.get("call").and_then(|value| value.get("args")),
        input.get("call").and_then(|value| value.get("input")),
        input
            .get("function")
            .and_then(|value| value.get("arguments")),
        input
            .get("functionCall")
            .and_then(|value| value.get("args")),
    ] {
        if let Some(value) = candidate {
            return Some(parse_embedded_json(value));
        }
    }
    None
}
fn parse_embedded_json(value: &Value) -> Value {
    match value {
        Value::String(text) => {
            serde_json::from_str::<Value>(text).unwrap_or_else(|_| value.clone())
        }
        _ => value.clone(),
    }
}
fn format_preview_value(name: &str, input: &Value) -> String {
    if let Some(command) = command_like_preview(input) {
        return command;
    }

    if let Some(object) = input.as_object() {
        let mut parts = Vec::new();
        push_preview_part(&mut parts, "path", object.get("path"));
        push_preview_part(&mut parts, "file_path", object.get("file_path"));
        push_preview_part(&mut parts, "project_path", object.get("project_path"));
        push_preview_part(&mut parts, "workspace_path", object.get("workspace_path"));
        push_preview_part(&mut parts, "libraryId", object.get("libraryId"));
        push_preview_part(&mut parts, "url", object.get("url"));
        push_preview_part(&mut parts, "query", object.get("query"));
        push_preview_part(&mut parts, "q", object.get("q"));
        push_preview_part(&mut parts, "selector", object.get("selector"));
        push_preview_part(&mut parts, "location", object.get("location"));
        push_preview_part(&mut parts, "id", object.get("id"));
        push_preview_part(&mut parts, "value", object.get("value"));

        push_count_part(&mut parts, "paths", object.get("paths"));
        push_count_part(&mut parts, "elements", object.get("elements"));
        push_count_part(&mut parts, "edits", object.get("edits"));

        if let Some(override_value) = object.get("command_override") {
            let rendered = format_array_or_scalar(override_value);
            if !rendered.is_empty() {
                parts.push(format!("command={rendered}"));
            }
        }
        if let Some(max_results) = object.get("max_results").and_then(|value| value.as_u64()) {
            parts.push(format!("max_results={max_results}"));
        }
        if let Some(timeout) = object.get("timeout").and_then(|value| value.as_u64()) {
            parts.push(format!("timeout={timeout}"));
        }

        if !parts.is_empty() {
            return parts.join(" | ");
        }
    }

    match input {
        Value::String(text) => text.clone(),
        _ => {
            let rendered = input.to_string();
            if rendered.is_empty() {
                name.to_string()
            } else {
                rendered
            }
        }
    }
}
fn command_like_preview(input: &Value) -> Option<String> {
    let object = input.as_object()?;
    if let Some(command_line) = object.get("command_line").and_then(|value| value.as_str()) {
        return Some(command_line.to_string());
    }
    if let Some(command) = object.get("command").and_then(|value| value.as_str()) {
        let args = object
            .get("args")
            .map(format_array_or_scalar)
            .unwrap_or_default();
        return if args.is_empty() {
            Some(command.to_string())
        } else {
            Some(format!("{command} {args}"))
        };
    }
    None
}
fn push_preview_part(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        Value::Array(values) => format!("{} items", values.len()),
        _ => value.to_string(),
    };
    if !rendered.is_empty() {
        parts.push(format!("{label}={rendered}"));
    }
}
fn push_count_part(parts: &mut Vec<String>, label: &str, value: Option<&Value>) {
    if let Some(count) = value
        .and_then(|value| value.as_array())
        .map(|items| items.len())
    {
        parts.push(format!("{label}={count}"));
    }
}
fn format_array_or_scalar(value: &Value) -> String {
    match value {
        Value::Array(values) => values
            .iter()
            .filter_map(|value| match value {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                Value::Bool(boolean) => Some(boolean.to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(boolean) => boolean.to_string(),
        _ => value.to_string(),
    }
}
fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut end = 0;
    for (seen, (idx, ch)) in s.char_indices().enumerate() {
        if seen == keep {
            break;
        }
        end = idx + ch.len_utf8();
    }
    format!("{}...", &s[..end])
}
