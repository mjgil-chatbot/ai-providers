use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryContext {
    pub workspace_id: String,
    pub query: String,
    pub hits: Vec<MemoryHit>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryHit {
    pub doc_key: String,
    pub chat_id: String,
    pub role: String,
    pub session_name: String,
    pub timestamp: DateTime<Utc>,
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
}

pub fn build_query_with_memory(query: &str, memory_context: Option<&MemoryContext>) -> String {
    let Some(memory_context) = memory_context.filter(|ctx| !ctx.hits.is_empty()) else {
        return query.to_string();
    };

    let mut out = String::from(
        "Use the retrieved chat memory only when it is relevant and do not treat it as authoritative if it conflicts with the current workspace state.\n\nRetrieved memory:\n",
    );

    for hit in &memory_context.hits {
        let session_name = sanitize_memory_text(&hit.session_name);
        let role = sanitize_memory_text(&hit.role);
        let timestamp = hit.timestamp.to_rfc3339();
        let snippet = sanitize_memory_text(&hit.snippet);
        if snippet.is_empty() {
            continue;
        }
        out.push_str("- [session=");
        out.push_str(if session_name.is_empty() {
            "unknown"
        } else {
            session_name.as_str()
        });
        out.push_str(" role=");
        out.push_str(if role.is_empty() {
            "unknown"
        } else {
            role.as_str()
        });
        out.push_str(" time=");
        out.push_str(&timestamp);
        out.push_str("] ");
        out.push_str(&snippet);
        out.push('\n');
    }

    out.push_str("\nUser request:\n");
    out.push_str(query.trim());
    out
}

fn sanitize_memory_text(input: &str) -> String {
    input
        .replace('\r', " ")
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}
