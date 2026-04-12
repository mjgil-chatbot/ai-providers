//! Text-based tool call parser for OpenRouter models.
//!
//! Models like Qwen emit tool calls as JSON text in their response rather than
//! structured tool_use blocks. This module parses those calls so they can be
//! executed by the caller with appropriate permission enforcement.

use serde_json::Value;
/// A parsed tool call from the model's text response.
#[derive(Debug, Clone)]
pub struct TextToolCall {
    pub tool: String,
    pub arguments: Value,
}
/// Scan the response text for a JSON tool call of the form:
///   {"tool": "<name>", "arguments": {...}}
///
/// Returns the first tool call found and the text before it (the "preamble"
/// the model wrote before invoking the tool).
pub fn parse_tool_call(response: &str) -> Option<(TextToolCall, &str)> {
    // Look for lines that start with `{"tool":` (possibly with leading whitespace)
    for (byte_offset, line) in line_offsets(response) {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            continue;
        }
        if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
            if let Some(tool_name) = val.get("tool").and_then(|v| v.as_str()) {
                let arguments = val
                    .get("arguments")
                    .cloned()
                    .unwrap_or(Value::Object(Default::default()));
                let preamble = &response[..byte_offset];
                return Some((
                    TextToolCall {
                        tool: tool_name.to_string(),
                        arguments,
                    },
                    preamble,
                ));
            }
        }
    }
    None
}
/// Iterate over lines in `text`, yielding `(byte_offset, line_str)`.
fn line_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut offset = 0;
    text.lines().map(move |line| {
        let start = offset;
        // +1 for the newline character (or 0 if last line without newline)
        offset += line.len() + 1;
        (start, line)
    })
}
