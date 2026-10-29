// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 共享 HTTP 出站外壳：TLS、认证、自定义头、超时、熔断、并发上限。

use crate::WorkflowError;
use ecat_circuit_breaker::{Breaker, BreakerConfig, BreakerError};
use ecat_tls::TlsClientConfig;
use reqwest::Method;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Semaphore, SemaphorePermit};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_CONCURRENCY: usize = 32;

/// HTTP 出站配置。
///
/// 适配器用 `#[serde(flatten)]` 把它嵌进自己的配置结构，因此这些字段名就是
/// 配置文件里的键；适配器自有字段**不得**与下列字段重名（重名会被这里吞掉）。
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowHttpConfig {
    pub base_url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// 设置后优先于 `username`/`password`（`Authorization: Bearer ...`）。
    #[serde(default)]
    pub bearer_token: Option<String>,
    /// 附加到每个请求上的自定义头。
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
    /// 单次 [`WorkflowHttp::guarded`] 调用的超时秒数。`0` = 不限；未配置 = 30 秒。
    ///
    /// 这是**外层**预算；reqwest client 自带的 30 秒总超时（`ecat_tls` 建的）
    /// 仍然生效，二者取先到者。
    #[serde(default)]
    pub query_timeout_secs: Option<u64>,
    /// 熔断配置；省略 = 保守默认阈值（失败率 0.5、窗口 30 秒、打开 10 秒）。
    #[serde(default)]
    pub breaker: Option<BreakerConfig>,
    /// 并发上限。`0` = **不限并发**（不建信号量）；未配置 = 32。
    #[serde(default)]
    pub max_concurrency: Option<usize>,
}

/// `0` = 显式禁用超时；未配置 = 30 秒。
pub(crate) fn query_timeout(secs: Option<u64>) -> Option<Duration> {
    match secs {
        None => Some(Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
        Some(0) => None,
        Some(s) => Some(Duration::from_secs(s)),
    }
}

/// `0` = 不限并发（不建信号量）；未配置 = 32。
pub(crate) fn concurrency_limit(max: Option<usize>) -> Option<Arc<Semaphore>> {
    match max {
        // 建 `Semaphore::new(0)` 会让每次调用永久挂在 `acquire()` 上：
        // 许可在最外层，没有任何预算能结束它。
        Some(0) => None,
        Some(n) => Some(Arc::new(Semaphore::new(n))),
        None => Some(Arc::new(Semaphore::new(DEFAULT_MAX_CONCURRENCY))),
    }
}

/// 逐适配器实例持有的 HTTP 出站外壳。
pub struct WorkflowHttp {
    client: reqwest::Client,
    base_url: String,
    username: Option<String>,
    password: Option<String>,
    bearer_token: Option<String>,
    headers: HashMap<String, String>,
    timeout: Option<Duration>,
    /// 逐 client 一个 —— 熔断器挂在**后端实例**上，不是进程上。
    breaker: Arc<Breaker>,
    /// `None` = 不限并发（`max_concurrency: 0`）。
    semaphore: Option<Arc<Semaphore>>,
}

impl WorkflowHttp {
    /// 构造外壳。同步、不做网络 I/O；TLS 证书文件在此时读取。
    pub fn new(cfg: WorkflowHttpConfig) -> Result<Self, WorkflowError> {
        let client = ecat_tls::build_reqwest_client(&cfg.tls)
            .map_err(|e| WorkflowError::Config(format!("TLS: {e}")))?;
        Ok(Self {
            client,
            // 末尾斜杠归一化，`request` 里只做一次拼接。
            base_url: cfg.base_url.trim_end_matches('/').to_string(),
            username: cfg.username,
            password: cfg.password,
            bearer_token: cfg.bearer_token,
            headers: cfg.headers,
            timeout: query_timeout(cfg.query_timeout_secs),
            breaker: Arc::new(Breaker::new(cfg.breaker.unwrap_or_default())),
            semaphore: concurrency_limit(cfg.max_concurrency),
        })
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// 拼出 `base_url + path` 并附加认证与自定义头。
    ///
    /// `path` 可带前导 `/` 与查询串；认证优先 `bearer_token`，否则 Basic。
    pub fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let req = self.client.request(method, url);
        let req = match &self.bearer_token {
            Some(token) => req.bearer_auth(token),
            None => ecat_tls::apply_basic_auth(req, &self.username, &self.password),
        };
        self.headers.iter().fold(req, |req, (name, value)| {
            req.header(name.as_str(), value.as_str())
        })
    }

    /// 一次出站调用的公共外壳：**许可 → 熔断 → 超时**。
    ///
    /// 顺序不能改：超时若在熔断外层，超时会 drop 内层 future，熔断器记失败的那句
    /// 在 `f().await` 之后，于是卡死的后端一次失败都记不上，熔断器永远不打开。
    /// 许可在最外：还在排队的请求没碰后端，不该计入熔断失败、也不该被超时掐断。
    ///
    /// `fut` 必须把非 2xx 响应转成 `Err` —— `Breaker` 只看 `Result`，
    /// `Ok` 一律记成功（没有 classify 钩子）。
    pub async fn guarded<F, T>(&self, fut: F) -> Result<T, WorkflowError>
    where
        F: std::future::Future<Output = Result<T, WorkflowError>> + Send,
        T: 'static,
    {
        let _permit = self.permit().await;
        let timeout = self.timeout;
        self.breaker
            .call(|| async move {
                match timeout {
                    Some(budget) => tokio::time::timeout(budget, fut).await.unwrap_or_else(|_| {
                        Err(WorkflowError::Timeout(format!(
                            "workflow request exceeded {}s",
                            budget.as_secs()
                        )))
                    }),
                    None => fut.await,
                }
            })
            .await
            .map_err(|e| match e {
                BreakerError::Inner(e) => e,
                open => WorkflowError::Unavailable(open.to_string()),
            })
    }

    /// 取一个并发许可；不限并发时返回 `None`。信号量从不 `close()`。
    async fn permit(&self) -> Option<SemaphorePermit<'_>> {
        match &self.semaphore {
            Some(sem) => Some(sem.acquire().await.expect("semaphore is never closed")),
            None => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn semaphore_permits(&self) -> Option<usize> {
        self.semaphore.as_ref().map(|s| s.available_permits())
    }
}
