# Host integration

`ai-providers` supplies the lower-level streaming provider API. The sibling
`rust-chatbot` and `chatbot-worker` manifests already depend on this crate; adding
that dependency is not a remaining migration step.

Callers construct `ProviderConfig`, optional `ProviderSession` and
`MemoryContext`, then call `call_provider_stream` with a sender for normalized
`StreamMessage` values. Provider command construction is also available through
`build_provider_command_string`.

Keep UI state, persistence, and workspace memory indexing in the host. Populate
`ProviderMessage.history_content` when expanded history is required. The host's
richer session type must be mapped into `ProviderSession` rather than coupling
this crate back to UI/session persistence structures.

CLI-backed detached turns can instead use the higher-level worker/adapter
boundary described in the sibling repositories. This crate does not replace
that durable spool or orchestration layer.

See [ARCHITECTURE.md](ARCHITECTURE.md) for modules and [README.md](README.md)
for supported provider families and release-mode development commands.
