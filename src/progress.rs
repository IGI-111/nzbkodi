//! Pure progress math: a rolling speed window and percentage rounding.
//!
//! Time is passed in explicitly (milliseconds since an arbitrary epoch)
//! so every computation here is deterministic and unit-testable.

use std::collections::VecDeque;

/// Rolling download-speed estimator over a fixed time window.
///
/// The estimate is `bytes recorded in the last window / window`, i.e.
/// smoothed by construction — a burst does not spike the reading.
#[derive(Debug)]
pub struct SpeedWindow {
    window_ms: u64,
    samples: VecDeque<(u64, u64)>,
}

impl SpeedWindow {
    #[must_use]
    pub fn new(window_ms: u64) -> Self {
        Self {
            window_ms: window_ms.max(1),
            samples: VecDeque::new(),
        }
    }

    /// Record `bytes` received at `t_ms` (monotonic, same clock for all
    /// calls).
    pub fn record_at(&mut self, t_ms: u64, bytes: u64) {
        self.samples.push_back((t_ms, bytes));
    }

    /// Current estimate in bytes per second, pruning expired samples.
    pub fn bps_at(&mut self, now_ms: u64) -> u64 {
        while let Some(&(t, _)) = self.samples.front() {
            if now_ms.saturating_sub(t) >= self.window_ms {
                self.samples.pop_front();
            } else {
                break;
            }
        }
        let bytes: u64 = self.samples.iter().map(|s| s.1).sum();
        bytes.saturating_mul(1000) / self.window_ms
    }
}

/// Percentage clamped to `0..=100`, rounded to one decimal.
///
/// A zero total reads as `0.0` — callers decide what "no data" means.
#[must_use]
pub fn percent(done: u64, total: u64) -> f64 {
    if total == 0 || done == 0 {
        return 0.0;
    }
    let p = (done as f64 / total as f64) * 100.0;
    (p.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_window_reads_zero() {
        let mut w = SpeedWindow::new(5_000);
        assert_eq!(w.bps_at(0), 0);
    }

    #[test]
    fn averages_over_the_window() {
        let mut w = SpeedWindow::new(5_000);
        w.record_at(1_000, 1_000);
        w.record_at(2_000, 3_000);
        // 4_000 bytes in a 5 s window -> 800 B/s.
        assert_eq!(w.bps_at(4_000), 800);
    }

    #[test]
    fn samples_expire_at_window_edge() {
        let mut w = SpeedWindow::new(5_000);
        w.record_at(0, 10_000);
        // Exactly at the edge (t == window): expired.
        assert_eq!(w.bps_at(5_000), 0);
        // Just inside: still counted.
        let mut w2 = SpeedWindow::new(5_000);
        w2.record_at(1, 10_000);
        assert_eq!(w2.bps_at(5_000), 2_000);
    }

    #[test]
    fn window_of_zero_is_defensive() {
        let mut w = SpeedWindow::new(0);
        w.record_at(0, 1_000);
        // Falls back to a 1 ms window.
        assert_eq!(w.bps_at(0), 1_000_000);
    }

    #[test]
    fn percent_basics() {
        assert_eq!(percent(0, 100), 0.0);
        assert_eq!(percent(0, 0), 0.0);
        assert_eq!(percent(50, 100), 50.0);
        assert_eq!(percent(100, 100), 100.0);
        assert_eq!(percent(150, 100), 100.0);
    }

    #[test]
    fn percent_rounds_to_one_decimal() {
        assert_eq!(percent(1, 3), 33.3);
        assert_eq!(percent(2, 3), 66.7);
    }
}
