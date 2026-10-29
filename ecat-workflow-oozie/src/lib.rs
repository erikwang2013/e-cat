// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Apache Oozie（REST v2）适配器。
//!
//! - 认证：`WorkflowHttpConfig` 的 Basic auth（`username` / `password`）。
//!   Oozie 自带 Kerberos/SPEGO 认证不在本适配器支持范围内。
//! - `definition` 是 Oozie 的 job configuration XML（字符串或
//!   `{"xml": "..."}`，通常含 `oozie.wf.application.path`）。`params`
//!   以 `<property>` 形式注入到 `</configuration>` 之前。
//! - `create` 提交（PREP）作业；`trigger` 对同一 job id 执行
//!   `action=start`。触发参数不支持（提交时已固化），非空会返回
//!   `Unsupported`。`schedule` 忽略。
//!
//! 所有出站调用（含读 body）都经过 [`WorkflowHttp::guarded`]。

use ecat_workflow::{
    RunFilter, RunId, RunState, RunStatus, WorkflowCapabilities, WorkflowClient, WorkflowError,
    WorkflowHttp, WorkflowHttpConfig, WorkflowId, WorkflowSpec,
};
use quick_xml::Reader;
use quick_xml::escape::escape;
use quick_xml::events::Event;
use reqwest::Method;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use serde_json::Value;

/// Oozie 适配器配置。
#[derive(Debug, Clone, Deserialize)]
pub struct OozieConfig {
    #[serde(flatten)]
    pub http: WorkflowHttpConfig,
}

/// Oozie REST v2 客户端。无状态（Basic auth 由 `WorkflowHttp` 注入）。
pub struct OozieClient {
    http: WorkflowHttp,
}

impl OozieClient {
    /// 构造客户端。同步、不做网络 I/O。
    pub fn from_config(cfg: OozieConfig) -> Result<Self, WorkflowError> {
        Ok(Self {
            http: WorkflowHttp::new(cfg.http)?,
        })
    }

    /// 发请求并解析 JSON。body 读取与状态码检查都在 `guarded` 内。
    async fn json<F>(&self, build: F) -> Result<Value, WorkflowError>
    where
        F: Fn() -> reqwest::RequestBuilder + Sync,
    {
        self.http
            .guarded(async {
                let resp = build()
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
                serde_json::from_str(&body).map_err(|e| {
                    WorkflowError::Parse(format!("oozie: invalid JSON: {e}: {}", snippet(&body)))
                })
            })
            .await
    }

    /// start / kill 返回 200 且通常没有 body；只做状态码检查。
    async fn ok<F>(&self, build: F) -> Result<(), WorkflowError>
    where
        F: Fn() -> reqwest::RequestBuilder + Sync,
    {
        self.http
            .guarded(async {
                let resp = build()
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
                Ok(())
            })
            .await
    }
}

#[async_trait::async_trait]
impl WorkflowClient for OozieClient {
    fn engine(&self) -> &'static str {
        "oozie"
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
        let xml = definition_xml(&spec.definition)?;
        let xml = inject_params(&xml, &spec.params)?;
        let value = self
            .json(|| {
                self.http
                    .request(Method::POST, "/oozie/v2/jobs")
                    .header(CONTENT_TYPE, "application/xml")
                    .body(xml.clone())
            })
            .await?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkflowError::Parse("oozie: submit response missing id".into()))?;
        Ok(WorkflowId(id.to_string()))
    }

    async fn trigger(&self, workflow: &WorkflowId, params: Value) -> Result<RunId, WorkflowError> {
        if !params.is_null() {
            return Err(WorkflowError::Unsupported(
                "oozie: trigger params are not supported; put them in the definition",
            ));
        }
        let url = job_url(&workflow.0)?;
        self.ok(|| {
            self.http
                .request(Method::PUT, &url)
                .query(&[("action", "start")])
        })
        .await?;
        Ok(RunId(workflow.0.clone()))
    }

    async fn status(&self, run: &RunId) -> Result<RunStatus, WorkflowError> {
        let url = job_url(&run.0)?;
        let value = self
            .json(|| {
                self.http
                    .request(Method::GET, &url)
                    .query(&[("show", "status")])
            })
            .await?;
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(&run.0)
            .to_string();
        Ok(RunStatus {
            run_id: run.clone(),
            workflow_id: WorkflowId(id),
            state: map_state(
                value
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            started_at: time(&value, "startTime"),
            finished_at: time(&value, "endTime"),
            message: value
                .get("errorMessage")
                .and_then(Value::as_str)
                .map(str::to_string),
            raw: value,
        })
    }

    async fn cancel(&self, run: &RunId) -> Result<(), WorkflowError> {
        let url = job_url(&run.0)?;
        self.ok(|| {
            self.http
                .request(Method::PUT, &url)
                .query(&[("action", "kill")])
        })
        .await?;
        Ok(())
    }

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<RunStatus>, WorkflowError> {
        let len = filter.limit.unwrap_or(50).max(1).to_string();
        let id_filter = filter
            .workflow_id
            .as_ref()
            .map(|workflow| format!("id={}", workflow.0));
        let value = self
            .json(|| {
                let mut query = vec![
                    ("jobtype".to_string(), "wf".to_string()),
                    ("len".to_string(), len.clone()),
                ];
                if let Some(filter) = &id_filter {
                    query.push(("filter".to_string(), filter.clone()));
                }
                self.http
                    .request(Method::GET, "/oozie/v2/jobs")
                    .query(&query)
            })
            .await?;
        let jobs = value
            .get("workflows")
            .and_then(Value::as_array)
            .ok_or_else(|| WorkflowError::Parse("oozie: jobs response missing workflows".into()))?;
        let mut runs = Vec::with_capacity(jobs.len());
        for job in jobs {
            let id = job
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| WorkflowError::Parse("oozie: workflow entry missing id".into()))?;
            runs.push(RunStatus {
                run_id: RunId(id.to_string()),
                workflow_id: WorkflowId(id.to_string()),
                state: map_state(
                    job.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                started_at: time(job, "startTime"),
                finished_at: time(job, "endTime"),
                message: job
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                raw: job.clone(),
            });
        }
        if let Some(state) = filter.state {
            runs.retain(|run| run.state == state);
        }
        if let Some(limit) = filter.limit {
            runs.truncate(limit as usize);
        }
        Ok(runs)
    }
}

