use std::sync::atomic::AtomicBool;
use std::sync::{mpsc, Arc};

use anyhow::Result;

use crate::config::{
    ClaudeCliConfig, CodexCliConfig, GeminiCliConfig, KimiCliConfig, OpenRouterCliConfig,
    OpenRouterConfig, QwenCodeCliConfig,
};
use crate::memory::MemoryContext;
use crate::session::ProviderSession;
use crate::stream::StreamMessage;

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

pub fn build_provider_command_string(
    config: &ProviderConfig,
    system_prompt: Option<&str>,
    query: &str,
    session: Option<&ProviderSession>,
    memory_context: Option<&MemoryContext>,
) -> String {
    match config {
        ProviderConfig::Claude(config) => crate::claude::build_claude_command_string(
            system_prompt,
            session,
            session.and_then(|session| session.claude_session_id.as_deref()),
            query,
            config,
        ),
        ProviderConfig::Glm(config) => crate::claude::build_claude_command_string(
            system_prompt,
            session,
            session.and_then(|session| session.glm_session_id.as_deref()),
            query,
            config,
        ),
        ProviderConfig::MiniMax(config) => crate::claude::build_claude_command_string(
            system_prompt,
            session,
            session.and_then(|session| session.minimax_session_id.as_deref()),
            query,
            config,
        ),
        ProviderConfig::Qwen(config) => crate::qwen_code::build_qwen_code_command_string(
            system_prompt,
            query,
            session,
            memory_context,
            config,
        ),
        ProviderConfig::Codex(config) => crate::codex::build_codex_command_string(
            system_prompt,
            session.and_then(|session| session.codex_session_id.as_deref()),
            query,
            session.and_then(|session| session.working_dir.as_deref()),
            config,
        ),
        ProviderConfig::Nemotron(config) => crate::codex::build_codex_command_string(
            system_prompt,
            session.and_then(|session| session.nemotron_session_id.as_deref()),
            query,
            session.and_then(|session| session.working_dir.as_deref()),
            config,
        ),
        ProviderConfig::Gemini(config) => crate::gemini::build_gemini_command_string(
            system_prompt,
            query,
            session,
            memory_context,
            config,
        ),
        ProviderConfig::OpenRouter(config) => crate::openrouter::build_openrouter_command_string(
            system_prompt,
            query,
            session,
            memory_context,
            config,
        ),
        ProviderConfig::OpenRouterCli(config) => {
            crate::openrouter::build_openrouter_cli_command_string(
                system_prompt,
                query,
                session,
                memory_context,
                config,
            )
        }
        ProviderConfig::Kimi(config) => crate::kimi::build_kimi_command_string(
            system_prompt,
            query,
            session,
            memory_context,
            config,
        ),
    }
}

pub fn call_provider_stream(
    config: &ProviderConfig,
    query: &str,
    system_prompt: Option<&str>,
    memory_context: Option<&MemoryContext>,
    session: Option<ProviderSession>,
    tx: mpsc::Sender<StreamMessage>,
    stop_signal: Arc<AtomicBool>,
) -> Result<()> {
    match config {
        ProviderConfig::Claude(config) => crate::claude::call_claude_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Glm(config) => crate::claude::call_claude_cli_stream(
            query,
            system_prompt,
            memory_context,
            session.map(|mut session| {
                session.claude_session_id = session.glm_session_id.clone();
                session
            }),
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::MiniMax(config) => crate::claude::call_claude_cli_stream(
            query,
            system_prompt,
            memory_context,
            session.map(|mut session| {
                session.claude_session_id = session.minimax_session_id.clone();
                session
            }),
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Qwen(config) => crate::qwen_code::call_qwen_code_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Codex(config) => crate::codex::call_codex_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Nemotron(config) => crate::codex::call_codex_cli_stream(
            query,
            system_prompt,
            memory_context,
            session.map(|mut session| {
                session.codex_session_id = session.nemotron_session_id.clone();
                session
            }),
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Gemini(config) => crate::gemini::call_gemini_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::OpenRouter(config) => crate::openrouter::call_openrouter_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::OpenRouterCli(config) => crate::openrouter::call_openrouter_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
        ProviderConfig::Kimi(config) => crate::kimi::call_kimi_cli_stream(
            query,
            system_prompt,
            memory_context,
            session,
            config,
            tx,
            stop_signal,
        ),
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn openrouter_cli_config_reports_openrouter_kind() {
        let fixture = ProviderConfig::OpenRouterCli(OpenRouterCliConfig::default());
        let actual = fixture.kind();
        let expected = ProviderKind::OpenRouter;
        assert_eq!(actual, expected);
    }
}
