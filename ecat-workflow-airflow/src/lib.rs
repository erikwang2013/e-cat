// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Airflow 3.x REST API v2 适配器，可选 v1（Airflow 2.x）兼容。
//!
//! `base_url` 是 Airflow host 根（如 `http://host:8080`），适配器按
//! `api_version` 追加 `/api/v2/` 或 `/api/v1/`。
//!
//! Airflow 用 `(dag_id, dag_run_id)` 定位一次运行，而 [`WorkflowClient`] 的
//! `status` / `cancel` 只拿得到一个 [`RunId`]；因此本适配器把 `RunId` 编码成
//! `"{dag_id}/{dag_run_id}"`。Airflow 的 dag_id 只允许字母数字与 `-_.`，不会含
//! `/`，按第一个 `/` 切分无歧义。
//!
//! v2 触发 body 必须带 `logical_date`（可为 null）：`params` 显式给出时取它，
//! 否则发 null；v1 不发该字段。v2 的 JWT 由 `WorkflowHttpConfig.bearer_token`
//! 承载（先用 `POST /auth/token` 换取）；v1 常用 basic auth 或 API key header。
//!
//! `create` 不支持：DAG 是部署到 dags 目录的 Python 文件，v2 的 `/dags` 只有
//! GET，REST 不能创建 DAG。
//!
//! `cancel` 只能 PATCH `state=failed`（v2 可变状态只有 queued/success/failed，
//! 没有 cancelled）。Airflow 会把该运行中未结束的 task instance 标成 failed、
//! 挂起的标成 skipped；运行中的任务在下一次心跳发现状态被外部改动后自行终止。
//! 这不是立即 SIGKILL，阻塞在非 Airflow 代码里的进程也不会被马上杀掉。v2 的
//! `POST .../clear` 是清空重跑，[`WorkflowClient`] 没有对应方法，本适配器不暴露。

use async_trait::async_trait;
use ecat_workflow::{
    RunFilter, RunId, RunState, RunStatus, WorkflowCapabilities, WorkflowClient, WorkflowError,
    WorkflowHttp, WorkflowHttpConfig, WorkflowId, WorkflowSpec,
};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

/// Airflow REST API 版本。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiVersion {
    V1,
    V2,
}

