use std::collections::VecDeque;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use rust_debug::tracing::{debug, error};
use serde_json::Value;
use crate::summary::JsonSummary;
#[derive(Debug, Clone)]
pub struct StreamMessage {
    pub response: String,
    pub command: String,
    pub session_id: Option<String>,
    pub summary: Option<JsonSummary>,
    pub is_final: bool,
}
pub struct KillerGuard {
    done: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl KillerGuard {
    pub fn new(done: Arc<AtomicBool>, handle: std::thread::JoinHandle<()>) -> Self {
        Self {
            done,
            handle: Some(handle),
        }
    }
}
impl Drop for KillerGuard {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
pub struct CliProcess {
    child: Arc<Mutex<Child>>,
    _killer_guard: KillerGuard,
    stderr_handle: Option<std::thread::JoinHandle<String>>,
    stop_signal: Arc<AtomicBool>,
}
pub struct CliFinish {
    pub exit_status: Option<ExitStatus>,
    pub stderr_tail: String,
    pub wait_error: Option<String>,
}
impl CliProcess {
    pub fn spawn(
        cmd: &mut Command,
        provider_name: &str,
        command_str: &str,
        tx: &std::sync::mpsc::Sender<StreamMessage>,
        stop_signal: Arc<AtomicBool>,
        stderr_tail_lines: usize,
    ) -> Option<(Self, BufReader<ChildStdout>)> {
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                error!(error=%e, provider=%provider_name, "failed to spawn command");
                let _ = tx.send(StreamMessage {
                    response: format!("Failed to spawn {} command: {}", provider_name, e),
                    command: command_str.to_string(),
                    session_id: None,
                    summary: None,
                    is_final: true,
                });
                return None;
            }
        };

        debug!(pid = child.id(), provider = %provider_name, "cli spawned");

        let stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = tx.send(StreamMessage {
                    response: "Failed to capture stdout".to_string(),
                    command: command_str.to_string(),
                    session_id: None,
                    summary: None,
                    is_final: true,
                });
                return None;
            }
        };

        let child = Arc::new(Mutex::new(child));
        let done_flag = Arc::new(AtomicBool::new(false));
        let child_for_killer = Arc::clone(&child);
        let stop_for_killer = Arc::clone(&stop_signal);
        let done_for_killer = Arc::clone(&done_flag);
        let killer_handle = std::thread::spawn(move || loop {
            if done_for_killer.load(Ordering::Relaxed) {
                return;
            }
            if stop_for_killer.load(Ordering::Relaxed) {
                if let Ok(mut child) = child_for_killer.lock() {
                    terminate_child_process_group(&mut child);
                }
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        });
        let killer_guard = KillerGuard::new(done_flag, killer_handle);

        let stderr = {
            let mut lock = match child.lock() {
                Ok(child) => child,
                Err(poisoned) => poisoned.into_inner(),
            };
            lock.stderr.take()
        };
        let stderr_handle = stderr.map(|stderr| {
            std::thread::spawn(move || {
                collect_stream_tail(BufReader::new(stderr), stderr_tail_lines)
            })
        });

        Some((
            Self {
                child,
                _killer_guard: killer_guard,
                stderr_handle,
                stop_signal,
            },
            BufReader::new(stdout),
        ))
    }

    pub fn is_stopped(&self) -> bool {
        self.stop_signal.load(Ordering::Relaxed)
    }

    pub fn kill(&self) {
        match self.child.lock() {
            Ok(mut child) => terminate_child_process_group(&mut child),
            Err(poisoned) => terminate_child_process_group(&mut poisoned.into_inner()),
        }
    }

    pub fn finish(self, provider_name: &str) -> CliFinish {
        let child_pid = match self.child.lock() {
            Ok(child) => child.id(),
            Err(poisoned) => poisoned.into_inner().id(),
        };
        debug!(
            provider = %provider_name,
            pid = child_pid,
            "waiting for cli child exit after stdout closed"
        );
        let (exit_status, wait_error) = match self.child.lock() {
            Ok(mut child) => match child.wait() {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error.to_string())),
            },
            Err(poisoned) => match poisoned.into_inner().wait() {
                Ok(status) => (Some(status), None),
                Err(error) => (None, Some(error.to_string())),
            },
        };
        let stderr_tail = join_stderr_thread(self.stderr_handle);
        debug!(
            provider = %provider_name,
            pid = child_pid,
            exit_status = ?exit_status,
            wait_error = ?wait_error,
            stderr_tail_len = stderr_tail.len(),
            "cli child exit wait completed"
        );
        CliFinish {
            exit_status,
            stderr_tail,
            wait_error,
        }
    }
}
fn terminate_child_process_group(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    if pid <= 0 {
        let _ = child.kill();
        return;
    }

    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    for _ in 0..10 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
}
pub struct ErrorContexts {
    pub stderr: String,
    pub stdout: String,
    pub response: String,
    pub error_detail: String,
}
impl ErrorContexts {
    pub fn build(
        stderr_tail: &str,
        stdout_tail: &VecDeque<String>,
        response_content: &str,
        last_error_detail: &Option<String>,
        tail_max_lines: usize,
    ) -> Self {
        let stderr = if stderr_tail.is_empty() {
            String::new()
        } else {
            format!("\n\nRecent stderr output:\n{}", stderr_tail)
        };
        let stdout = if stdout_tail.is_empty() {
            String::new()
        } else {
            format!(
                "\n\nRecent stdout output:\n{}",
                stdout_tail.iter().cloned().collect::<Vec<_>>().join("\n")
            )
        };
        let response = {
            let tail = tail_text_lines(response_content, tail_max_lines);
            if tail.is_empty() {
                String::new()
            } else {
                format!("\n\nRecent response output:\n{}", tail)
            }
        };
        let error_detail = last_error_detail
            .as_ref()
            .map(|detail| format!("\n\nRecent error detail:\n{}", detail))
            .unwrap_or_default();
        Self {
            stderr,
            stdout,
            response,
            error_detail,
        }
    }

