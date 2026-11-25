//! Pacing and backoff.
//!
//! The whole point of this crate is to be *slower* than a human could be
//! bothered to be, so the account never looks like a scraper:
//!   * a fixed base interval between requests, with random jitter,
//!   * a long pause after every batch,
//!   * exponential backoff with a hard ceiling when the server pushes back.

use std::time::Duration;

use crate::rng::Rng;

#[derive(Debug, Clone, Copy)]
pub struct PaceConfig {
    /// Base gap between two requests.
    pub interval: Duration,
    /// Fraction of `interval` to randomise by, in `[0, 1]`.
    pub jitter: f64,
    /// Requests per batch; `0` disables batch pauses.
    pub batch_size: u32,
    /// How long to rest between batches.
    pub batch_pause: Duration,
}

impl Default for PaceConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(4),
            jitter: 0.4,
            batch_size: 15,
            batch_pause: Duration::from_secs(30),
        }
    }
}

impl PaceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.interval < Duration::from_millis(500) {
            return Err("--interval below 0.5s is reckless; raise it".into());
        }
        if !(0.0..=1.0).contains(&self.jitter) {
            return Err("--jitter must be between 0 and 1".into());
        }
        Ok(())
    }

    /// Wall-clock estimate for `n` requests, ignoring server-side backoff.
    pub fn estimate(&self, n: u32) -> Duration {
        if n == 0 {
            return Duration::ZERO;
        }
        let gaps = u64::from(n - 1);
        let pauses = u64::from((n - 1).checked_div(self.batch_size).unwrap_or(0));
        self.interval * gaps as u32 + self.batch_pause * pauses as u32
    }
}

pub struct Pacer {
    cfg: PaceConfig,
    rng: Rng,
    sent: u32,
}

impl Pacer {
    pub fn new(cfg: PaceConfig) -> Self {
        Self {
            cfg,
            rng: Rng::from_entropy(),
            sent: 0,
        }
    }

    #[cfg(test)]
    pub fn seeded(cfg: PaceConfig, seed: u64) -> Self {
        Self {
            cfg,
            rng: Rng::seeded(seed),
            sent: 0,
        }
    }

    /// How long to sleep before request number `sent + 1`. The first request
    /// goes out immediately.
    pub fn next_delay(&mut self) -> Duration {
        let delay = if self.sent == 0 {
            Duration::ZERO
        } else if self.cfg.batch_size > 0 && self.sent % self.cfg.batch_size == 0 {
            self.jittered(self.cfg.batch_pause)
        } else {
            self.jittered(self.cfg.interval)
        };
        self.sent += 1;
        delay
    }

    fn jittered(&mut self, base: Duration) -> Duration {
        let factor = 1.0 + self.cfg.jitter * self.rng.signed_unit();
        base.mul_f64(factor.max(0.1))
    }
}

/// Exponential backoff for retryable server responses.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    base: Duration,
    max: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(base: Duration, max: Duration) -> Self {
        Self {
            base,
            max,
            attempt: 0,
        }
    }

    /// Next wait, doubling each time, capped at `max`.
    pub fn next(&mut self) -> Duration {
        let factor = 1u32 << self.attempt.min(10);
        self.attempt = self.attempt.saturating_add(1);
        (self.base * factor).min(self.max)
    }

    pub fn attempts(&self) -> u32 {
        self.attempt
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_secs(60), Duration::from_secs(900))
    }
}

/// Render a duration the way a human reads a progress line.
pub fn human(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0 => format!("{}ms", d.subsec_millis()),
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {}s", s / 60, s % 60),
        s => format!("{}h {}m", s / 3600, (s % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PaceConfig {
        PaceConfig {
            interval: Duration::from_secs(4),
            jitter: 0.5,
            batch_size: 3,
            batch_pause: Duration::from_secs(30),
        }
    }

    #[test]
    fn first_request_is_immediate() {
        let mut p = Pacer::seeded(cfg(), 1);
        assert_eq!(p.next_delay(), Duration::ZERO);
    }

    #[test]
    fn delays_stay_inside_the_jitter_band() {
        let mut p = Pacer::seeded(cfg(), 99);
        p.next_delay();
        for _ in 0..200 {
            let d = p.next_delay();
            let within_interval = d >= Duration::from_secs(2) && d <= Duration::from_secs(6);
            let within_pause = d >= Duration::from_secs(15) && d <= Duration::from_secs(45);
            assert!(
                within_interval || within_pause,
                "delay {d:?} outside both bands"
            );
        }
    }

    #[test]
    fn a_long_pause_lands_on_every_batch_boundary() {
        let mut p = Pacer::seeded(cfg(), 7);
        let delays: Vec<Duration> = (0..7).map(|_| p.next_delay()).collect();
        // batch_size 3 -> requests 4 and 7 are preceded by the long pause.
        assert!(delays[3] > Duration::from_secs(10), "{:?}", delays[3]);
        assert!(delays[6] > Duration::from_secs(10), "{:?}", delays[6]);
        assert!(delays[1] < Duration::from_secs(10), "{:?}", delays[1]);
    }

    #[test]
    fn zero_batch_size_disables_pauses() {
        let mut p = Pacer::seeded(
            PaceConfig {
                batch_size: 0,
                ..cfg()
            },
            3,
        );
        for _ in 0..50 {
            assert!(p.next_delay() < Duration::from_secs(10));
        }
    }

    #[test]
    fn backoff_doubles_then_saturates() {
        let mut b = Backoff::new(Duration::from_secs(60), Duration::from_secs(900));
        assert_eq!(b.next(), Duration::from_secs(60));
        assert_eq!(b.next(), Duration::from_secs(120));
        assert_eq!(b.next(), Duration::from_secs(240));
        assert_eq!(b.next(), Duration::from_secs(480));
        assert_eq!(b.next(), Duration::from_secs(900));
        assert_eq!(b.next(), Duration::from_secs(900), "must stay capped");
        // A fresh Backoff is how the runner resets: one per company.
        let mut fresh = Backoff::new(Duration::from_secs(60), Duration::from_secs(900));
        assert_eq!(fresh.next(), Duration::from_secs(60));
    }

    #[test]
    fn backoff_never_overflows() {
        let mut b = Backoff::default();
        for _ in 0..1_000 {
            let _ = b.next();
        }
        assert_eq!(b.next(), Duration::from_secs(900));
    }

    #[test]
    fn estimate_counts_gaps_not_requests() {
        let c = cfg();
        assert_eq!(c.estimate(0), Duration::ZERO);
        assert_eq!(c.estimate(1), Duration::ZERO);
        // 2 requests = 1 gap of 4s, no batch boundary yet.
        assert_eq!(c.estimate(2), Duration::from_secs(4));
        // 7 requests = 6 gaps + 2 batch pauses.
        assert_eq!(c.estimate(7), Duration::from_secs(6 * 4 + 2 * 30));
    }

    #[test]
    fn rejects_reckless_settings() {
        let fast = PaceConfig {
            interval: Duration::from_millis(100),
            ..PaceConfig::default()
        };
        assert!(fast.validate().is_err());
        let bad_jitter = PaceConfig {
            jitter: 2.0,
            ..PaceConfig::default()
        };
        assert!(bad_jitter.validate().is_err());
        assert!(PaceConfig::default().validate().is_ok());
    }

    #[test]
    fn human_is_readable() {
        assert_eq!(human(Duration::from_secs(45)), "45s");
        assert_eq!(human(Duration::from_secs(90)), "1m 30s");
        assert_eq!(human(Duration::from_secs(7_260)), "2h 1m");
    }
}
