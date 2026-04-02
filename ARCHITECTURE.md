# Architecture

`ai-providers` is the provider-runtime layer extracted from a larger chatbot
application. This repository keeps the execution and streaming boundary narrow:
it translates provider-specific CLIs and APIs into a shared stream model
without owning UI state, persistence, or higher-level workflow orchestration.

## Module layout

- `src/api.rs` exposes the provider-neutral entry points:
  `build_provider_command_string`, `call_provider_stream`, `ProviderConfig`,
  and `ProviderKind`.
- `src/config.rs` holds provider-specific configuration types and shared prompt
  helpers.
- `src/stream.rs`, `src/summary.rs`, `src/session.rs`, and `src/memory.rs`
  define the common runtime data model shared across providers.
- Provider adapters live under `src/claude`, `src/codex`, `src/gemini`,
  `src/kimi`, `src/openrouter`, and `src/qwen_code`.
- `src/sandbox.rs`, `src/sandbox_workspace.rs`, and `src/sandbox_glibc.rs`
  provide optional runsc-oriented sandbox helpers used by some CLI adapters.

## Request flow

1. Callers construct a `ProviderConfig` and optional `ProviderSession` or
   `MemoryContext`.
2. `call_provider_stream` dispatches to the provider-specific adapter.
3. The adapter runs the target CLI or HTTP request and emits normalized
   `StreamMessage` values through the caller-provided channel.
4. Provider-specific token usage and session identifiers are folded back into
   shared summary and session types before returning to the caller.

## Release contract

- `README.md` documents the public API and supported providers.
- `RUST_CHATBOT_INTEGRATION.md` describes the current host integration boundary without relying on local filesystem paths.
- `LICENSE` defines the repository license for source distribution.
- `.github/workflows/ci.yml` runs formatting, build, and test checks against
  the pinned Rust toolchain in `rust-toolchain.toml`.