impl ApiVersion {
    fn parse(raw: &str) -> Result<Self, WorkflowError> {
        match raw {
            "v2" => Ok(Self::V2),
            "v1" => Ok(Self::V1),
            other => Err(WorkflowError::Config(format!(
                "Airflow api_version must be \"v2\" or \"v1\", got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "v1",
            Self::V2 => "v2",
        }
    }
}

/// 宽容去掉旧式 API 根后缀，避免拼成 `/api/v1/api/v2`。
fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim_end_matches('/');
    for suffix in ["/api/v1", "/api/v2"] {
        if let Some(base) = trimmed.strip_suffix(suffix) {
            return base.to_string();
        }
    }
    trimmed.to_string()
}

/// Airflow 适配器配置。
#[derive(Debug, Clone, Deserialize)]
pub struct AirflowConfig {
    #[serde(flatten)]
    pub http: WorkflowHttpConfig,
    /// `"v2"`（默认，Airflow 3.x）或 `"v1"`（Airflow 2.x）。
    #[serde(default)]
    pub api_version: Option<String>,
    /// 设置后触发时生成 `"{prefix}{纳秒时间戳}"` 作为 `dag_run_id`；
    /// 不设则由 Airflow 按 logical date 生成。
    #[serde(default)]
    pub dag_run_id_prefix: Option<String>,
}

/// 逐实例持有出站外壳的 Airflow 客户端。
pub struct AirflowClient {
    http: WorkflowHttp,
    api_version: ApiVersion,
    dag_run_id_prefix: Option<String>,
}

impl AirflowClient {
    /// 同步构造；不做网络 I/O。
    pub fn from_config(mut cfg: AirflowConfig) -> Result<Self, WorkflowError> {
        let api_version = ApiVersion::parse(cfg.api_version.as_deref().unwrap_or("v2"))?;
        // 宽容：允许用户把 API 根（`/api/v1` / `/api/v2`）写进 base_url。
        cfg.http.base_url = normalize_base_url(&cfg.http.base_url);
        Ok(Self {
            http: WorkflowHttp::new(cfg.http)?,
            api_version,
            dag_run_id_prefix: cfg.dag_run_id_prefix,
        })
    }

    /// 在 host 根 base_url 后拼 `/api/{version}/`。
    fn path(&self, tail: &str) -> String {
        format!("api/{}/{tail}", self.api_version.as_str())
    }

    /// 检查 2xx 并解析 JSON；**必须在 `guarded` 内调用**（这里直接做 I/O）。
    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, WorkflowError> {
        let resp = req
            .send()
            .await
            .map_err(|e| WorkflowError::Transport(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp
            .text()
            .await
            .map_err(|e| WorkflowError::Transport(e.to_string()))?;
        if !(200..300).contains(&status) {
            return Err(WorkflowError::Http { status, body });
        }
        serde_json::from_str(&body)
            .map_err(|e| WorkflowError::Parse(format!("Airflow response is not JSON: {e}")))
    }

    /// 拆 `dag_id/dag_run_id` 编码。
    fn split_run(run: &RunId) -> Result<(&str, &str), WorkflowError> {
        run.0.split_once('/').ok_or_else(|| {
            WorkflowError::Config(format!(
                "Airflow RunId must be 'dag_id/dag_run_id', got {:?}",
                run.0
            ))
        })
    }

    /// 把 DagRun JSON 翻成统一状态快照。
    fn run_status(v: &Value) -> Result<RunStatus, WorkflowError> {
        let dag_id = v["dag_id"]
            .as_str()
            .ok_or_else(|| WorkflowError::Parse("DAG run has no dag_id".to_string()))?;
        let run_id = v["dag_run_id"]
            .as_str()
            .ok_or_else(|| WorkflowError::Parse("DAG run has no dag_run_id".to_string()))?;
        let state = v["state"]
            .as_str()
            .ok_or_else(|| WorkflowError::Parse("DAG run has no state".to_string()))?;
        Ok(RunStatus {
            run_id: RunId(format!("{dag_id}/{run_id}")),
            workflow_id: WorkflowId(dag_id.to_string()),
            state: map_state(state),
            started_at: v["start_date"].as_str().map(str::to_owned),
            finished_at: v["end_date"].as_str().map(str::to_owned),
            message: None,
            raw: v.clone(),
        })
    }
}

#[async_trait]
impl WorkflowClient for AirflowClient {
    fn engine(&self) -> &'static str {
        "airflow"
    }

    fn capabilities(&self) -> WorkflowCapabilities {
        WorkflowCapabilities {
            create: false,
            trigger: true,
            cancel: true,
            list_runs: true,
        }
    }

    async fn create(&self, _spec: &WorkflowSpec) -> Result<WorkflowId, WorkflowError> {
        Err(WorkflowError::Unsupported(
            "Airflow DAGs are deployed as files; REST cannot create them",
        ))
    }

    async fn trigger(&self, workflow: &WorkflowId, params: Value) -> Result<RunId, WorkflowError> {
        let dag_id = workflow.0.as_str();
        let path = self.path(&format!("dags/{}/dagRuns", segment(dag_id)));
        let mut body = serde_json::Map::new();
        // v2 的 `logical_date` 必填（可为 null）；params 显式给出时顶层复用它。
        if self.api_version == ApiVersion::V2 {
            body.insert(
                "logical_date".to_string(),
                params.get("logical_date").cloned().unwrap_or(Value::Null),
            );
        }
        // `conf` 必须是对象；无参数时显式发 `{}`。
        body.insert(
            "conf".to_string(),
            if params.is_null() { json!({}) } else { params },
        );
        if let Some(prefix) = &self.dag_run_id_prefix {
            body.insert(
                "dag_run_id".to_string(),
                json!(format!("{prefix}{}", now_nanos())),
            );
        }
        let body = Value::Object(body);

        let resp = self
            .http
            .guarded(async {
                self.send(self.http.request(Method::POST, &path).json(&body))
                    .await
            })
            .await?;
        let run_id = resp["dag_run_id"].as_str().ok_or_else(|| {
            WorkflowError::Parse("trigger response has no dag_run_id".to_string())
        })?;
        Ok(RunId(format!("{dag_id}/{run_id}")))
    }

    async fn status(&self, run: &RunId) -> Result<RunStatus, WorkflowError> {
        let (dag_id, run_id) = Self::split_run(run)?;
        let path = self.path(&format!(
            "dags/{}/dagRuns/{}",
            segment(dag_id),
            segment(run_id)
        ));
        let resp = self
            .http
            .guarded(async { self.send(self.http.request(Method::GET, &path)).await })
            .await?;
        Self::run_status(&resp)
    }

    async fn cancel(&self, run: &RunId) -> Result<(), WorkflowError> {
        let (dag_id, run_id) = Self::split_run(run)?;
        let path = self.path(&format!(
            "dags/{}/dagRuns/{}",
            segment(dag_id),
            segment(run_id)
        ));
        // v1/v2 都没有 stop：标 failed 是唯一能停住调度的操作，见模块文档。
        let body = json!({"state": "failed"});
        self.http
            .guarded(async {
                self.send(self.http.request(Method::PATCH, &path).json(&body))
                    .await
                    .map(|_| ())
            })
            .await
    }

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<RunStatus>, WorkflowError> {
        // dagRuns 列表以 dag_id 为路径参数；本适配器要求调用方显式给出 dag_id。
        let workflow = filter
            .workflow_id
            .as_ref()
            .ok_or_else(|| WorkflowError::Remote("workflow_id (dag_id) is required".to_string()))?;
        let mut path = self.path(&format!("dags/{}/dagRuns", segment(&workflow.0)));
        let mut query = Vec::new();
        if let Some(limit) = filter.limit {
            query.push(format!("limit={limit}"));
        }
        if let Some(state) = filter.state {
            query.push(format!("state={}", filter_state(state)?));
        }
        if !query.is_empty() {
            path.push('?');
            path.push_str(&query.join("&"));
        }

        let resp = self
            .http
            .guarded(async { self.send(self.http.request(Method::GET, &path)).await })
            .await?;
        let runs = resp["dag_runs"].as_array().ok_or_else(|| {
            WorkflowError::Parse("list response has no dag_runs array".to_string())
        })?;
        runs.iter().map(Self::run_status).collect()
    }
}

/// Airflow DagState -> 统一状态。
fn map_state(state: &str) -> RunState {
    match state {
        "queued" => RunState::Queued,
        "running" => RunState::Running,
        "success" => RunState::Succeeded,
        "failed" | "upstream_failed" => RunState::Failed,
        // dag run 正常不会有 skipped，兼容上游返回。
        "skipped" => RunState::Cancelled,
        _ => RunState::Unknown,
    }
}

/// 统一状态 -> Airflow `state` 过滤值。
fn filter_state(state: RunState) -> Result<&'static str, WorkflowError> {
    match state {
        RunState::Queued => Ok("queued"),
        RunState::Running => Ok("running"),
        RunState::Succeeded => Ok("success"),
        RunState::Failed => Ok("failed"),
        // Airflow 没有 cancelled 状态；cancel 也是标 failed，过滤时等价。
        RunState::Cancelled => Ok("failed"),
        RunState::Unknown => Err(WorkflowError::Config(
            "cannot filter Airflow runs by RunState::Unknown".to_string(),
        )),
    }
}

/// 路径段百分号编码：`dag_id` 本身安全，但 `dag_run_id` 可以由调用方任意给。
fn segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// 生成 run id 后缀：纳秒时间戳。
fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos())
}
