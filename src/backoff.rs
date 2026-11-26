use rand::Rng;
use std::time::Duration;

#[derive(Debug)]
pub struct Backoff {
    base_ms: u64,
    max_ms: u64,
    factor: f64,
    current_ms: u64,
}

impl Backoff {
    pub fn new(base_ms: u64, max_ms: u64) -> Self {
        Self {
            base_ms,
            max_ms,
            factor: 2.0,
            current_ms: base_ms,
        }
    }

    pub fn reset(&mut self) {
        self.current_ms = self.base_ms;
    }

    pub fn next_delay(&mut self) -> Duration {
        let jitter = rand::thread_rng().gen_range(0.8..1.2);
        let ms = std::cmp::min(((self.current_ms as f64) * jitter) as u64, self.max_ms);
        self.current_ms =
            std::cmp::min(((self.current_ms as f64) * self.factor) as u64, self.max_ms);
        tracing::debug!("backoff delay: {}ms", ms);
        Duration::from_millis(ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_grows_to_cap() {
        let mut b = Backoff::new(1000, 60_000);
        let mut delays = Vec::new();
        for _ in 0..10 {
            delays.push(b.next_delay().as_millis() as u64);
        }
        assert!(
            delays[0] <= 2000,
            "first delay jittered from 1s: {:?}",
            delays[0]
        );
        for d in &delays {
            assert!(*d <= 60_000 + 200, "delay exceeded cap: {d}");
        }
        let last = *delays.last().unwrap();
        // Jitter floor is 0.8x cap, so 48_000 is the deterministic minimum.
        assert!(last >= 48_000, "expected near-cap delay, got {last}");
    }

    #[test]
    fn reset_returns_to_base() {
        let mut b = Backoff::new(1000, 60_000);
        for _ in 0..8 {
            b.next_delay();
        }
        b.reset();
        let d = b.next_delay().as_millis();
        assert!(d <= 2000, "after reset delay should be ~1s, got {d}");
    }
}
