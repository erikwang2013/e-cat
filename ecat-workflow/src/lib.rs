// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 工作流编排的统一契约与共享 HTTP 出站外壳。
//!
//! Airflow / DolphinScheduler / Azkaban / Oozie 四个适配器各自实现
//! [`WorkflowClient`]，并复用 [`WorkflowHttp`] 做全部出站调用：
//!
//! - [`WorkflowHttp::request`] 负责拼 URL、附加 Basic/Bearer 认证与自定义头；
//! - [`WorkflowHttp::guarded`] 负责超时、熔断与并发上限，顺序固定为
//!   **许可 → 熔断 → 超时**（超时在熔断内层，卡死的后端才会被计入失败）；
//! - [`WorkflowHttpConfig`] 可用 `#[serde(flatten)]` 嵌入适配器配置，
//!   但适配器自有字段不得与它的字段重名。
//!
//! 明确的非目标（v1）：指标（不依赖 ecat-metrics）。

mod error;
mod http;
mod spec;

#[cfg(test)]
mod tests;

pub use error::WorkflowError;
pub use http::{WorkflowHttp, WorkflowHttpConfig};
pub use spec::{
    RunFilter, RunId, RunState, RunStatus, WorkflowCapabilities, WorkflowId, WorkflowSpec,
};

use async_trait::async_trait;

/// 各引擎工作流 API 的统一契约。
///
/// 适配器只做三件事：把 [`WorkflowSpec`] 翻译成引擎请求、把引擎响应翻译成
/// [`RunStatus`] / id、把非 2xx 与引擎业务错误映射成 [`WorkflowError`]。
/// 所有 I/O 都必须经过 [`WorkflowHttp::guarded`]。
#[async_trait]
pub trait WorkflowClient: Send + Sync {
    /// 引擎名（如 `"airflow"`），用于日志与路由。
    fn engine(&self) -> &'static str;

    /// 本适配器实际支持的操作。
    fn capabilities(&self) -> WorkflowCapabilities;

    async fn create(&self, spec: &WorkflowSpec) -> Result<WorkflowId, WorkflowError>;

    async fn trigger(
        &self,
        workflow: &WorkflowId,
        params: serde_json::Value,
    ) -> Result<RunId, WorkflowError>;

    async fn status(&self, run: &RunId) -> Result<RunStatus, WorkflowError>;

    async fn cancel(&self, run: &RunId) -> Result<(), WorkflowError>;

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<RunStatus>, WorkflowError>;
}
