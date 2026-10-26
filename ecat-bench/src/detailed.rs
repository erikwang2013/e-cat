// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 细粒度延迟统计：在原有 p50/p95/p99 之外记录 p99.9、最大值、错误数与
//! 超过阈值的请求数。用于验证“最大值 < 10ms”这类尾部 SLO。
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 一次压测的完整统计；错误请求也计入延迟样本，并单独计数。
#[derive(Debug, Clone)]
pub struct DetailedBenchReport {
    pub name: String,
    pub total_requests: u64,
    pub total_duration: Duration,
    pub avg_latency_us: f64,
    pub p50_latency_us: f64,
    pub p95_latency_us: f64,
    pub p99_latency_us: f64,
    pub p999_latency_us: f64,
    pub max_latency_us: f64,
    pub throughput_rps: f64,
    pub errors: u64,
    pub over_threshold: u64,
    pub threshold_us: f64,
}

impl DetailedBenchReport {
    pub fn print(&self) {
        println!("=== {} ===", self.name);
        println!("  requests:   {}", self.total_requests);
        println!("  errors:     {}", self.errors);
        println!("  duration:   {:.2?}", self.total_duration);
        println!("  throughput: {:.0} req/s", self.throughput_rps);
        println!("  avg:        {:.0} µs", self.avg_latency_us);
        println!("  p50:        {:.0} µs", self.p50_latency_us);
        println!("  p95:        {:.0} µs", self.p95_latency_us);
        println!("  p99:        {:.0} µs", self.p99_latency_us);
        println!("  p99.9:      {:.0} µs", self.p999_latency_us);
        println!("  max:        {:.0} µs", self.max_latency_us);
        println!("  >= {:.0} µs:  {}", self.threshold_us, self.over_threshold);
    }

    /// 转成旧版摘要，便于复用已有打印/对比代码。
    pub fn summary(&self) -> super::BenchResult {
        super::BenchResult {
            name: self.name.clone(),
            total_requests: self.total_requests,
            total_duration: self.total_duration,
            avg_latency_us: self.avg_latency_us,
            p50_latency_us: self.p50_latency_us,
            p95_latency_us: self.p95_latency_us,
            p99_latency_us: self.p99_latency_us,
            throughput_rps: self.throughput_rps,
        }
    }
}

/// 与 [`super::run_bench_with_warmup`] 相同的并发/预热语义，但闭包返回
/// `Result`，错误请求计入 `errors`；同时报告 p99.9、最大值和阈值越界数。
pub async fn run_bench_detailed<F, Fut>(
    name: &str,
    concurrency: usize,
    total: u64,
    warmup: u64,
    threshold: Duration,
    f: F,
) -> DetailedBenchReport
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<(), String>> + Send,
{
    if concurrency == 0 || total == 0 {
        return DetailedBenchReport {
            name: name.to_string(),
            total_requests: 0,
            total_duration: Duration::ZERO,
            avg_latency_us: 0.0,
            p50_latency_us: 0.0,
            p95_latency_us: 0.0,
            p99_latency_us: 0.0,
            p999_latency_us: 0.0,
            max_latency_us: 0.0,
            throughput_rps: 0.0,
            errors: 0,
            over_threshold: 0,
            threshold_us: threshold.as_micros() as f64,
        };
    }

    let chunk_size = total / concurrency as u64;
    let remainder = total % concurrency as u64;
    let warmup_chunk = warmup / concurrency as u64;
    let warmup_remainder = warmup % concurrency as u64;
    let f = Arc::new(f);
    let barrier = Arc::new(tokio::sync::Barrier::new(concurrency + 1));
    let mut handles = Vec::with_capacity(concurrency);

    for index in 0..concurrency {
        let f = Arc::clone(&f);
        let barrier = Arc::clone(&barrier);
        let n = chunk_size + u64::from((index as u64) < remainder);
        let planned = n;
        let wn = warmup_chunk + u64::from((index as u64) < warmup_remainder);
        let handle = tokio::spawn(async move {
            for _ in 0..wn {
                let _ = f().await;
            }
            barrier.wait().await;
            let mut latencies = Vec::with_capacity(n as usize);
            let mut errors = 0_u64;
            for _ in 0..n {
                let started = Instant::now();
                if f().await.is_err() {
                    errors += 1;
                }
                latencies.push(started.elapsed().as_micros() as f64);
            }
            (latencies, errors)
        });
        handles.push((handle, planned));
    }

    // worker 若在预热阶段 panic，屏障永远不会释放——超时失败而不是挂死。
    let _ = tokio::time::timeout(Duration::from_secs(30), barrier.wait())
        .await
        .unwrap_or_else(|_| panic!("bench workers failed to reach steady state (warmup panic?)"));
    let started = Instant::now();

    let mut latencies = Vec::with_capacity(total as usize);
    let mut errors = 0_u64;
    for (handle, planned) in handles {
        match handle.await {
            Ok((worker_latencies, worker_errors)) => {
                latencies.extend(worker_latencies);
                errors += worker_errors;
            }
            // panic 的 worker 没有样本；按整块计划量记错误，避免报告“零错误”。
            Err(_) => errors += planned,
        }
    }
    let total_duration = started.elapsed();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let count = latencies.len();
    let threshold_us = threshold.as_micros() as f64;
    let avg = if count == 0 {
        0.0
    } else {
        latencies.iter().sum::<f64>() / count as f64
    };
    let pct = |p: f64| percentile(&latencies, p);
    DetailedBenchReport {
        name: name.to_string(),
        total_requests: count as u64,
        total_duration,
        avg_latency_us: avg,
        p50_latency_us: pct(0.50),
        p95_latency_us: pct(0.95),
        p99_latency_us: pct(0.99),
        p999_latency_us: pct(0.999),
        max_latency_us: latencies.last().copied().unwrap_or(0.0),
        throughput_rps: if total_duration.is_zero() {
            0.0
        } else {
            count as f64 / total_duration.as_secs_f64()
        },
        errors,
        over_threshold: latencies
            .iter()
            .filter(|value| **value >= threshold_us)
            .count() as u64,
        threshold_us,
    }
}

