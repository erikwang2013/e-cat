// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::future::Future;
use std::time::Duration;
use tokio::task::JoinHandle;

/// Scheduler for periodic and one-shot tasks.
pub struct Scheduler {
    handles: Vec<JoinHandle<()>>,
}

impl Scheduler {
    /// Create an empty scheduler.
    pub fn new() -> Self {
        Self {
            handles: Vec::new(),
        }
    }

    /// Run `job` every `interval`. The first run happens after one
    /// `interval` — the immediate first tick is skipped.
    pub fn every<F, Fut>(&mut self, interval: Duration, job: F)
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Capture the start instant here, before spawning: inside the task
        // `Instant::now()` would be evaluated at first poll, which makes
        // the schedule drift under paused clocks.
        let start = tokio::time::Instant::now();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval_at(start + interval, interval);
            loop {
                ticker.tick().await;
                job().await;
            }
        });
        self.handles.push(handle);
    }

    /// Run `job` once after `delay`.
    pub fn once<F, Fut>(&mut self, delay: Duration, job: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        // Same reasoning as `every`: create the sleep outside the task.
        let sleep = tokio::time::sleep(delay);
        let handle = tokio::spawn(async move {
            sleep.await;
            job().await;
        });
        self.handles.push(handle);
    }

    /// Wait for all scheduled tasks to finish. With `every` jobs this
    /// never returns — use `shutdown` to stop.
    pub async fn run(self) {
        for handle in self.handles {
            let _ = handle.await;
        }
    }

    /// Abort all scheduled tasks.
    pub fn shutdown(self) {
        for handle in self.handles {
            handle.abort();
        }
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // The paused-clock test only asserts the deterministic property: no work
    // runs before the first interval elapses. Task scheduling under a paused
    // clock is unreliable (tokio fires timers lazily at first poll), so the
    // periodic/one-shot behavior is covered by the real-time tests below.
    #[tokio::test]
    async fn every_skips_first_tick() {
        tokio::time::pause();
        tokio::time::advance(Duration::from_millis(1)).await;
        let count = Arc::new(AtomicUsize::new(0));
        let mut sched = Scheduler::new();
        let c = Arc::clone(&count);
        sched.every(Duration::from_millis(100), move || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::advance(Duration::from_millis(99)).await;
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
        sched.shutdown();
    }

    #[tokio::test]
    async fn every_runs_periodically() {
        let count = Arc::new(AtomicUsize::new(0));
        let mut sched = Scheduler::new();
        let c = Arc::clone(&count);
        sched.every(Duration::from_millis(20), move || {
            let c = Arc::clone(&c);
            async move {
                c.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(110)).await;
        sched.shutdown();
        // ~5 ticks expected on a 20ms interval over 110ms; only the lower
        // bound matters so a loaded CI machine cannot flake the test.
        let n = count.load(Ordering::SeqCst);
        assert!(n >= 3, "expected several ticks, got {n}");
    }

    #[tokio::test]
    async fn once_fires_exactly_once() {
        let fired = Arc::new(AtomicUsize::new(0));
        let mut sched = Scheduler::new();
        let f = Arc::clone(&fired);
        sched.once(Duration::from_millis(30), move || {
            let f = Arc::clone(&f);
            async move {
                f.fetch_add(1, Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        sched.shutdown();
        assert_eq!(fired.load(Ordering::SeqCst), 1);
    }
}
