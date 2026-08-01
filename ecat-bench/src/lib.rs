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
    let start = Instant::now();
    let mut latencies = Vec::with_capacity(total as usize);
    let chunk_size = total / concurrency as u64;
    let mut handles = Vec::with_capacity(concurrency);
    let shared_f = std::sync::Arc::new(f);

    for _ in 0..concurrency {
        let f = std::sync::Arc::clone(&shared_f);
        handles.push(tokio::spawn(async move {
            let mut lats = Vec::with_capacity(chunk_size as usize);
            for _ in 0..chunk_size {
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
    let avg = latencies.iter().sum::<f64>() / latencies.len() as f64;
    let p50 = latencies[latencies.len() / 2];
    let p99 = latencies[(latencies.len() as f64 * 0.99) as usize];

    BenchResult {
        name: name.to_string(),
        total_requests: latencies.len() as u64,
        total_duration,
        avg_latency_us: avg,
        p50_latency_us: p50,
        p99_latency_us: p99,
        throughput_rps: latencies.len() as f64 / total_duration.as_secs_f64(),
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
