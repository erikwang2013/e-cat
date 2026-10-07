// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::window::SlidingWindow;

/// 熔断器状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

impl BreakerState {
    /// 指标的数值编码：`0` = closed、`1` = open、`2` = half-open。
    ///
    /// `ecat_outbound_breaker_state` 的取值即此。**不要改这个映射** ——
    /// 告警规则与仪表盘会写死这三个数；将来加状态只能往后再追加数字。
    ///
    /// 放在本 crate 而不是指标侧：编码器与枚举定义放一起才不会各改各的。
    pub fn code(self) -> u8 {
        match self {
            BreakerState::Closed => 0,
            BreakerState::Open => 1,
            BreakerState::HalfOpen => 2,
        }
    }
}

/// 熔断阈值。字段与 `CircuitBreakerLayer` 的 builder 一一对应，
/// 默认值与 `CircuitBreakerLayer::new()` 相同（0.5 / 30s / 3 / 10s）。
///
/// 每个字段都有 `#[serde(default)]`：配置文件里写 `breaker: {}`
/// 与 `breaker: {"failure_ratio": 0.5}` 都必须能反序列化（前者是常见写法）。
#[derive(Debug, Clone, serde::Deserialize)]
pub struct BreakerConfig {
    #[serde(default = "default_failure_ratio")]
    pub failure_ratio: f64,
    #[serde(default = "default_window", with = "duration_secs")]
    pub window: Duration,
    #[serde(default = "default_half_open_probes")]
    pub half_open_probes: u32,
    #[serde(default = "default_open_duration", with = "duration_secs")]
    pub open_duration: Duration,
}

fn default_failure_ratio() -> f64 {
    0.5
}

fn default_window() -> Duration {
    Duration::from_secs(30)
}

fn default_half_open_probes() -> u32 {
    3
}

fn default_open_duration() -> Duration {
    Duration::from_secs(10)
}

/// `Duration` ↔ 秒的 serde 适配：配置里写 `window: 30`，
/// 而不是 serde 默认给的 `{secs, nanos}` 结构体。
mod duration_secs {
    use serde::{Deserialize, Deserializer};
    use std::time::Duration;

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_secs(u64::deserialize(d)?))
    }
}

impl Default for BreakerConfig {
    /// 走同一组 `default_*` 函数，而不是把四个数字再抄一遍 ——
    /// 抄两遍就会分叉：serde 侧省略字段拿到 30 秒、`Default` 侧拿到别的。
    fn default() -> Self {
        Self {
            failure_ratio: default_failure_ratio(),
            window: default_window(),
            half_open_probes: default_half_open_probes(),
            open_duration: default_open_duration(),
        }
    }
}

/// 分类回调：把"传输成功"的响应判定为业务失败（如 HTTP 5xx）。
/// 签名经 Any 泛化，配置时类型安全，调用时 downcast 不匹配则视为成功。
pub(crate) type Classify = Arc<dyn Fn(&dyn std::any::Any) -> bool + Send + Sync>;

/// 熔断状态机的内部状态（原 `BreakerInner`）。
pub(crate) struct Inner {
    pub(crate) state: BreakerState,
    pub(crate) window: SlidingWindow,
    pub(crate) opened_at: Option<Instant>,
    pub(crate) half_open_count: u32,
    /// `Closed → Open` 的累计次数。半开探测失败**重新打开也算一次**
    /// （那是「后端还是不行」的第二次确认，与首次打开同等重要）。
    pub(crate) opened_total: u64,
}

/// 熔断器：`Open` 时不调用下游，直接快速失败。
///
/// 不依赖 tower，供非 tower 场景（如 RDBMS 端点路由）逐端点持有。
pub struct Breaker {
    config: BreakerConfig,
    classify: Option<Classify>,
    inner: Mutex<Inner>,
}

impl Breaker {
    pub fn new(config: BreakerConfig) -> Self {
        let inner = Inner {
            state: BreakerState::Closed,
            window: SlidingWindow::new(config.window),
            opened_at: None,
            half_open_count: 0,
            opened_total: 0,
        };
        Self {
            config,
            classify: None,
            inner: Mutex::new(inner),
        }
    }

    pub(crate) fn with_classify(mut self, classify: Option<Classify>) -> Self {
        self.classify = classify;
        self
    }

