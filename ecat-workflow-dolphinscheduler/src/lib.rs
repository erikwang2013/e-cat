// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache DolphinScheduler 3.4.3 REST API 适配器，兼容 3.2 及更早的旧命名。
//!
//! `base_url` 是上下文根 `http://host:12345/dolphinscheduler`（没有版本段）。
//! `api_style` 选择接口命名，不按版本字符串猜：
//! - `"workflow"`（默认，3.3+）：`workflow-definition` / `workflow-instances` /
//!   `start-workflow-instance`；create 会紧接着 POST `.../release`（ONLINE），
//!   因为新定义默认 OFFLINE；start 直接返回 workflow instance id。
//! - `"legacy"`（3.2-）：`process-definition` / `process-instances` /
//!   `start-process-instance`；create 不自动发布；start 只返回 triggerCode，
//!   适配器在同一个 `guarded` 调用内轮询解析出 instance id（约 10 秒预算）。
//!
//! 约定：
//! - [`WorkflowId`] 是定义 code，[`RunId`] 是实例 id，均为十进制字符串。
//! - 写接口全部是 form 编码（`@RequestParam`）；`taskDefinitionJson` /
//!   `taskRelationJson` / `locations` 的值本身是 JSON 字符串。
//! - `schedule` 不实现：DS 的定时是独立的 schedules API，与单次触发无关。
//! - `cancel` 是 executors/execute + STOP，由 master 消费后停止运行中的任务，
//!   不是同步杀死进程。

mod util;

use async_trait::async_trait;
use ecat_workflow::{
    RunFilter, RunId, RunStatus, WorkflowCapabilities, WorkflowClient, WorkflowError, WorkflowHttp,
    WorkflowHttpConfig, WorkflowId, WorkflowSpec,
};
use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use util::{engine_state, form_json, json_i64, map_state, now_schedule_time, parse_i64, required};

/// legacy trigger 的轮询次数 × 间隔 ≈ 10 秒，留给 master 消费命令并落库。
const TRIGGER_POLL_ATTEMPTS: usize = 50;
const TRIGGER_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// DS 接口命名风格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiStyle {
    Workflow,
    Legacy,
}

impl ApiStyle {
    fn parse(raw: &str) -> Result<Self, WorkflowError> {
        match raw {
            "workflow" => Ok(Self::Workflow),
            "legacy" => Ok(Self::Legacy),
            other => Err(WorkflowError::Config(format!(
                "DolphinScheduler api_style must be \"workflow\" or \"legacy\", got {other:?}"
            ))),
        }
    }
}

/// DolphinScheduler 适配器配置。
#[derive(Debug, Clone, Deserialize)]
pub struct DolphinSchedulerConfig {
    #[serde(flatten)]
    pub http: WorkflowHttpConfig,
    /// 项目 code（`t_ds_project.code`，非 id）。
    pub project_code: i64,
    /// API token，以 `token` 头发送；配置了它就会覆盖 `http.headers` 里的 `token`。
    #[serde(default)]
    pub token: Option<String>,
    /// 租户 code。3.2.0 的 trigger 使用它；create 侧 3.2.0 已无此参数，
    /// 仍会随表单发送以兼容 3.1.x（3.2.0 忽略未知表单字段）。
    #[serde(default)]
    pub tenant_code: Option<String>,
    /// `"workflow"`（默认，3.3+）或 `"legacy"`（3.2-）。
    #[serde(default)]
    pub api_style: Option<String>,
}

/// 逐实例持有出站外壳的 DolphinScheduler 客户端。
pub struct DolphinSchedulerClient {
    http: WorkflowHttp,
    project_code: i64,
    token: Option<String>,
    tenant_code: Option<String>,
    api_style: ApiStyle,
}

/// DS 统一响应包：业务错误是 HTTP 200 + `code != 0`。
#[derive(Deserialize)]
struct Envelope {
    code: i64,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    data: Value,
}

