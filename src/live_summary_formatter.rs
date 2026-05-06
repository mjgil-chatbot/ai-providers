use crate::summary::format_tool_input_preview;
use serde_json::Value;

const TOOL_COMMAND_PREVIEW_MAX_CHARS: usize = 57;
const TOOL_GREP_PREVIEW_MAX_CHARS: usize = 37;

#[derive(Clone, Copy)]
pub struct LiveSummaryFormatter {
    fallback_model_label: &'static str,
}

impl LiveSummaryFormatter {
    pub const fn new(fallback_model_label: &'static str) -> Self {
        Self {
            fallback_model_label,
        }
    }

    pub fn compose_response(&self, streamed_summary: &str, actual_text_content: &str) -> String {
        match (
            streamed_summary.trim().is_empty(),
            actual_text_content.trim().is_empty(),
        ) {
            (false, false) => format!("{streamed_summary}\n\n{actual_text_content}"),
            (false, true) => streamed_summary.to_string(),
            (true, false) => actual_text_content.to_string(),
            (true, true) => String::new(),
        }
    }

    pub fn bullet_line(&self, detail: &str) -> String {
        detail.to_string()
    }

    pub fn started_line(&self, model: Option<&str>) -> String {
        self.bullet_line(&format!("{} agent started", self.model_label(model)))
    }

    pub fn completion_line(
        &self,
        model: Option<&str>,
        duration_ms: u64,
        cost_usd: f64,
        turns: u64,
    ) -> String {
        self.bullet_line(&format!(
            "{} complete | {}ms | ${:.4} | {} turns",
            self.model_label(model),
            duration_ms,
            cost_usd,
            turns,
        ))
    }

    pub fn detail_line(&self, model: Option<&str>, detail: &str) -> String {
        self.bullet_line(&format!("{} | {}", self.model_label(model), detail))
    }

    pub fn user_message_line(&self) -> String {
        self.bullet_line("User message")
    }

    pub fn tool_detail(&self, tool_name: &str, input: &Value) -> String {
        match tool_name {
            "Bash" | "bash" | "Shell" | "shell" | "mcp__bash__run" => input
                .get("command")
                .and_then(|value| value.as_str())
                .map(|command| {
                    let normalized = unwrap_shell_wrapper(command).unwrap_or(command);
                    format!(
                        "Bash {}",
                        markdown_inline_code(&truncate_with_ellipsis(
                            normalized.lines().next().unwrap_or(normalized),
                            TOOL_COMMAND_PREVIEW_MAX_CHARS,
                        ))
                    )
                })
                .unwrap_or_else(|| "Bash".to_string()),
            "Read" | "read" | "View" => input
                .get("file_path")
                .and_then(|value| value.as_str())
                .map(|path| format!("Read {}", path.rsplit('/').next().unwrap_or(path)))
                .unwrap_or_else(|| "Read".to_string()),
            "Write" | "write" => input
                .get("file_path")
                .and_then(|value| value.as_str())
                .map(|path| format!("Write {}", path.rsplit('/').next().unwrap_or(path)))
                .unwrap_or_else(|| "Write".to_string()),
            "Edit" | "edit" => input
                .get("file_path")
                .and_then(|value| value.as_str())
                .map(|path| format!("Edit {}", path.rsplit('/').next().unwrap_or(path)))
                .unwrap_or_else(|| "Edit".to_string()),
            "Glob" | "glob" => input
                .get("pattern")
                .and_then(|value| value.as_str())
                .map(|pattern| format!("Glob {}", markdown_inline_code(pattern)))
                .unwrap_or_else(|| "Glob".to_string()),
            "Grep" | "grep" => input
                .get("pattern")
                .and_then(|value| value.as_str())
                .map(|pattern| {
                    format!(
                        "Grep {}",
                        markdown_inline_code(&truncate_with_ellipsis(
                            pattern,
                            TOOL_GREP_PREVIEW_MAX_CHARS,
                        ))
                    )
                })
                .unwrap_or_else(|| "Grep".to_string()),
            _ if tool_name.starts_with("mcp__") => self.mcp_tool_detail(tool_name, input),
            _ => tool_name.to_string(),
        }
    }

    pub fn command_execution_line(&self, model: Option<&str>, command: &str) -> String {
        let normalized = unwrap_shell_wrapper(command).unwrap_or(command);
        let label = match normalized.split_whitespace().next() {
            Some("rg") | Some("grep") => "Grep",
            _ => "Bash",
        };
        self.detail_line(
            model,
            &format!(
                "{} {}",
                label,
                markdown_inline_code(&truncate_with_ellipsis(
                    normalized.lines().next().unwrap_or(normalized),
                    120,
                ))
            ),
        )
    }

    pub fn mcp_tool_line(&self, model: Option<&str>, tool_name: &str, input: &Value) -> String {
        self.detail_line(model, &self.mcp_tool_detail(tool_name, input))
    }

    fn mcp_tool_detail(&self, tool_name: &str, input: &Value) -> String {
        let label = mcp_tool_label(tool_name);
        let preview = markdownify_tool_preview(&format_tool_input_preview(tool_name, input));
        if preview.is_empty() {
            label
        } else {
            format!("{label} {preview}")
        }
    }

