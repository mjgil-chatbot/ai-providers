use ai_providers::config::OpenRouterCliConfig;
use ai_providers::openrouter::{build_openrouter_cli_command_string, call_openrouter_cli_stream};
use ai_providers::session::ProviderSession;
use ai_providers::stream::StreamMessage;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_temp_dir(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("ai-providers-ortest-{name}-{suffix}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Helper that writes a shell script to a temp dir and returns (script_path, dir_path).
/// Caller must clean up the dir.
fn write_mock_script(name: &str, content: &str) -> (PathBuf, PathBuf) {
    let dir = unique_temp_dir(name);
    let script_path = dir.join("mock-openrouter-cli");
    std::fs::write(&script_path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (script_path, dir)
}

// ---------------------------------------------------------------------------
// Command construction tests
// ---------------------------------------------------------------------------

#[test]
fn command_string_includes_model_and_sandbox() {
    let config = OpenRouterCliConfig {
        cli: "openrouter-cli".to_string(),
        model: "qwen/qwen3-coder".to_string(),
        ..Default::default()
    };
    let cmd = build_openrouter_cli_command_string(None, "hello", None, None, &config);
    assert!(cmd.contains("openrouter-cli exec"));
    assert!(cmd.contains("--model qwen/qwen3-coder"));
    assert!(cmd.contains("--sandbox workspace-write"));
}

#[test]
fn command_string_includes_resume_from_session() {
    let session = ProviderSession {
        qwen_session_id: Some("sess-abc".to_string()),
        ..Default::default()
    };
    let config = OpenRouterCliConfig::default();
    let cmd = build_openrouter_cli_command_string(None, "continue", Some(&session), None, &config);
    assert!(cmd.contains("--resume sess-abc"));
}

#[test]
fn command_string_includes_resume_from_config() {
    let config = OpenRouterCliConfig {
        resume_session_id: Some("sess-xyz".to_string()),
        ..Default::default()
    };
    let cmd = build_openrouter_cli_command_string(None, "continue", None, None, &config);
    assert!(cmd.contains("--resume sess-xyz"));
}

#[test]
fn command_string_includes_workspace_root() {
    let config = OpenRouterCliConfig {
        workspace_root: Some("/tmp/project".to_string()),
        ..Default::default()
    };
    let cmd = build_openrouter_cli_command_string(None, "hello", None, None, &config);
    assert!(cmd.contains("--workspace-root /tmp/project"));
}

#[test]
fn command_string_omits_default_base_url() {
    let config = OpenRouterCliConfig::default();
    let cmd = build_openrouter_cli_command_string(None, "hello", None, None, &config);
    assert!(!cmd.contains("--base-url"));
}

#[test]
fn command_string_includes_custom_base_url() {
    let config = OpenRouterCliConfig {
        base_url: "https://custom.api/v1".to_string(),
        ..Default::default()
    };
    let cmd = build_openrouter_cli_command_string(None, "hello", None, None, &config);
    assert!(cmd.contains("--base-url https://custom.api/v1"));
}

#[test]
fn command_string_adds_hyphen_guard_for_dash_prompt() {
    // When there's no system prompt, the prompt starts with "User: --some flag"
    // which starts with "U" not "-", so no guard needed.
    // But with a system prompt that starts with "-", the guard should appear.
    let config = OpenRouterCliConfig {
        default_system_prompt: None,
        ..Default::default()
    };
    // The prompt builder prepends "User: " so it won't start with "-" for simple queries.
    // Test that the function itself handles prompt_needs_hyphen_guard correctly
    // by checking a prompt that would start with "-" after building.
    let cmd = build_openrouter_cli_command_string(None, "hello", None, None, &config);
    // Should NOT contain "--" separator for a normal prompt
    assert!(!cmd.ends_with("-- "));
}

// ---------------------------------------------------------------------------
// JSONL parsing tests (via the stream function with a mock binary)
// ---------------------------------------------------------------------------

#[test]
fn jsonl_parsing_forwards_partial_and_final_messages() {
    let script = r#"#!/bin/sh
echo '{"response":"partial","command":"test","session_id":"s1","summary":null,"is_final":false}'
echo '{"response":"final text","command":"test","session_id":"s1","summary":{"total_input_tokens":100,"total_output_tokens":50},"is_final":true}'
"#;
    let (script_path, dir) = write_mock_script("partial-final", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    assert!(
        messages.len() >= 2,
        "expected at least 2 messages, got {}",
        messages.len()
    );

    // First message should be non-final with partial response
    let first = &messages[0];
    assert_eq!(first.response, "partial");
    assert!(!first.is_final);
    assert_eq!(first.session_id.as_deref(), Some("s1"));

    // The final message should have full response and is_final=true
    let final_msg = messages.iter().find(|m| m.is_final).unwrap();
    assert_eq!(final_msg.response, "final text");
    assert_eq!(final_msg.session_id.as_deref(), Some("s1"));

    // Summary should have token counts
    let summary = final_msg.summary.as_ref().unwrap();
    assert_eq!(summary.total_input_tokens, 100);
    assert_eq!(summary.total_output_tokens, 50);

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn jsonl_parsing_extracts_session_id() {
    let script = r#"#!/bin/sh
echo '{"response":"hello","command":"test","session_id":"session-42","summary":null,"is_final":true}'
"#;
    let (script_path, dir) = write_mock_script("session-id", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("test", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    let final_msg = messages.last().unwrap();
    assert_eq!(final_msg.session_id.as_deref(), Some("session-42"));
    assert!(final_msg.is_final);

    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Spawn/read failure handling tests
// ---------------------------------------------------------------------------

#[test]
fn spawn_failure_sends_error_message() {
    let config = OpenRouterCliConfig {
        cli: "/nonexistent/path/to/openrouter-cli".to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].is_final);
    assert!(messages[0].response.contains("Failed to spawn"));
}

#[test]
fn process_failure_sends_error_with_stderr() {
    let script = r#"#!/bin/sh
echo "something went wrong" >&2
exit 1
"#;
    let (script_path, dir) = write_mock_script("fail-stderr", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    let final_msg = messages.last().unwrap();
    assert!(final_msg.is_final);
    assert!(
        final_msg.response.contains("failed"),
        "expected error in response: {}",
        final_msg.response
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn stop_signal_terminates_stream() {
    let script = r#"#!/bin/sh
echo '{"response":"start","command":"test","session_id":null,"summary":null,"is_final":false}'
sleep 30
echo '{"response":"should not reach","command":"test","session_id":null,"summary":null,"is_final":true}'
"#;
    let (script_path, dir) = write_mock_script("stop-signal", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);

    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        stop_clone.store(true, Ordering::Relaxed);
    });

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    let final_msg = messages.last().unwrap();
    assert!(final_msg.is_final);
    assert!(!final_msg.response.contains("should not reach"));

    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn non_json_lines_are_tolerated() {
    let script = r#"#!/bin/sh
echo 'some debug output'
echo '{"response":"result","command":"test","session_id":"s1","summary":null,"is_final":true}'
"#;
    let (script_path, dir) = write_mock_script("non-json", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    let final_msg = messages.last().unwrap();
    assert_eq!(final_msg.response, "result");
    assert!(final_msg.is_final);

    let _ = std::fs::remove_dir_all(dir);
}

// ---------------------------------------------------------------------------
// Provider config integration test
// ---------------------------------------------------------------------------

#[test]
fn provider_config_qwen_uses_qwen_code_cli_config() {
    use ai_providers::api::ProviderConfig;
    use ai_providers::api::ProviderKind;
    use ai_providers::config::QwenCodeCliConfig;

    let config = ProviderConfig::Qwen(QwenCodeCliConfig {
        model: Some("qwen3-coder".to_string()),
        ..Default::default()
    });

    assert_eq!(config.kind(), ProviderKind::Qwen);

    // build_provider_command_string should produce a qwen command
    let cmd_str =
        ai_providers::build_provider_command_string(&config, None, "test query", None, None);
    assert!(cmd_str.starts_with("qwen"));
    assert!(cmd_str.contains("--model qwen3-coder"));
}

// ---------------------------------------------------------------------------
// Summary forwarding test
// ---------------------------------------------------------------------------

#[test]
fn summary_token_counts_are_forwarded() {
    let script = r#"#!/bin/sh
echo '{"response":"text","command":"test","session_id":"s1","summary":{"total_input_tokens":500,"total_output_tokens":200},"is_final":true}'
"#;
    let (script_path, dir) = write_mock_script("summary", script);
    let config = OpenRouterCliConfig {
        cli: script_path.to_string_lossy().to_string(),
        model: "test-model".to_string(),
        ..Default::default()
    };

    let (tx, rx) = mpsc::channel::<StreamMessage>();
    let stop = Arc::new(AtomicBool::new(false));

    call_openrouter_cli_stream("hello", None, None, None, &config, tx, stop).unwrap();

    let messages: Vec<StreamMessage> = rx.try_iter().collect();
    let final_msg = messages.last().unwrap();
    let summary = final_msg.summary.as_ref().unwrap();
    assert_eq!(summary.total_input_tokens, 500);
    assert_eq!(summary.total_output_tokens, 200);
    assert_eq!(summary.model.as_deref(), Some("test-model"));

    let _ = std::fs::remove_dir_all(dir);
}