impl DolphinSchedulerClient {
    /// 同步构造；不做网络 I/O。
    pub fn from_config(mut cfg: DolphinSchedulerConfig) -> Result<Self, WorkflowError> {
        if cfg.project_code <= 0 {
            return Err(WorkflowError::Config(
                "DolphinScheduler project_code must be positive".to_string(),
            ));
        }
        let api_style = ApiStyle::parse(cfg.api_style.as_deref().unwrap_or("workflow"))?;
        // 配置字段优先于通用 headers，避免同一个头出现两个值。
        if cfg.token.is_some() {
            cfg.http
                .headers
                .retain(|name, _| !name.eq_ignore_ascii_case("token"));
        }
        Ok(Self {
            http: WorkflowHttp::new(cfg.http)?,
            project_code: cfg.project_code,
            token: cfg.token,
            tenant_code: cfg.tenant_code,
            api_style,
        })
    }

    /// 带 token 头的请求构造器；**I/O 必须在 `guarded` 内发生**。
    fn form_request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let req = self.http.request(method, path);
        match &self.token {
            Some(token) => req.header("token", token),
            None => req,
        }
    }

    fn definition_segment(&self) -> &'static str {
        match self.api_style {
            ApiStyle::Workflow => "workflow-definition",
            ApiStyle::Legacy => "process-definition",
        }
    }

    fn instance_segment(&self) -> &'static str {
        match self.api_style {
            ApiStyle::Workflow => "workflow-instances",
            ApiStyle::Legacy => "process-instances",
        }
    }

    /// 两种风格的 start 表单字段；workflow 风格额外带优先级与 workerGroup。
    fn start_form(
        &self,
        definition_field: &'static str,
        definition: i64,
        params: &Value,
    ) -> Vec<(&'static str, String)> {
        let mut form = vec![
            (definition_field, definition.to_string()),
            ("scheduleTime", now_schedule_time()),
            ("failureStrategy", "CONTINUE".to_string()),
            ("warningType", "NONE".to_string()),
        ];
        if self.api_style == ApiStyle::Workflow {
            form.push(("workflowInstancePriority", "MEDIUM".to_string()));
            form.push(("workerGroup", "default".to_string()));
        }
        if let Some(tenant) = &self.tenant_code {
            form.push(("tenantCode", tenant.clone()));
        }
        if !params.is_null() {
            let start_params = match params {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            form.push(("startParams", start_params));
        }
        form
    }

    /// 发送表单请求，检查 HTTP 状态与 DS 业务码，返回 `data`。
    /// **必须在 `guarded` 内调用**（这里直接做 I/O）。
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
        let envelope: Envelope = serde_json::from_str(&body).map_err(|e| {
            WorkflowError::Parse(format!("DolphinScheduler response is not JSON: {e}"))
        })?;
        if envelope.code != 0 {
            return Err(WorkflowError::Remote(format!(
                "DolphinScheduler error {}: {}",
                envelope.code,
                envelope.msg.unwrap_or_default()
            )));
        }
        Ok(envelope.data)
    }

    /// 部分部署的 start 只返回 triggerCode；轮询 trigger 列表解析实例 id。
    /// **必须在 `guarded` 内调用**（这里直接做 I/O）。
    async fn poll_trigger(
        &self,
        trigger_path: &str,
        data: &Value,
        label: &str,
    ) -> Result<RunId, WorkflowError> {
        let trigger_code = json_i64(data, "trigger code")?;
        let poll = format!("{trigger_path}?triggerCode={trigger_code}");
        for attempt in 0..TRIGGER_POLL_ATTEMPTS {
            let data = self.send(self.form_request(Method::GET, &poll)).await?;
            if let Some(instance) = data.as_array().and_then(|list| list.first()) {
                let id = json_i64(&instance["id"], "workflow instance id")?;
                return Ok(RunId(id.to_string()));
            }
            if attempt + 1 < TRIGGER_POLL_ATTEMPTS {
                tokio::time::sleep(TRIGGER_POLL_INTERVAL).await;
            }
        }
        Err(WorkflowError::Remote(format!(
            "DolphinScheduler accepted triggerCode {trigger_code} but no {label} appeared within ~10s"
        )))
    }

    /// 把实例 JSON 翻成统一状态快照。3.4 返回 `workflowDefinitionCode`，
    /// 3.2 返回 `processDefinitionCode`。
    fn run_status(v: &Value) -> Result<RunStatus, WorkflowError> {
        let id = json_i64(&v["id"], "workflow instance id")?;
        let definition = json_i64(&v["workflowDefinitionCode"], "workflow definition code")
            .or_else(|_| json_i64(&v["processDefinitionCode"], "process definition code"))?;
        let state = v["state"]
            .as_str()
            .ok_or_else(|| WorkflowError::Parse("workflow instance has no state".to_string()))?;
        Ok(RunStatus {
            run_id: RunId(id.to_string()),
            workflow_id: WorkflowId(definition.to_string()),
            state: map_state(state),
            started_at: v["startTime"].as_str().map(str::to_owned),
            finished_at: v["endTime"].as_str().map(str::to_owned),
            message: None,
            raw: v.clone(),
        })
    }
}

