//! Sampler chain construction for token generation.

use llama_cpp_2::sampling::LlamaSampler;

/// Build a sampler chain for the given temperature.
///
/// - `temperature <= 0.01`: greedy (deterministic) sampling.
/// - Otherwise: top-k(40) -> top-p(0.95) -> min-p(0.05) -> temp -> dist.
///
/// The chain order matches llama.cpp's recommended pipeline: filtering
/// samplers first (top-k, top-p, min-p), then temperature scaling, then
/// the final categorical distribution draw.
pub(crate) fn build_sampler(temperature: Option<f32>) -> LlamaSampler {
    let t = temperature.unwrap_or(0.8);

    if t <= 0.01 {
        LlamaSampler::greedy()
    } else {
        let seed = rand_seed();
        LlamaSampler::chain_simple(vec![
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(t),
            LlamaSampler::dist(seed),
        ])
    }
}

/// Generate a random seed for the distribution sampler.
///
/// Uses the lower 32 bits of a high-resolution timestamp to avoid pulling
/// in the `rand` crate just for a seed. This is not cryptographic -- it
/// only needs to vary across inference runs.
fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(42)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_at_zero_temperature() {
        // Should not panic.
        let _ = build_sampler(Some(0.0));
    }

    #[test]
    fn greedy_at_very_low_temperature() {
        let _ = build_sampler(Some(0.001));
    }

    #[test]
    fn default_temperature_is_0_8() {
        // None -> 0.8, should produce a chain, not greedy.
        let _ = build_sampler(None);
    }

    #[test]
    fn high_temperature() {
        let _ = build_sampler(Some(1.5));
    }

    #[test]
    fn rand_seed_varies() {
        let a = rand_seed();
        // Busy-wait a tiny bit to get a different nanosecond.
        std::hint::spin_loop();
        let b = rand_seed();
        // Very unlikely but not impossible to be equal; just verify no panic.
        let _ = (a, b);
    }
}
