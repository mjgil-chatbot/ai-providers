use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderMessage {
    pub content: String,
    pub is_user: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_content: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderSession {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glm_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimax_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qwen_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nemotron_session_id: Option<String>, // Nemotron Codex-compatible session ID
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omniforge_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kimi_session_id: Option<String>,
    #[serde(default)]
    pub messages: Vec<ProviderMessage>,
}
