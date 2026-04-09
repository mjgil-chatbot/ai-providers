use std::collections::VecDeque;
use std::io::BufRead;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use anyhow::Result;
use rust_debug::tracing::{error, info, warn};
use serde_json::Value;
pub use super::tool_exec::TextToolCall;
/// Maximum number of agent loop iterations (tool call → result → next response).
const MAX_TOOL_ITERATIONS: usize = 25;
const STDERR_TAIL_MAX_LINES: usize = 120;