    pub fn best_failure_detail(
        &self,
        exit_status: Option<&std::process::ExitStatus>,
        wait_error: Option<&str>,
    ) -> String {
        if !self.stderr.is_empty() {
            self.stderr.clone()
        } else if !self.stdout.is_empty() {
            self.stdout.clone()
        } else if !self.error_detail.is_empty() {
            self.error_detail.clone()
        } else if !self.response.is_empty() {
            self.response.clone()
        } else if let Some(wait_error) = wait_error {
            format!("\n\nFailed to wait for child process: {}", wait_error)
        } else if let Some(exit_status) = exit_status {
            format!("\n\nExit status: {}", exit_status)
        } else {
            "\n\nExit status unavailable".to_string()
        }
    }
}
pub fn collect_stream_tail<R: BufRead>(reader: R, max_lines: usize) -> String {
    if max_lines == 0 {
        return String::new();
    }

    let mut tail = VecDeque::with_capacity(max_lines);
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        if tail.len() == max_lines {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    tail.into_iter().collect::<Vec<_>>().join("\n")
}
pub fn tail_text_lines(text: &str, max_lines: usize) -> String {
    if max_lines == 0 || text.trim().is_empty() {
        return String::new();
    }

    let mut tail = VecDeque::with_capacity(max_lines);
    for line in text.lines() {
        if tail.len() == max_lines {
            tail.pop_front();
        }
        tail.push_back(line);
    }
    tail.into_iter().collect::<Vec<_>>().join("\n")
}
pub fn join_stderr_thread(handle: Option<std::thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default()
}
pub fn extract_error_detail(json: &Value) -> Option<String> {
    if let Some(error) = json.get("error") {
        let mut parts: Vec<String> = Vec::new();

        // Primary message
        if let Some(message) = error.get("message").and_then(|v| v.as_str()) {
            parts.push(message.to_string());
        } else if let Some(message) = error.as_str() {
            parts.push(message.to_string());
        }

        // Error code (OpenRouter returns e.g. 429 for rate limits)
        if let Some(code) = error.get("code").and_then(|v| v.as_i64()) {
            parts.push(format!("(HTTP {})", code));
        } else if let Some(code) = error.get("code").and_then(|v| v.as_str()) {
            parts.push(format!("(code: {})", code));
        }

        // Provider metadata (OpenRouter includes provider_name, raw error, etc.)
        if let Some(metadata) = error.get("metadata").and_then(|v| v.as_object()) {
            if let Some(provider) = metadata.get("provider_name").and_then(|v| v.as_str()) {
                parts.push(format!("[provider: {}]", provider));
            }
            if let Some(raw) = metadata.get("raw").and_then(|v| v.as_str()) {
                if !raw.is_empty() && raw.len() < 500 {
                    parts.push(format!("raw: {}", raw));
                }
            }
        }

        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }

    if let Some(status) = json.get("status").and_then(|value| value.as_str()) {
        if status != "success" {
            if let Some(message) = json.get("message").and_then(|value| value.as_str()) {
                return Some(message.to_string());
            }
            return Some(format!("status: {}", status));
        }
    }

    if let Some(code) = json.get("status_code").and_then(|value| value.as_i64()) {
        return Some(format!("status code: {}", code));
    }
    if let Some(code) = json.get("code").and_then(|value| value.as_i64()) {
        return Some(format!("code: {}", code));
    }
    if let Some(code) = json.get("code").and_then(|value| value.as_str()) {
        return Some(format!("code: {}", code));
    }

    None
}
