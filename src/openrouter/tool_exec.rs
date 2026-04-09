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