/// `definition` 为 XML 字符串，或 `{"xml": "..."}`。
fn definition_xml(definition: &Value) -> Result<String, WorkflowError> {
    match definition {
        Value::String(xml) => Ok(xml.clone()),
        Value::Object(map) => map
            .get("xml")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| {
                WorkflowError::Config("oozie: definition must be XML or {\"xml\": \"...\"}".into())
            }),
        _ => Err(WorkflowError::Config(
            "oozie: definition must be XML or {\"xml\": \"...\"}".into(),
        )),
    }
}

/// 在 `</configuration>` 前注入 `<property>`；params 为 null 时原样返回。
///
/// 用 quick-xml 走完整个文档：既能拿到闭合标签的精确字节位置，
/// 也顺带校验 XML（损坏 -> Parse）。没有 `<configuration>` -> Config。
fn inject_params(xml: &str, params: &Value) -> Result<String, WorkflowError> {
    let properties = match params {
        Value::Null => Vec::new(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                let value = value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string());
                format!(
                    "<property><name>{}</name><value>{}</value></property>",
                    escape(key),
                    escape(&value)
                )
            })
            .collect::<Vec<_>>(),
        _ => {
            return Err(WorkflowError::Unsupported(
                "oozie: params must be a JSON object",
            ));
        }
    };
    let mut reader = Reader::from_str(xml);
    let mut close_at = None;
    loop {
        match reader.read_event() {
            Ok(Event::End(end)) if close_at.is_none() => {
                if end.name().as_ref() == b"configuration" {
                    close_at = Some(reader.buffer_position() as usize);
                }
            }
            Ok(Event::Empty(empty))
                if close_at.is_none() && empty.name().as_ref() == b"configuration" =>
            {
                return Err(WorkflowError::Config(
                    "oozie: empty <configuration/> has nowhere to inject params".into(),
                ));
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => {
                return Err(WorkflowError::Parse(format!(
                    "oozie: invalid definition XML at byte {}: {e}",
                    reader.buffer_position()
                )));
            }
        }
    }
    let close_at = close_at.ok_or_else(|| {
        WorkflowError::Config("oozie: definition XML has no <configuration> element".into())
    })?;
    // End 事件后 buffer_position 指向闭合标签 `>` 之后；往前找它的 `<`。
    let open_at = xml[..close_at]
        .rfind('<')
        .ok_or_else(|| WorkflowError::Parse("oozie: cannot locate </configuration>".into()))?;
    Ok(format!(
        "{}{}{}",
        &xml[..open_at],
        properties.concat(),
        &xml[open_at..]
    ))
}

/// job id → `/oozie/v2/job/{id}`；只放行 Oozie id 合法字符，防路径注入。
fn job_url(id: &str) -> Result<String, WorkflowError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(WorkflowError::Config(format!(
            "oozie: invalid job id {id:?}"
        )));
    }
    Ok(format!("/oozie/v2/job/{id}"))
}

fn map_state(status: &str) -> RunState {
    match status {
        "PREP" | "PREPARED" => RunState::Queued,
        "RUNNING" | "SUSPENDED" => RunState::Running,
        "SUCCEEDED" => RunState::Succeeded,
        "FAILED" => RunState::Failed,
        "KILLED" => RunState::Cancelled,
        _ => RunState::Unknown,
    }
}

fn time(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn snippet(body: &str) -> String {
    body.chars().take(512).collect()
}
