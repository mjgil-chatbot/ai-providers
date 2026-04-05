use std::path::{Path, PathBuf};

use crate::sandbox::RunscMount;

#[derive(Clone, Debug)]
pub struct ClaudeCliConfig {
    pub cli: String,
    pub model: String,
    pub json_schema: Option<String>,
    pub output_format: String,
    pub skip_permissions: bool,
    pub verbose_output: bool,
    pub allow_api_billing: bool,
    pub config_dir: Option<PathBuf>,
    pub require_mcp_servers: bool,
    pub extra_env: Vec<(String, String)>,
    pub runsc_image: Option<String>,
    pub runsc_workspace_includes: Vec<String>,
    pub runsc_mounts: Vec<RunscMount>,
}

impl Default for ClaudeCliConfig {
    fn default() -> Self {
        Self {
            cli: "claude".to_string(),
            model: "claude-opus-4-6".to_string(),
            json_schema: None,
            output_format: "stream-json".to_string(),
            skip_permissions: true,
            verbose_output: true,
            allow_api_billing: false,
            config_dir: None,
            require_mcp_servers: false,
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct CodexCliConfig {
    pub cli: String,
    pub model: Option<String>,
    pub output_schema: Option<PathBuf>,
    pub json_stream: bool,
    pub reasoning_effort: Option<String>,
    pub sandbox_mode: Option<String>,
    pub force_skip_git_repo_check: bool,
    pub skip_git_repo_check_prefixes: Vec<PathBuf>,
    pub extra_env: Vec<(String, String)>,
    pub runsc_image: Option<String>,
    pub runsc_workspace_includes: Vec<String>,
    pub runsc_mounts: Vec<RunscMount>,
}

impl Default for CodexCliConfig {
    fn default() -> Self {
        Self {
            cli: "codex".to_string(),
            model: None,
            output_schema: None,
            json_stream: true,
            reasoning_effort: None,
            sandbox_mode: None,
            force_skip_git_repo_check: false,
            skip_git_repo_check_prefixes: Vec::new(),
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct GeminiCliConfig {
    pub cli: String,
    pub model: Option<String>,
    pub output_format: String,
    pub approval_mode: Option<String>,
    pub include_directories: Vec<String>,
    pub project_settings_path: Option<PathBuf>,
    pub extra_env: Vec<(String, String)>,
    pub runsc_image: Option<String>,
    pub runsc_workspace_includes: Vec<String>,
    pub runsc_mounts: Vec<RunscMount>,
}

impl Default for GeminiCliConfig {
    fn default() -> Self {
        Self {
            cli: "gemini".to_string(),
            model: None,
            output_format: "stream-json".to_string(),
            approval_mode: None,
            include_directories: Vec::new(),
            project_settings_path: None,
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenRouterConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub max_tokens: u32,
    /// Model context window size in tokens (from OpenRouter `/api/v1/models`).
    pub context_window_size: u64,
    /// Provider-level system prompt prepended before the session system prompt.
    pub default_system_prompt: Option<String>,
}

impl Default for OpenRouterConfig {
    fn default() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".to_string(),
            api_key: String::new(),
            model: String::new(),
            max_tokens: 16384,
            context_window_size: 0,
            default_system_prompt: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct OpenRouterCliConfig {
    pub cli: String,
    pub model: String,
    pub api_key: String,
    pub base_url: String,
    pub context_window_size: u64,
    pub default_system_prompt: Option<String>,
    pub resume_session_id: Option<String>,
    pub workspace_root: Option<String>,
    pub extra_env: Vec<(String, String)>,
    /// Path to .mcp.json for MCP server definitions.
    pub mcp_config: Option<String>,
}

impl Default for OpenRouterCliConfig {
    fn default() -> Self {
        Self {
            cli: "openrouter-cli".to_string(),
            model: "qwen/qwen3.6-plus:free".to_string(),
            api_key: String::new(),
            base_url: "https://openrouter.ai/api/v1".to_string(),
            context_window_size: 0,
            default_system_prompt: None,
            resume_session_id: None,
            workspace_root: None,
            extra_env: Vec::new(),
            mcp_config: None,
        }
    }
}

impl From<OpenRouterCliConfig> for OpenRouterConfig {
    fn from(c: OpenRouterCliConfig) -> Self {
        Self {
            base_url: c.base_url,
            api_key: c.api_key,
            model: c.model,
            max_tokens: 16384,
            context_window_size: c.context_window_size,
            default_system_prompt: c.default_system_prompt,
        }
    }
}

#[derive(Clone, Debug)]
pub struct QwenCodeCliConfig {
    pub cli: String,
    pub model: Option<String>,
    pub output_format: String,
    pub yolo: bool,
    pub append_system_prompt: Option<String>,
    pub resume_session_id: Option<String>,
    pub include_directories: Vec<String>,
    pub allowed_mcp_server_names: Vec<String>,
    pub auth_type: Option<String>,
    pub extra_env: Vec<(String, String)>,
    pub runsc_image: Option<String>,
    pub runsc_workspace_includes: Vec<String>,
    pub runsc_mounts: Vec<RunscMount>,
}

impl Default for QwenCodeCliConfig {
    fn default() -> Self {
        Self {
            cli: "qwen".to_string(),
            model: None,
            output_format: "stream-json".to_string(),
            yolo: true,
            append_system_prompt: None,
            resume_session_id: None,
            include_directories: Vec::new(),
            allowed_mcp_server_names: Vec::new(),
            auth_type: Some("qwen-oauth".to_string()),
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct KimiCliConfig {
    pub cli: String,
    pub model: Option<String>,
    pub output_format: String,
    pub yolo: bool,
    pub resume_session_id: Option<String>,
    pub extra_env: Vec<(String, String)>,
    pub runsc_image: Option<String>,
    pub runsc_workspace_includes: Vec<String>,
    pub runsc_mounts: Vec<RunscMount>,
}

impl Default for KimiCliConfig {
    fn default() -> Self {
        Self {
            cli: "kimi".to_string(),
            model: None,
            output_format: "stream-json".to_string(),
            yolo: true,
            resume_session_id: None,
            extra_env: Vec::new(),
            runsc_image: None,
            runsc_workspace_includes: Vec::new(),
            runsc_mounts: Vec::new(),
        }
    }
}

pub fn prompt_needs_hyphen_guard(prompt: &str) -> bool {
    prompt.trim_start().starts_with('-')
}

pub fn format_prompt_arg(prompt: &str) -> String {
    let escaped = prompt.replace('\\', "\\\\").replace('"', "\\\"");
    format!("-p \"{}\"", escaped)
}

pub fn add_prompt_arg(cmd: &mut std::process::Command, prompt: &str) {
    cmd.arg("-p").arg(prompt);
}

pub fn has_mcp_servers(config_dir: Option<&Path>) -> bool {
    let Some(path) = claude_config_path(config_dir) else {
        return false;
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return false,
    };

    let json: serde_json::Value = match serde_json::from_str(&content) {
        Ok(json) => json,
        Err(_) => return false,
    };

    let top_level = json.get("mcpServers").map(has_entries).unwrap_or(false);
    if top_level {
        return true;
    }

    json.get("projects")
        .and_then(|projects| projects.as_object())
        .map(|projects| {
            projects
                .values()
                .any(|project| project.get("mcpServers").map(has_entries).unwrap_or(false))
        })
        .unwrap_or(false)
}

fn claude_config_path(config_dir: Option<&Path>) -> Option<PathBuf> {
    claude_config_dir(config_dir).map(|config_dir| config_dir.join(".claude.json"))
}

fn claude_config_dir(config_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(config_dir) = config_dir {
        return Some(config_dir.to_path_buf());
    }

    if let Some(config_dir) =
        std::env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(config_dir));
    }

    dirs::home_dir()
}

fn has_entries(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .map(|value| !value.is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::has_mcp_servers;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn detects_project_scoped_mcp_servers() {
        let dir = unique_temp_dir("config");
        fs::write(
            dir.join(".claude.json"),
            r#"{"projects":{"/tmp/project":{"mcpServers":{"memory":{}}}}}"#,
        )
        .unwrap();

        assert!(has_mcp_servers(Some(&dir)));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detects_top_level_mcp_servers() {
        let dir = unique_temp_dir("config");
        fs::write(dir.join(".claude.json"), r#"{"mcpServers":{"memory":{}}}"#).unwrap();

        assert!(has_mcp_servers(Some(&dir)));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detects_mcp_servers_from_claude_config_dir_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = unique_temp_dir("config-env");
        fs::write(dir.join(".claude.json"), r#"{"mcpServers":{"memory":{}}}"#).unwrap();

        let previous = std::env::var_os("CLAUDE_CONFIG_DIR");
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);

        assert!(has_mcp_servers(None));

        match previous {
            Some(previous) => std::env::set_var("CLAUDE_CONFIG_DIR", previous),
            None => std::env::remove_var("CLAUDE_CONFIG_DIR"),
        }

        fs::remove_dir_all(dir).unwrap();
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ai-providers-{name}-{suffix}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