/// 最近秩百分位：p=0.999、n=10000 时取第 9990 个样本（1-based）。
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil() as usize;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[tokio::test]
    async fn counts_errors_and_latencies() {
        let calls = Arc::new(AtomicU64::new(0));
        let f = {
            let calls = Arc::clone(&calls);
            move || {
                let calls = Arc::clone(&calls);
                async move {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    if n.is_multiple_of(3) {
                        Err("boom".to_string())
                    } else {
                        Ok(())
                    }
                }
            }
        };
        let report = run_bench_detailed("errors", 2, 30, 0, Duration::from_millis(10), f).await;
        assert_eq!(report.total_requests, 30);
        assert_eq!(report.errors, 10);
        assert_eq!(report.over_threshold, 0);
    }

    #[tokio::test]
    async fn max_and_p999_cover_tail() {
        let calls = Arc::new(AtomicU64::new(0));
        let f = {
            let calls = Arc::clone(&calls);
            move || {
                let calls = Arc::clone(&calls);
                async move {
                    if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Ok::<(), String>(())
                }
            }
        };
        let report = run_bench_detailed("tail", 1, 20, 0, Duration::from_millis(1), f).await;
        assert!(
            report.max_latency_us >= 4_000.0,
            "max must capture the 5ms sample: {}",
            report.max_latency_us
        );
        assert!(
            report.p999_latency_us <= report.max_latency_us,
            "p99.9 must not exceed max"
        );
        assert!(report.over_threshold >= 1);
    }

    #[test]
    fn percentile_uses_nearest_rank() {
        let values: Vec<f64> = (1..=1000).map(f64::from).collect();
        assert_eq!(percentile(&values, 0.50), 500.0);
        assert_eq!(percentile(&values, 0.99), 990.0);
        assert_eq!(percentile(&values, 0.999), 999.0);
        assert_eq!(percentile(&[], 0.999), 0.0);
    }

    #[test]
    fn p999_equals_max_below_1000_samples() {
        for n in 1..1000 {
            let values: Vec<f64> = (1..=n).map(f64::from).collect();
            assert_eq!(
                percentile(&values, 0.999),
                n as f64,
                "p99.9 must be the last sample for n={n}"
            );
        }
    }

    #[tokio::test]
    async fn zero_total_returns_empty_report() {
        let report = run_bench_detailed("zero", 4, 0, 10, Duration::from_millis(10), || async {
            Ok(())
        })
        .await;
        assert_eq!(report.total_requests, 0);
        assert_eq!(report.errors, 0);
        assert_eq!(report.max_latency_us, 0.0);
    }
}