    fn model_label(&self, model: Option<&str>) -> String {
        friendly_model_label(
            model.unwrap_or(self.fallback_model_label),
            self.fallback_model_label,
        )
    }
}

fn friendly_model_label(model: &str, fallback: &str) -> String {
    let lower = model.to_ascii_lowercase();
    if model == "unknown" {
        return fallback.to_string();
    }
    if lower.contains("gpt-5.4-high") {
        return "GPT-5.4-High".to_string();
    }
    if lower.contains("gpt-5.4") {
        return "GPT-5.4".to_string();
    }
    if lower.contains("gpt-5") {
        return "GPT-5".to_string();
    }
    if lower.contains("opus") {
        return "Opus".to_string();
    }
    if lower.contains("haiku") {
        return "Haiku".to_string();
    }
    if lower.contains("sonnet") {
        return "Sonnet".to_string();
    }
    if lower.contains("minimax") {
        return minimax_friendly_label(&lower);
    }
    if lower.contains("gemini") {
        return "Gemini".to_string();
    }
    if lower.contains("codex") {
        return "Codex".to_string();
    }
    model.to_string()
}

fn minimax_friendly_label(model_lower: &str) -> String {
    let version = if model_lower.contains("m2.7") {
        "2.7"
    } else if model_lower.contains("m2.5") {
        "2.5"
    } else if model_lower.contains("m2.1") {
        "2.1"
    } else if model_lower.contains("m2") {
        "2"
    } else {
        return "MiniMax".to_string();
    };

    let hs = if model_lower.contains("highspeed") {
        "-HS"
    } else {
        ""
    };

    format!("MM-{}{}", version, hs)
}

fn mcp_tool_label(tool_name: &str) -> String {
    match tool_name
        .strip_prefix("mcp__")
        .and_then(|rest| rest.split_once("__"))
    {
        Some((server, tool)) => format!("MCP {}.{}", server, tool),
        None => tool_name.to_string(),
    }
}

fn markdownify_tool_preview(preview: &str) -> String {
    if preview.trim().is_empty() {
        return String::new();
    }
    preview
        .split(" | ")
        .map(markdownify_preview_segment)
        .collect::<Vec<_>>()
        .join(" | ")
}

fn markdownify_preview_segment(segment: &str) -> String {
    match segment.split_once('=') {
        Some((key, value)) if !value.trim().is_empty() => {
            format!("{key}={}", markdown_inline_code(value))
        }
        _ => markdown_inline_code(segment),
    }
}

fn markdown_inline_code(text: &str) -> String {
    if text.contains('`') {
        format!("`` {text} ``")
    } else {
        format!("`{text}`")
    }
}

fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        text.to_string()
    }
}

fn unwrap_shell_wrapper(command: &str) -> Option<&str> {
    for prefix in [
        "/bin/bash -lc ",
        "/usr/bin/bash -lc ",
        "bash -lc ",
        "sh -lc ",
        "zsh -lc ",
    ] {
        let rest = command.strip_prefix(prefix)?;
        if rest.len() >= 2 {
            let quote = rest.chars().next()?;
            if (quote == '\'' || quote == '"') && rest.ends_with(quote) {
                return Some(&rest[1..rest.len() - 1]);
            }
        }
        return Some(rest);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::LiveSummaryFormatter;
    use serde_json::json;

    #[test]
    fn compose_response_places_summary_above_text() {
        let formatter = LiveSummaryFormatter::new("Agent");
        let rendered = formatter.compose_response("Agent | Bash `pwd`", "final answer");
        assert_eq!(rendered, "Agent | Bash `pwd`\n\nfinal answer");
    }

    #[test]
    fn gpt_54_high_label_is_preferred_over_generic_codex() {
        let formatter = LiveSummaryFormatter::new("Codex");
        let rendered = formatter.started_line(Some("gpt-5.4-high"));
        assert_eq!(rendered, "GPT-5.4-High agent started");
    }

    #[test]
    fn bash_tool_detail_unwraps_shell_wrapper() {
        let formatter = LiveSummaryFormatter::new("Codex");
        let detail = formatter.tool_detail(
            "bash",
            &json!({"command": "/bin/bash -lc 'git status --short --branch'"}),
        );
        assert_eq!(detail, "Bash `git status --short --branch`");
    }

    #[test]
    fn bash_tool_detail_uses_shared_preview_limit() {
        let formatter = LiveSummaryFormatter::new("Agent");
        let detail = formatter.tool_detail(
            "bash",
            &json!({"command": "012345678901234567890123456789012345678901234567890123456789"}),
        );
        assert_eq!(
            detail,
            "Bash `012345678901234567890123456789012345678901234567890123456...`"
        );
    }

    #[test]
    fn mcp_preview_is_markdownified() {
        let formatter = LiveSummaryFormatter::new("Codex");
        let line = formatter.mcp_tool_line(
            Some("gpt-5.4-high"),
            "mcp__lightweight_builds__run_build",
            &json!({
                "project_path": "/tmp/project",
                "command_override": ["cargo", "test", "--lib"]
            }),
        );
        assert!(line.starts_with("GPT-5.4-High | MCP lightweight_builds.run_build "));
        assert!(line.contains("project_path=`/tmp/project`"));
    }
}
