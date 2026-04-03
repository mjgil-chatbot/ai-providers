use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};
use anyhow::Result;
use crate::config::{
    ClaudeCliConfig, CodexCliConfig, GeminiCliConfig, KimiCliConfig, OpenRouterCliConfig,
    OpenRouterConfig, QwenCodeCliConfig,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Claude,
    Glm,
    MiniMax,
    Codex,
    Nemotron,
    Gemini,
    Qwen,
    OpenRouter,
    Kimi,
}
#[derive(Clone, Debug)]
pub enum ProviderConfig {
    Claude(ClaudeCliConfig),
    Glm(ClaudeCliConfig),
    MiniMax(ClaudeCliConfig),
    Qwen(QwenCodeCliConfig),
    Codex(CodexCliConfig),
    Nemotron(CodexCliConfig), // Reuses the Codex CLI adapter
    Gemini(GeminiCliConfig),
    OpenRouter(OpenRouterConfig),
    OpenRouterCli(OpenRouterCliConfig),
    Kimi(KimiCliConfig),
}
impl ProviderConfig {
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Claude(_) => ProviderKind::Claude,
            Self::Glm(_) => ProviderKind::Glm,
            Self::MiniMax(_) => ProviderKind::MiniMax,
            Self::Qwen(_) => ProviderKind::Qwen,
            Self::Codex(_) => ProviderKind::Codex,
            Self::Nemotron(_) => ProviderKind::Nemotron,
            Self::Gemini(_) => ProviderKind::Gemini,
            Self::OpenRouter(_) => ProviderKind::OpenRouter,
            Self::OpenRouterCli(_) => ProviderKind::OpenRouter,
            Self::Kimi(_) => ProviderKind::Kimi,
        }
    }
}
