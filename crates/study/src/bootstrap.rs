/// Deterministic 64-bit generator. A dependency would do, but reproducing a
/// confidence interval on another machine should not depend on a crate's
/// version picking a different stream.
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Percentile 95% interval for the mean of `data`, resampling whole elements
/// with replacement. Elements are trading days, so within-day dependence is
/// kept intact.
pub fn paired_bootstrap(data: &[f64], resamples: usize, seed: u64) -> (f64, f64) {
    if data.is_empty() || resamples == 0 {
        return (f64::NAN, f64::NAN);
    }
    let mut rng = SplitMix64::new(seed);
    let mut means = Vec::with_capacity(resamples);
    for _ in 0..resamples {
        let mut sum = 0.0;
        for _ in 0..data.len() {
            sum += data[rng.below(data.len())];
        }
        means.push(sum / data.len() as f64);
    }
    means.sort_by(|a, b| a.partial_cmp(b).expect("no NaNs in resampled means"));
    let lo = means[(resamples as f64 * 0.025) as usize];
    let hi = means[((resamples as f64 * 0.975) as usize).min(resamples - 1)];
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_constant_sample_has_a_degenerate_interval() {
        let (lo, hi) = paired_bootstrap(&[1.0, 1.0, 1.0, 1.0], 1_000, 7);
        assert!((lo - 1.0).abs() < 1e-12);
        assert!((hi - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_sample_centred_on_zero_has_an_interval_containing_zero() {
        let data: Vec<f64> = (-50..=50).map(|i| i as f64).collect();
        let (lo, hi) = paired_bootstrap(&data, 2_000, 7);
        assert!(lo < 0.0 && hi > 0.0);
    }

    #[test]
    fn a_shifted_sample_has_an_interval_that_excludes_zero() {
        let data: Vec<f64> = (0..100).map(|i| 10.0 + (i % 5) as f64 * 0.1).collect();
        let (lo, hi) = paired_bootstrap(&data, 2_000, 7);
        assert!(lo > 0.0 && hi > lo);
    }

    #[test]
    fn the_same_seed_gives_the_same_interval() {
        let data: Vec<f64> = (0..40).map(|i| (i as f64).sin()).collect();
        assert_eq!(
            paired_bootstrap(&data, 500, 42),
            paired_bootstrap(&data, 500, 42)
        );
    }

    #[test]
    fn an_empty_sample_is_not_an_interval() {
        let (lo, hi) = paired_bootstrap(&[], 100, 1);
        assert!(lo.is_nan() && hi.is_nan());
    }
}
