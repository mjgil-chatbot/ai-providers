pub mod cli;
mod default_executor;
pub mod tool_exec;

pub use cli::{
    build_openrouter_command_string, call_openrouter_stream, call_openrouter_stream_with_executor,
    stream_openrouter_turn, ToolExecResult, TurnResult,
};
pub use tool_exec::TextToolCall;

pub mod cli_adapter;
pub use cli_adapter::{build_openrouter_cli_command_string, call_openrouter_cli_stream};
