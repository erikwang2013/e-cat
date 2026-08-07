// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BenchResult {
    pub name: String,
    pub total_requests: u64,
    pub total_duration: Duration,
    pub avg_latency_us: f64,
    pub p50_latency_us: f64,
    pub p99_latency_us: f64,
    pub throughput_rps: f64,
}

impl BenchResult {
    pub fn print(&self) {
        println!("=== {} ===", self.name);
        println!("  requests:   {}", self.total_requests);
        println!("  duration:   {:.2?}", self.total_duration);
        println!("  throughput: {:.0} req/s", self.throughput_rps);
        println!("  avg:        {:.0} µs", self.avg_latency_us);
        println!("  p50:        {:.0} µs", self.p50_latency_us);
        println!("  p99:        {:.0} µs", self.p99_latency_us);
    }
}

pub async fn run_bench<F, Fut>(name: &str, concurrency: usize, total: u64, f: F) -> BenchResult
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send,
{
    if concurrency == 0 || total == 0 {
        return BenchResult {
            name: name.to_string(),
            total_requests: 0,
            total_duration: Duration::ZERO,
            avg_latency_us: 0.0,
            p50_latency_us: 0.0,
            p99_latency_us: 0.0,
            throughput_rps: 0.0,
        };
    }
    let start = Instant::now();
    let mut latencies = Vec::with_capacity(total as usize);
    let chunk_size = total / concurrency as u64;
    let remainder = total % concurrency as u64;
    let mut handles = Vec::with_capacity(concurrency);
    let shared_f = std::sync::Arc::new(f);

    for i in 0..concurrency {
        let f = std::sync::Arc::clone(&shared_f);
        // Spread the remainder so no requests are dropped.
        let n = chunk_size + u64::from((i as u64) < remainder);
        handles.push(tokio::spawn(async move {
            let mut lats = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let t0 = Instant::now();
                f().await;
                lats.push(t0.elapsed().as_micros() as f64);
            }
            lats
        }));
    }

    for handle in handles {
        if let Ok(lats) = handle.await {
            latencies.extend(lats);
        }
    }

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let total_duration = start.elapsed();
    let count = latencies.len();
    let avg = if count > 0 {
        latencies.iter().sum::<f64>() / count as f64
    } else {
        0.0
    };
    let p50 = if count > 0 { latencies[count / 2] } else { 0.0 };
    let p99 = if count > 0 {
        latencies[(count as f64 * 0.99) as usize]
    } else {
        0.0
    };

    BenchResult {
        name: name.to_string(),
        total_requests: count as u64,
        total_duration,
        avg_latency_us: avg,
        p50_latency_us: p50,
        p99_latency_us: p99,
        throughput_rps: count as f64 / total_duration.as_secs_f64(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bench_simple() {
        let result = run_bench("noop", 2, 20, || async {}).await;
        assert_eq!(result.total_requests, 20);
        assert!(result.throughput_rps > 0.0);
    }

    #[tokio::test]
    async fn bench_even_split() {
        let result = run_bench("even", 5, 20, || async {}).await;
        assert_eq!(result.total_requests, 20);
    }

    #[tokio::test]
    async fn bench_distributes_remainder() {
        let result = run_bench("rem", 3, 10, || async {}).await;
        assert_eq!(result.total_requests, 10);
        assert!(result.p50_latency_us >= 0.0);
        assert!(result.p99_latency_us >= 0.0);
    }

    #[tokio::test]
    async fn bench_concurrency_greater_than_total() {
        let result = run_bench("over", 10, 3, || async {}).await;
        assert_eq!(result.total_requests, 3);
        assert!(result.p50_latency_us >= 0.0);
    }

    #[tokio::test]
    async fn bench_zero_total() {
        let result = run_bench("zero", 4, 0, || async {}).await;
        assert_eq!(result.total_requests, 0);
        assert_eq!(result.p50_latency_us, 0.0);
        assert_eq!(result.p99_latency_us, 0.0);
    }

    #[tokio::test]
    async fn bench_zero_concurrency() {
        let result = run_bench("noconc", 0, 10, || async {}).await;
        assert_eq!(result.total_requests, 0);
    }

    #[test]
    fn bench_result_print() {
        let r = BenchResult {
            name: "test".into(),
            total_requests: 100,
            total_duration: Duration::from_secs(1),
            avg_latency_us: 500.0,
            p50_latency_us: 450.0,
            p99_latency_us: 900.0,
            throughput_rps: 100.0,
        };
        r.print();
    }
}