    /// 执行一次受熔断保护的调用。
    ///
    /// - 熔断器 `Open` → **不调用 `f`**，直接返回 `Err(BreakerError::Open)`
    /// - 半开态 → 放行 `half_open_probes` 个探测
    /// - `Ok`/`Err` 按 `classify` 的判据记入滑动窗口
    pub async fn call<F, Fut, T, E>(&self, f: F) -> Result<T, BreakerError<E>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>> + Send,
        T: 'static,
        E: std::fmt::Display,
    {
        {
            let mut inner = self.lock();
            match inner.state {
                BreakerState::Open => {
                    if let Some(opened_at) = inner.opened_at {
                        if opened_at.elapsed() >= self.config.open_duration {
                            tracing::info!("circuit breaker: open → half-open");
                            inner.state = BreakerState::HalfOpen;
                            inner.half_open_count = 0;
                        } else {
                            return Err(BreakerError::Open);
                        }
                    }
                }
                BreakerState::HalfOpen => {
                    if inner.half_open_count >= self.config.half_open_probes {
                        return Err(BreakerError::ProbesExhausted);
                    }
                    inner.half_open_count += 1;
                }
                BreakerState::Closed => {}
            }
        }

        let result = f().await;
        let mut inner = self.lock();

        match &result {
            Ok(resp) => {
                // 配置 classify 回调后，业务失败响应（如 HTTP 5xx）
                // 也计入失败窗口；未配置时传输成功一律计成功（兼容）。
                let is_failure = self
                    .classify
                    .as_ref()
                    .is_some_and(|c| c(resp as &dyn std::any::Any));
                inner.window.record(!is_failure);
            }
            Err(e) => {
                tracing::warn!(error = %e, "circuit breaker: request failed");
                inner.window.record(false);
            }
        }

        match inner.state {
            BreakerState::Closed => {
                if inner.window.total() >= 5
                    && inner.window.failure_ratio() >= self.config.failure_ratio
                {
                    tracing::warn!(
                        ratio = inner.window.failure_ratio(),
                        "circuit breaker: closed → open"
                    );
                    inner.state = BreakerState::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.opened_total += 1;
                }
            }
            BreakerState::HalfOpen => {
                if result.is_ok() {
                    tracing::info!("circuit breaker: half-open → closed");
                    inner.state = BreakerState::Closed;
                    inner.opened_at = None;
                    // 清空窗口，否则旧的高失败率会立即再次触发 open。
                    inner.window.clear();
                } else {
                    tracing::warn!("circuit breaker: half-open → open (probe failed)");
                    inner.state = BreakerState::Open;
                    inner.opened_at = Some(Instant::now());
                    inner.opened_total += 1;
                }
            }
            BreakerState::Open => {}
        }

        result.map_err(BreakerError::Inner)
    }

    /// 读取**有效**状态，供选端点时判断是否跳过（如 `RdbmsRouting`）。
    ///
    /// **包含冷却期的 `Open` → `HalfOpen` 转换。** 这一点是**必需的，不是便利**：
    /// 调用方（如 `RdbmsRouting`）用它来**跳过** `Open` 的端点；若这里不转换，
    /// 被跳过的端点就永远没人调 `call` —— 而真正的状态迁移在 `call` 里 ——
    /// 于是冷却期过后也不会被重新放行，**任何失败过的端点被永久排除**，
    /// 读能力单调缩水。熔断器的意义正是「恢复后重新放行」。
    ///
    /// 本方法仍是**只读**的：它只**报告**冷却期已过，不改内部的 `state` 字段
    /// （真正的迁移仍由 `call` 做）。所以拿的是普通锁，不是可变语义。
    pub fn state(&self) -> BreakerState {
        let inner = self.lock();
        let cooled_down = inner
            .opened_at
            .is_some_and(|t| t.elapsed() >= self.config.open_duration);
        if inner.state == BreakerState::Open && cooled_down {
            BreakerState::HalfOpen
        } else {
            inner.state
        }
    }

