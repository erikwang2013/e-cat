// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Azkaban 3.x legacy REST 适配器（仓库已归档，最后 release 3.90.0，无 4.x）。
//!
//! - 认证：`POST /?action=login` 表单登录，从响应体或 `Set-Cookie`
//!   （`azkaban.browser.session.id`）取 `session.id`，缓存在本实例上；
//!   后续请求把 `session.id` 作为 query/form 参数携带。
//! - `create`：`definition` 为 base64 zip，或
//!   `{"project", "flow", "zip_base64"}`；先建项目再 multipart 上传。
//! - `WorkflowId` / `RunId` 编码：`project/flow` 与数字 execid。
//! - `schedule` 不在适配范围内，忽略。
//!
//! 所有出站调用都经过 [`WorkflowHttp::guarded`]；登录与业务请求是两次独立
//! 出站，各自包一层，不嵌套。

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ecat_workflow::{
    RunFilter, RunId, RunState, RunStatus, WorkflowCapabilities, WorkflowClient, WorkflowError,
    WorkflowHttp, WorkflowHttpConfig, WorkflowId, WorkflowSpec,
};
use reqwest::Method;
use reqwest::header::SET_COOKIE;
use reqwest::multipart::{Form, Part};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;

/// Azkaban web server 登录后返回的 cookie 名。
const SESSION_COOKIE: &str = "azkaban.browser.session.id";

/// Azkaban 适配器配置。
#[derive(Debug, Clone, Deserialize)]
pub struct AzkabanConfig {
    #[serde(flatten)]
    pub http: WorkflowHttpConfig,
    /// 默认项目名；`WorkflowSpec.name` 也可用。
    #[serde(default)]
    pub project: Option<String>,
}

/// Azkaban 客户端。会话缓存在实例内，多个请求共享同一次登录。
pub struct AzkabanClient {
    http: WorkflowHttp,
    project: Option<String>,
    username: String,
    password: String,
    session: Mutex<Option<String>>,
}

impl AzkabanClient {
    /// 构造客户端。同步、不做网络 I/O（首次调用时才登录）。
    pub fn from_config(cfg: AzkabanConfig) -> Result<Self, WorkflowError> {
        let username = cfg
            .http
            .username
            .clone()
            .ok_or_else(|| WorkflowError::Config("azkaban: username is required".into()))?;
        let password = cfg
            .http
            .password
            .clone()
            .ok_or_else(|| WorkflowError::Config("azkaban: password is required".into()))?;
        Ok(Self {
            http: WorkflowHttp::new(cfg.http)?,
            project: cfg.project,
            username,
            password,
            session: Mutex::new(None),
        })
    }

    /// 一次业务调用：取 session → 发请求；session 失效时清缓存、重登一次。
    ///
    /// 首次尝试与重试各自是一次 [`WorkflowHttp::guarded`]（不嵌套）：
    /// 登录本身也是一次独立的 guarded。
    async fn call<F>(&self, build: F) -> Result<Value, WorkflowError>
    where
        F: Fn(&str) -> reqwest::RequestBuilder + Sync,
    {
        let sid = self.session().await?;
        match self.attempt(&sid, &build).await {
            Err(err) if session_expired(&err) => {
                self.invalidate(&sid).await;
                let sid = self.session().await?;
                self.attempt(&sid, &build).await
            }
            result => result,
        }
    }

    async fn attempt<F>(&self, sid: &str, build: &F) -> Result<Value, WorkflowError>
    where
        F: Fn(&str) -> reqwest::RequestBuilder + Sync,
    {
        self.http
            .guarded(async {
                let resp = build(sid)
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
                let value: Value = serde_json::from_str(&body).map_err(|e| {
                    WorkflowError::Parse(format!("azkaban: invalid JSON: {e}: {}", snippet(&body)))
                })?;
                // Azkaban 的业务错误统一走 `error` 字段（HTTP 仍是 200）。
                match value.get("error") {
                    Some(err) => Err(WorkflowError::Remote(format!(
                        "azkaban: {}",
                        display_json(err)
                    ))),
                    None => Ok(value),
                }
            })
            .await
    }

    /// 取缓存的 session；没有则登录。锁跨登录持有，并发调用只登一次。
    async fn session(&self) -> Result<String, WorkflowError> {
        let mut slot = self.session.lock().await;
        if let Some(sid) = slot.as_ref() {
            return Ok(sid.clone());
        }
        let sid = self.login().await?;
        *slot = Some(sid.clone());
        Ok(sid)
    }

