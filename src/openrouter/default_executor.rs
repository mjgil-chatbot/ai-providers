use rust_debug::tracing::{info, warn};

use super::cli::{ToolExecResult, ToolExecutor};
use super::tool_exec::TextToolCall;

/// Execute a tool call with basic execution (no permission enforcement).
/// This is the default executor used by call_openrouter_stream for backward
/// compatibility. Callers should use stream_openrouter_turn with their own
/// executor for permission enforcement.
fn default_tool_executor(call: TextToolCall) -> ToolExecResult {
    use std::process::Command;

    match call.tool.as_str() {
        "bash" => {
            let command = call
                .arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if command.is_empty() {
                return ToolExecResult {
                    output: "Error: empty command".into(),
                    success: false,
                };
            }

            info!(
                command = command,
                "executing bash tool call (default executor)"
            );

            let result = Command::new("bash").arg("-c").arg(command).output();

            match result {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    let mut combined = String::new();
                    if !stdout.is_empty() {
                        combined.push_str(&stdout);
                    }
                    if !stderr.is_empty() {
                        if !combined.is_empty() {
                            combined.push('\n');
                        }
                        combined.push_str("[stderr] ");
                        combined.push_str(&stderr);
                    }
                    if combined.len() > 50_000 {
                        combined.truncate(50_000);
                        combined.push_str("\n... (output truncated at 50000 bytes)");
                    }
                    ToolExecResult {
                        output: if combined.is_empty() {
                            "(no output)".into()
                        } else {
                            combined
                        },
                        success: output.status.success(),
                    }
                }
                Err(err) => ToolExecResult {
                    output: format!("Error running command: {}", err),
                    success: false,
                },
            }
        }
        "read_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if path.is_empty() {
                return ToolExecResult {
                    output: "Error: missing 'path' argument".into(),
                    success: false,
                };
            }

            info!(
                path = path,
                "executing read_file tool call (default executor)"
            );

            match std::fs::read_to_string(path) {
                Ok(mut content) => {
                    if content.len() > 50_000 {
                        content.truncate(50_000);
                        content.push_str("\n... (file truncated at 50000 bytes)");
                    }
                    ToolExecResult {
                        output: content,
                        success: true,
                    }
                }
                Err(err) => ToolExecResult {
                    output: format!("Error reading file: {}", err),
                    success: false,
                },
            }
        }
        "write_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let content = call
                .arguments
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if path.is_empty() {
                return ToolExecResult {
                    output: "Error: missing 'path' argument".into(),
                    success: false,
                };
            }

            info!(
                path = path,
                "executing write_file tool call (default executor)"
            );

            match std::fs::write(path, content) {
                Ok(()) => ToolExecResult {
                    output: format!("Successfully wrote {} bytes", content.len()),
                    success: true,
                },
                Err(err) => ToolExecResult {
                    output: format!("Error writing file: {}", err),
                    success: false,
                },
            }
        }
        "edit_file" => {
            let path = call
                .arguments
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let old_string = call
                .arguments
                .get("old_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let new_string = call
                .arguments
                .get("new_string")
                .and_then(|v| v.as_str())
                .unwrap_or("");

            if path.is_empty() || old_string.is_empty() {
                return ToolExecResult {
                    output: "Error: missing 'path' or 'old_string' argument".into(),
                    success: false,
                };
            }

            info!(
                path = path,
                "executing edit_file tool call (default executor)"
            );

            let content = match std::fs::read_to_string(path) {
                Ok(c) => c,
                Err(err) => {
                    return ToolExecResult {
                        output: format!("Error reading file: {}", err),
                        success: false,
                    };
                }
            };

            let count = content.matches(old_string).count();
            if count == 0 {
                return ToolExecResult {
                    output: "Error: old_string not found in file".into(),
                    success: false,
                };
            }
            if count > 1 {
                return ToolExecResult {
                    output: format!("Error: old_string found {} times (must be unique)", count),
                    success: false,
                };
            }

            let updated = content.replacen(old_string, new_string, 1);
            match std::fs::write(path, &updated) {
                Ok(()) => ToolExecResult {
                    output: "Successfully edited file".into(),
                    success: true,
                },
                Err(err) => ToolExecResult {
                    output: format!("Error writing file: {}", err),
                    success: false,
                },
            }
        }
        other => {
            warn!(tool = other, "unknown tool requested by model");
            ToolExecResult {
                output: format!("Error: unknown tool '{}'", other),
                success: false,
            }
        }
    }
}

pub(super) fn default_executor() -> ToolExecutor {
    Box::new(default_tool_executor)
}