    /// `Closed → Open` 的累计次数（半开探测失败重新打开也计入）。
    ///
    /// 供指标 `ecat_outbound_breaker_open_total`。用 `state()` 轮询猜测
    /// 「开了几次」是错的 —— 轮询间隔决定准确性，而且熔断器可能开又关，
    /// 两次探测之间发生的事抓不到。
    pub fn opened_total(&self) -> u64 {
        self.lock().opened_total
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BreakerError<E> {
    #[error("circuit breaker is open")]
    Open,
    #[error("circuit breaker: too many probes")]
    ProbesExhausted,
    #[error(transparent)]
    Inner(E),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 连续失败触发 `Open` 后，`state()` 必须报告 `Open` ——
    /// 这是 `RdbmsRouting` 跳过已熔断端点的唯一依据。
    #[tokio::test]
    async fn state_reports_open_after_repeated_failures() {
        let breaker = Breaker::new(BreakerConfig::default());
        assert_eq!(breaker.state(), BreakerState::Closed);

        for _ in 0..5 {
            let r: Result<(), BreakerError<std::io::Error>> = breaker
                .call(|| async { Err::<(), std::io::Error>(std::io::Error::other("fail")) })
                .await;
            assert!(matches!(r, Err(BreakerError::Inner(_))));
        }

        assert_eq!(breaker.state(), BreakerState::Open);
    }

    /// 连续打 5 次失败把熔断器打到 `Open`。
    async fn trip(breaker: &Breaker) {
        for _ in 0..5 {
            let r: Result<(), BreakerError<std::io::Error>> = breaker
                .call(|| async { Err::<(), std::io::Error>(std::io::Error::other("fail")) })
                .await;
            assert!(matches!(r, Err(BreakerError::Inner(_))));
        }
    }

    /// **冷却期过后 `state()` 必须报告 `HalfOpen`。**
    ///
    /// 这是「跳过 `Open` 端点」那类调用方（如 `RdbmsRouting`）能恢复的**唯一**途径：
    /// 真正的迁移在 `call` 里，而被跳过的端点没人调 `call` —— `state()` 不转换的话，
    /// **任何失败过的端点被永久排除**，读能力单调缩水。
    ///
    /// `open_duration = ZERO` 让冷却期立即过去 —— 测试**不依赖时间**，无 flake。
    #[tokio::test]
    async fn state_reports_half_open_once_the_cooldown_has_elapsed() {
        let breaker = Breaker::new(BreakerConfig {
            open_duration: Duration::ZERO,
            ..BreakerConfig::default()
        });
        trip(&breaker).await;
        assert_eq!(
            breaker.state(),
            BreakerState::HalfOpen,
            "冷却期已过 ⇒ 必须报告 HalfOpen，否则端点被永久排除"
        );
    }

    /// 反向对照：冷却期**未过**时仍报 `Open`。
    ///
    /// 没有这条，上一条可能被实现成「恒 `HalfOpen`」而照样通过。
    #[tokio::test]
    async fn state_stays_open_while_the_cooldown_is_running() {
        let breaker = Breaker::new(BreakerConfig {
            open_duration: Duration::from_secs(600),
            ..BreakerConfig::default()
        });
        trip(&breaker).await;
        assert_eq!(breaker.state(), BreakerState::Open);
    }

    /// `opened_total` 数的是**打开次数**，不是当前状态 ——
    /// 只断言「打开后 ≥1」是空验收（一个恒返回 1 的实现也能过）。
    /// 这里驱动两次独立的打开：首次按失败率、之后由半开探测失败重新打开。
    #[tokio::test]
    async fn opened_total_counts_each_open_not_the_state() {
        let cfg = BreakerConfig {
            open_duration: Duration::from_millis(20),
            ..BreakerConfig::default()
        };
        let b = Breaker::new(cfg);
        assert_eq!(b.opened_total(), 0, "初始不得已经计过数");

        let fail = || async { Err::<(), &str>("backend down") };
        // 窗口的样本下限是 5（`breaker.rs:134` 的 `window.total() >= 5`），
        // 恰好打满才触发打开。
        for _ in 0..5 {
            let _ = b.call(fail).await;
        }
        assert_eq!(b.state(), BreakerState::Open);
        assert_eq!(b.opened_total(), 1);

        // 冷却 → 半开 → 探测失败 → 重新打开，计到 2。
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = b.call(fail).await;
        assert_eq!(b.opened_total(), 2, "半开探测失败重新打开必须再计一次");
    }

    /// 三个状态的编码是**对外契约**（告警规则里写死了这些数），
    /// 逐个钉住而不是只钉一个 —— 只钉一个的话，改另一个的映射不会被发现。
    #[test]
    fn state_codes_are_the_metric_contract() {
        assert_eq!(BreakerState::Closed.code(), 0);
        assert_eq!(BreakerState::Open.code(), 1);
        assert_eq!(BreakerState::HalfOpen.code(), 2);
    }

    /// serde 默认值必须与 `Default` **逐字段相同** ——
    /// 配置文件省略字段与代码里 `BreakerConfig::default()` 是两条路径，
    /// 分叉了就是「同一份配置在两种构造方式下行为不同」。
    #[test]
    fn deserialized_defaults_match_default_impl() {
        let from_json: BreakerConfig = serde_json::from_str("{}").unwrap();
        let d = BreakerConfig::default();
        assert_eq!(from_json.failure_ratio, d.failure_ratio);
        assert_eq!(from_json.window, d.window);
        assert_eq!(from_json.half_open_probes, d.half_open_probes);
        assert_eq!(from_json.open_duration, d.open_duration);

        // 显式给值也要生效（只测 `{}` 的话，一个忽略输入的实现也能过）。
        let given: BreakerConfig =
            serde_json::from_str(r#"{"failure_ratio": 0.9, "window": 5}"#).unwrap();
        assert_eq!(given.failure_ratio, 0.9);
        assert_eq!(given.window, Duration::from_secs(5));
    }
}
