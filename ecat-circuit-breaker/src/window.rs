// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::time::{Duration, Instant};

pub(crate) struct SlidingWindow {
    successes: u64,
    failures: u64,
    window_start: Instant,
    window: Duration,
}

impl SlidingWindow {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            successes: 0,
            failures: 0,
            window_start: Instant::now(),
            window,
        }
    }

    pub(crate) fn record(&mut self, success: bool) {
        self.rotate();
        if success {
            self.successes += 1;
        } else {
            self.failures += 1;
        }
    }

    pub(crate) fn total(&mut self) -> u64 {
        self.rotate();
        self.successes + self.failures
    }

    pub(crate) fn failure_ratio(&mut self) -> f64 {
        let total = self.total();
        if total == 0 {
            return 0.0;
        }
        self.failures as f64 / total as f64
    }

    fn rotate(&mut self) {
        if self.window_start.elapsed() >= self.window {
            self.successes = 0;
            self.failures = 0;
            self.window_start = Instant::now();
        }
    }

    /// 清空窗口计数并重置窗口起点。
    pub(crate) fn clear(&mut self) {
        self.successes = 0;
        self.failures = 0;
        self.window_start = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_window_counts() {
        let mut w = SlidingWindow::new(Duration::from_secs(60));
        assert_eq!(w.total(), 0);
        w.record(true);
        w.record(true);
        w.record(false);
        assert_eq!(w.total(), 3);
        assert!((w.failure_ratio() - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn sliding_window_clear_resets_counters() {
        let mut w = SlidingWindow::new(Duration::from_secs(60));
        w.record(false);
        w.record(false);
        assert_eq!(w.failure_ratio(), 1.0);
        w.clear();
        assert_eq!(w.total(), 0);
        assert_eq!(w.failure_ratio(), 0.0);
    }
}
