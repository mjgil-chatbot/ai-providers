use crate::live_summary_formatter::LiveSummaryFormatter;
use rust_debug::tracing::info;
use serde_json::Value;
const LIVE_SUMMARY_FORMATTER: LiveSummaryFormatter = LiveSummaryFormatter::new("Agent");
pub fn compute_cost(
    model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    let normalized = model.unwrap_or_default().to_ascii_lowercase();
    let (base_input_rate, output_rate, cache_read_rate, cache_write_rate) =
        if normalized.contains("minimax-m2.7-highspeed") {
            (0.6, 2.4, 0.06, 0.375)
        } else if normalized.contains("minimax-m2.5-highspeed")
            || normalized.contains("minimax-m2.1-highspeed")
        {
            (0.6, 2.4, 0.03, 0.375)
        } else if normalized.contains("minimax-m2.7") {
            (0.3, 1.2, 0.06, 0.375)
        } else if normalized.contains("minimax-m2.5")
            || normalized.contains("minimax-m2.1")
            || normalized.contains("minimax-m2")
        {
            (0.3, 1.2, 0.03, 0.375)
        } else {
            let (base_input_rate, output_rate) = if normalized.contains("opus") {
                (15.0, 75.0)
            } else if normalized.contains("haiku") {
                (0.80, 4.0)
            } else {
                (3.0, 15.0)
            };
            (
                base_input_rate,
                output_rate,
                base_input_rate * 0.10,
                base_input_rate * 1.25,
            )
        };

    let million = 1_000_000.0;
    (input_tokens as f64 / million) * base_input_rate
        + (cache_creation_tokens as f64 / million) * cache_write_rate
        + (cache_read_tokens as f64 / million) * cache_read_rate
        + (output_tokens as f64 / million) * output_rate
}
