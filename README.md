# ai-providers

`ai-providers` is a Rust library that wraps multiple AI provider CLIs and APIs
behind a single streaming interface.

The crate currently supports:

- Claude-compatible CLIs
- Codex-compatible CLIs
- Gemini CLI
- Kimi CLI
- OpenRouter HTTP and CLI adapters
- Qwen Code CLI

It intentionally owns:

- provider adapters and command construction
- provider-neutral stream handling and summaries
- lightweight session and memory-context types
- optional sandbox helpers for CLI execution

It intentionally does not own:

- UI state
- session persistence
- memory indexing and retrieval
- workflow orchestration outside a single provider request

## Public API

The main entry points are:

- `ProviderConfig` and `ProviderKind` in `src/api.rs`
- `build_provider_command_string(...)`
- `call_provider_stream(...)`

`call_provider_stream(...)` accepts a provider config, prompt inputs, optional
session state, and a channel sender for normalized `StreamMessage` values.

## Development

Rust tooling is pinned in `rust-toolchain.toml`, and formatting is pinned in
`rustfmt.toml`.

Local validation:

```bash
cargo fmt --check
cargo build --release --all-targets
cargo test --release --all-targets
```

Additional repo docs:

- `ARCHITECTURE.md` for crate structure and release expectations
- `RUST_CHATBOT_INTEGRATION.md` for host integration boundaries

## License

First-party code is available under [0BSD](LICENSE).

Third-party components retain their own terms.
