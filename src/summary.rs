use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub input_preview: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnUsage {
    pub timestamp: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub duration_ms: u64,
    pub cost_usd: f64,
    pub num_turns: u32,
    #[serde(default)]
    pub context_window_tokens: u64,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallRecord>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct JsonSummary {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub total_duration_ms: u64,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    #[serde(default)]
    pub cache_creation_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    pub tools_used: Vec<String>,
    pub response_count: u32,
    #[serde(default)]
    pub context_window_tokens: u64,
    #[serde(default)]
    pub context_window_size: u64,
    #[serde(default)]
    pub num_compactions: u32,
    #[serde(default)]
    pub turn_history: Vec<TurnUsage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_tool_calls: Vec<ToolCallRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JsonSummaryPreview {
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub total_duration_ms: u64,
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub tools_used: Vec<String>,
    pub response_count: u32,
    pub context_window_tokens: u64,
    pub context_window_size: u64,
    pub num_compactions: u32,
    pub turn_count: usize,
    pub recent_turns: Vec<TurnUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}
const PREVIEW_RECENT_TURNS: usize = 5;
const TOOL_INPUT_PREVIEW_MAX_CHARS: usize = 120;
static MCP_SERVER_COMMANDS: Mutex<Option<McpServerCommandCache>> = Mutex::new(None);
#[derive(Clone, Debug)]
struct McpServerCommandCache {
    path: PathBuf,
    commands: HashMap<String, String>,
}
impl JsonSummary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn to_preview(&self) -> JsonSummaryPreview {
        let recent_start = self.turn_history.len().saturating_sub(PREVIEW_RECENT_TURNS);
        JsonSummaryPreview {
            session_id: self.session_id.clone(),
            model: self.model.clone(),
            total_duration_ms: self.total_duration_ms,
            total_cost_usd: self.total_cost_usd,
            total_input_tokens: self.total_input_tokens,
            total_output_tokens: self.total_output_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cache_read_tokens: self.cache_read_tokens,
            tools_used: self.tools_used.clone(),
            response_count: self.response_count,
            context_window_tokens: self.context_window_tokens,
            context_window_size: self.context_window_size,
            num_compactions: self.num_compactions,
            turn_count: self.turn_history.len(),
            recent_turns: self.turn_history[recent_start..].to_vec(),
            reasoning_effort: self.reasoning_effort.clone(),
        }
    }

    pub fn merge(&mut self, other: &JsonSummary) {
        self.response_count += other.response_count;
        self.total_duration_ms += other.total_duration_ms;
        self.total_input_tokens += other.total_input_tokens;
        self.total_output_tokens += other.total_output_tokens;
        self.cache_creation_tokens += other.cache_creation_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.total_cost_usd += other.total_cost_usd;

        if other.context_window_tokens > 0 {
            self.context_window_tokens = other.context_window_tokens;
        }
        if other.context_window_size > 0 {
            self.context_window_size = other.context_window_size;
        }
        if self.session_id.is_none() {
            self.session_id = other.session_id.clone();
        }
        if self.model.is_none() {
            self.model = other.model.clone();
        }
        if self.reasoning_effort.is_none() {
            self.reasoning_effort = other.reasoning_effort.clone();
        }

        for tool in &other.tools_used {
            if !self.tools_used.contains(tool) {
                self.tools_used.push(tool.clone());
            }
        }

        self.num_compactions += other.num_compactions;
        self.turn_history.extend(other.turn_history.iter().cloned());
        self.pending_tool_calls
            .extend(other.pending_tool_calls.iter().cloned());
    }

    pub fn record_turn_usage(&mut self, turn: &JsonSummary) {
        self.turn_history.push(TurnUsage {
            timestamp: Utc::now().to_rfc3339(),
            input_tokens: turn.total_input_tokens,
            output_tokens: turn.total_output_tokens,
            cache_creation_tokens: turn.cache_creation_tokens,
            cache_read_tokens: turn.cache_read_tokens,
            duration_ms: turn.total_duration_ms,
            cost_usd: turn.total_cost_usd,
            num_turns: turn.response_count,
            context_window_tokens: turn.context_window_tokens,
            tool_calls: turn.pending_tool_calls.clone(),
        });
    }
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
