pub mod api;
pub mod claude;
pub mod codex;
pub mod config;
pub mod gemini;
pub mod kimi;
mod live_summary_formatter;
pub mod memory;
pub mod openrouter;
pub mod qwen_code;
pub mod sandbox;
pub mod session;
pub mod stream;
pub mod summary;
pub mod workspace;

pub use api::{build_provider_command_string, call_provider_stream, ProviderConfig, ProviderKind};

#[cfg(test)]
mod cap_tests {
    use cap::Cap;
    use std::alloc;

    const TEST_ALLOCATION_LIMIT_BYTES: usize = 64 * 1024 * 1024;

    #[global_allocator]
    static TEST_ALLOCATOR: Cap<alloc::System> =
        Cap::new(alloc::System, TEST_ALLOCATION_LIMIT_BYTES);

    #[test]
    fn small_allocations_fit_under_test_cap() {
        let bytes: Vec<u8> = std::iter::repeat(0_u8).take(16 * 1024).collect();
        assert_eq!(bytes.len(), 16 * 1024);
    }
}