#[async_trait]
impl WorkflowClient for DolphinSchedulerClient {
    fn engine(&self) -> &'static str {
        "dolphinscheduler"
    }

    fn capabilities(&self) -> WorkflowCapabilities {
        WorkflowCapabilities {
            create: true,
            trigger: true,
            cancel: true,
            list_runs: true,
        }
    }

    async fn create(&self, spec: &WorkflowSpec) -> Result<WorkflowId, WorkflowError> {
        let definition = spec.definition.as_object().ok_or_else(|| {
            WorkflowError::Config("DolphinScheduler definition must be a JSON object".to_string())
        })?;
        let mut form = vec![
            ("name", spec.name.clone()),
            (
                "taskDefinitionJson",
                form_json(required(definition, "taskDefinitionJson")?),
            ),
            (
                "taskRelationJson",
                form_json(required(definition, "taskRelationJson")?),
            ),
            (
                "locations",
                form_json(definition.get("locations").unwrap_or(&json!([]))),
            ),
            (
                "executionType",
                definition
                    .get("executionType")
                    .and_then(Value::as_str)
                    .unwrap_or("PARALLEL")
                    .to_string(),
            ),
        ];
        if let Some(description) = definition.get("description").and_then(Value::as_str) {
            form.push(("description", description.to_string()));
        }
        // 3.3+ 的 create 控制器没有 tenantCode 参数。
        if self.api_style == ApiStyle::Legacy
            && let Some(tenant) = &self.tenant_code
        {
            form.push(("tenantCode", tenant.clone()));
        }

        let path = format!(
            "projects/{}/{}",
            self.project_code,
            self.definition_segment()
        );
        let code = self
            .http
            .guarded(async {
                let data = self
                    .send(self.form_request(Method::POST, &path).form(&form))
                    .await?;
                let code = json_i64(&data["code"], "workflow definition code")?;
                if self.api_style == ApiStyle::Workflow {
                    // 3.3+ 新建定义默认 OFFLINE；不发布就无法 trigger。
                    let release_path = format!(
                        "projects/{}/{}/{code}/release",
                        self.project_code,
                        self.definition_segment()
                    );
                    let release = vec![("releaseState", "ONLINE".to_string())];
                    self.send(
                        self.form_request(Method::POST, &release_path)
                            .form(&release),
                    )
                    .await?;
                }
                Ok(code)
            })
            .await?;
        Ok(WorkflowId(code.to_string()))
    }

    async fn trigger(&self, workflow: &WorkflowId, params: Value) -> Result<RunId, WorkflowError> {
        let definition = parse_i64(&workflow.0, "workflow_id (definition code)")?;
        match self.api_style {
            ApiStyle::Workflow => {
                let form = self.start_form("workflowDefinitionCode", definition, &params);
                let start_path = format!(
                    "projects/{}/executors/start-workflow-instance",
                    self.project_code
                );
                let trigger_path =
                    format!("projects/{}/workflow-instances/trigger", self.project_code);
                self.http
                    .guarded(async {
                        let data = self
                            .send(self.form_request(Method::POST, &start_path).form(&form))
                            .await?;
                        // 3.4.3 直接返回实例 id 数组；部分部署仍回 triggerCode，退化为轮询。
                        match data.as_array().map(|list| list.as_slice()) {
                            Some([instance, ..]) => {
                                let id = json_i64(instance, "workflow instance id")?;
                                Ok(RunId(id.to_string()))
                            }
                            Some([]) => Err(WorkflowError::Remote(
                                "start-workflow-instance returned no workflow instance".to_string(),
                            )),
                            None => {
                                self.poll_trigger(&trigger_path, &data, "workflow instance")
                                    .await
                            }
                        }
                    })
                    .await
            }
            ApiStyle::Legacy => {
                let form = self.start_form("processDefinitionCode", definition, &params);
                let start_path = format!(
                    "projects/{}/executors/start-process-instance",
                    self.project_code
                );
                let trigger_path =
                    format!("projects/{}/process-instances/trigger", self.project_code);
                self.http
                    .guarded(async {
                        let data = self
                            .send(self.form_request(Method::POST, &start_path).form(&form))
                            .await?;
                        self.poll_trigger(&trigger_path, &data, "process instance")
                            .await
                    })
                    .await
            }
        }
    }

    async fn status(&self, run: &RunId) -> Result<RunStatus, WorkflowError> {
        let id = parse_i64(&run.0, "run_id (workflow instance id)")?;
        let path = format!(
            "projects/{}/{}/{id}",
            self.project_code,
            self.instance_segment()
        );
        let data = self
            .http
            .guarded(async { self.send(self.form_request(Method::GET, &path)).await })
            .await?;
        Self::run_status(&data)
    }

    async fn cancel(&self, run: &RunId) -> Result<(), WorkflowError> {
        let id = parse_i64(&run.0, "run_id (workflow instance id)")?;
        let path = format!("projects/{}/executors/execute", self.project_code);
        let req = self.form_request(Method::POST, &path);
        let req = match self.api_style {
            // 3.3+ 的 execute 用 query 参数；3.2 用表单。
            ApiStyle::Workflow => req.query(&[
                ("workflowInstanceId", id.to_string()),
                ("executeType", "STOP".to_string()),
            ]),
            ApiStyle::Legacy => req.form(&[
                ("processInstanceId", id.to_string()),
                ("executeType", "STOP".to_string()),
            ]),
        };
        self.http
            .guarded(async { self.send(req).await.map(|_| ()) })
            .await
    }

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<RunStatus>, WorkflowError> {
        let mut path = format!(
            "projects/{}/{}?pageNo=1&pageSize={}",
            self.project_code,
            self.instance_segment(),
            filter.limit.unwrap_or(10)
        );
        if let Some(workflow) = &filter.workflow_id {
            let definition = parse_i64(&workflow.0, "workflow_id (definition code)")?;
            let field = match self.api_style {
                ApiStyle::Workflow => "workflowDefinitionCode",
                ApiStyle::Legacy => "processDefineCode",
            };
            path.push_str(&format!("&{field}={definition}"));
        }
        if let Some(state) = filter.state {
            path.push_str("&stateType=");
            path.push_str(engine_state(state)?);
        }

        let data = self
            .http
            .guarded(async { self.send(self.form_request(Method::GET, &path)).await })
            .await?;
        let runs = data["totalList"].as_array().ok_or_else(|| {
            WorkflowError::Parse("list response has no data.totalList array".to_string())
        })?;
        runs.iter().map(Self::run_status).collect()
    }
}