    /// 只清掉「此次失败时使用的那个」session，避免抹掉别的任务刚刷新的。
    async fn invalidate(&self, used: &str) {
        let mut slot = self.session.lock().await;
        if slot.as_deref() == Some(used) {
            *slot = None;
        }
    }

    /// `POST /?action=login`。session 优先取响应体，其次取 Set-Cookie。
    async fn login(&self) -> Result<String, WorkflowError> {
        let username = self.username.clone();
        let password = self.password.clone();
        self.http
            .guarded(async {
                let resp = self
                    .http
                    .request(Method::POST, "/?action=login")
                    .form(&[
                        ("action", "login"),
                        ("username", username.as_str()),
                        ("password", password.as_str()),
                    ])
                    .send()
                    .await
                    .map_err(|e| WorkflowError::Transport(e.to_string()))?;
                let status = resp.status().as_u16();
                let cookie = cookie_session(resp.headers());
                let body = resp
                    .text()
                    .await
                    .map_err(|e| WorkflowError::Transport(e.to_string()))?;
                if !(200..300).contains(&status) {
                    return Err(WorkflowError::Http { status, body });
                }
                let value: Value = serde_json::from_str(&body).map_err(|e| {
                    WorkflowError::Parse(format!(
                        "azkaban: login response: {e}: {}",
                        snippet(&body)
                    ))
                })?;
                if let Some(err) = value.get("error") {
                    return Err(WorkflowError::Remote(format!(
                        "azkaban: login failed: {}",
                        display_json(err)
                    )));
                }
                value
                    .get("session.id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(cookie)
                    .ok_or_else(|| {
                        WorkflowError::Remote("azkaban: login response missing session.id".into())
                    })
            })
            .await
    }

    /// 解出 `(project, flow, zip_bytes)`。
    fn decode_definition(
        &self,
        spec: &WorkflowSpec,
    ) -> Result<(String, String, Vec<u8>), WorkflowError> {
        let (project, flow, encoded) = match &spec.definition {
            Value::String(encoded) => (
                self.project.clone().unwrap_or_else(|| spec.name.clone()),
                spec.name.clone(),
                encoded.clone(),
            ),
            Value::Object(map) => {
                let encoded = map
                    .get("zip_base64")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        WorkflowError::Config("azkaban: definition.zip_base64 is required".into())
                    })?;
                let project = map
                    .get("project")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| self.project.clone())
                    .unwrap_or_else(|| spec.name.clone());
                let flow = map
                    .get("flow")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| spec.name.clone());
                (project, flow, encoded.to_string())
            }
            _ => {
                return Err(WorkflowError::Config(
                    "azkaban: definition must be a base64 zip string or \
                     {\"project\", \"flow\", \"zip_base64\"}"
                        .into(),
                ));
            }
        };
        let zip = STANDARD.decode(&encoded).map_err(|e| {
            WorkflowError::Config(format!("azkaban: zip_base64 is not valid base64: {e}"))
        })?;
        Ok((project, flow, zip))
    }
}

