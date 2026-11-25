//! A tiny deterministic-on-demand PRNG.
//!
//! Jitter is the only thing we need randomness for, so pulling in `rand` would
//! be the heaviest dependency in the crate for the least benefit.

use std::time::{SystemTime, UNIX_EPOCH};

pub struct Rng(u64);

impl Rng {
    /// Seeded from the clock. Good enough for jitter, not for crypto.
    pub fn from_entropy() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545_F491_4F6C_DD1D);
        Self::seeded(nanos ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn seeded(seed: u64) -> Self {
        Self(if seed == 0 { 1 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in `[-1, 1)`.
    pub fn signed_unit(&mut self) -> f64 {
        self.unit() * 2.0 - 1.0
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    #[test]
    fn unit_stays_in_range() {
        let mut rng = Rng::seeded(42);
        for _ in 0..10_000 {
            let v = rng.unit();
            assert!((0.0..1.0).contains(&v), "{v}");
            let s = rng.signed_unit();
            assert!((-1.0..1.0).contains(&s), "{s}");
        }
    }

    #[test]
    fn same_seed_same_sequence() {
        let a: Vec<f64> = (0..5).map(|_| Rng::seeded(7).unit()).collect();
        assert!(
            a.windows(2).all(|w| w[0] == w[1]),
            "a fresh seed must replay"
        );
        let mut r1 = Rng::seeded(7);
        let mut r2 = Rng::seeded(7);
        for _ in 0..100 {
            assert_eq!(r1.next_u64(), r2.next_u64());
        }
    }

    #[test]
    fn zero_seed_does_not_lock_up() {
        let mut rng = Rng::seeded(0);
        assert_ne!(rng.next_u64(), 0);
    }
}