#[async_trait::async_trait]
impl WorkflowClient for AzkabanClient {
    fn engine(&self) -> &'static str {
        "azkaban"
    }

    fn capabilities(&self) -> WorkflowCapabilities {
        WorkflowCapabilities {
            create: true,
            trigger: true,
            cancel: true,
            list_runs: false,
        }
    }

    async fn create(&self, spec: &WorkflowSpec) -> Result<WorkflowId, WorkflowError> {
        let (project, flow, zip) = self.decode_definition(spec)?;
        self.call(|sid| {
            self.http
                .request(Method::POST, "/manager?action=create")
                .form(&[
                    ("session.id", sid),
                    ("name", project.as_str()),
                    ("description", spec.name.as_str()),
                ])
        })
        .await?;
        self.call(|sid| {
            let file = Part::bytes(zip.clone())
                .file_name(format!("{flow}.zip"))
                .mime_str("application/zip")
                .expect("static MIME type is valid");
            let form = Form::new()
                .text("ajax", "upload")
                .text("session.id", sid.to_string())
                .text("project", project.clone())
                .part("file", file);
            self.http
                .request(Method::POST, "/manager?ajax=upload")
                .multipart(form)
        })
        .await?;
        Ok(WorkflowId(format!("{project}/{flow}")))
    }

    async fn trigger(&self, workflow: &WorkflowId, params: Value) -> Result<RunId, WorkflowError> {
        let (project, flow) = split_id(&workflow.0)?;
        let overrides = match params {
            Value::Null => Vec::new(),
            Value::Object(map) => map
                .into_iter()
                .map(|(key, value)| (format!("flowOverride[{key}]"), display_json(&value)))
                .collect::<Vec<_>>(),
            _ => {
                return Err(WorkflowError::Unsupported(
                    "azkaban: trigger params must be a JSON object",
                ));
            }
        };
        let value = self
            .call(|sid| {
                let mut form = vec![
                    ("session.id".to_string(), sid.to_string()),
                    ("project".to_string(), project.clone()),
                    ("flow".to_string(), flow.clone()),
                ];
                form.extend(overrides.clone());
                self.http
                    .request(Method::POST, "/executor?ajax=executeFlow")
                    .form(&form)
            })
            .await?;
        let execid = value.get("execid").and_then(id_string).ok_or_else(|| {
            WorkflowError::Parse("azkaban: executeFlow response missing execid".into())
        })?;
        Ok(RunId(execid))
    }

    async fn status(&self, run: &RunId) -> Result<RunStatus, WorkflowError> {
        let value = self
            .call(|sid| {
                self.http
                    .request(Method::GET, "/executor?ajax=fetchexecflow")
                    .query(&[("execid", run.0.as_str()), ("session.id", sid)])
            })
            .await?;
        let project = value
            .get("project")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let flow = value
            .get("flow")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let workflow_id = if project.is_empty() {
            WorkflowId(flow.to_string())
        } else {
            WorkflowId(format!("{project}/{flow}"))
        };
        Ok(RunStatus {
            run_id: run.clone(),
            workflow_id,
            state: map_state(
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            started_at: timestamp(&value, "startTime"),
            finished_at: timestamp(&value, "endTime"),
            message: None,
            raw: value,
        })
    }

    async fn cancel(&self, run: &RunId) -> Result<(), WorkflowError> {
        self.call(|sid| {
            self.http
                .request(Method::GET, "/executor?ajax=cancelFlow")
                .query(&[("execid", run.0.as_str()), ("session.id", sid)])
        })
        .await?;
        Ok(())
    }

    async fn list_runs(&self, _filter: &RunFilter) -> Result<Vec<RunStatus>, WorkflowError> {
        // 官方 ajax API 未明确核实列表端点，不猜。
        Err(WorkflowError::Unsupported(
            "azkaban: list_runs is not supported",
        ))
    }
}

/// `project/flow` → 两段；形状不对按本地配置错误处理。
fn split_id(id: &str) -> Result<(String, String), WorkflowError> {
    match id.split_once('/') {
        Some((project, flow)) if !project.is_empty() && !flow.is_empty() && !flow.contains('/') => {
            Ok((project.to_string(), flow.to_string()))
        }
        _ => Err(WorkflowError::Config(format!(
            "azkaban: workflow id must be project/flow, got {id:?}"
        ))),
    }
}

fn map_state(status: &str) -> RunState {
    match status {
        "READY" | "DISPATCHING" | "PREPARING" | "QUEUED" => RunState::Queued,
        "RUNNING" | "PAUSED" | "FAILED_FINISHING" | "KILLING" => RunState::Running,
        "SUCCEEDED" | "FAILED_SUCCEEDED" | "SKIPPED" => RunState::Succeeded,
        "FAILED" | "EXECUTION_STOPPED" => RunState::Failed,
        "KILLED" | "CANCELLED" => RunState::Cancelled,
        _ => RunState::Unknown,
    }
}

/// Azkaban 时间戳是毫秒数（0 = 未设置），也容忍字符串。
fn timestamp(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::Number(n) => n.as_u64().filter(|n| *n > 0).map(|n| n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn display_json(value: &Value) -> String {
    value
        .as_str()
        .map(str::to_string)
        .unwrap_or_else(|| value.to_string())
}

fn snippet(body: &str) -> String {
    body.chars().take(512).collect()
}

/// session 失效：401/403，或响应里带 session / re-login / login error 字样。
fn session_expired(err: &WorkflowError) -> bool {
    match err {
        WorkflowError::Http { status, .. } => matches!(status, 401 | 403),
        other => {
            let text = other.to_string().to_lowercase();
            text.contains("session") || text.contains("re-login") || text.contains("login error")
        }
    }
}

/// 从 `Set-Cookie` 里取 `azkaban.browser.session.id=<value>`。
fn cookie_session(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| part.trim().strip_prefix(SESSION_COOKIE))
        .filter_map(|rest| rest.strip_prefix('='))
        .map(str::to_string)
        .next()
}
