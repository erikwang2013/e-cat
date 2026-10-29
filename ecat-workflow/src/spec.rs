// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 与具体引擎无关的工作流数据模型。

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 一次工作流定义（`WorkflowClient::create` 的入参）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSpec {
    pub name: String,
    /// 引擎相关的定义原文（DAG 文件内容、节点定义等），适配器自行解释。
    #[serde(default)]
    pub definition: Value,
    /// 触发参数；`Null` = 无参数。
    #[serde(default)]
    pub params: Value,
    /// cron 表达式；`None` = 仅手动触发。
    #[serde(default)]
    pub schedule: Option<String>,
}

/// 引擎侧工作流 id（DAG id、project/process 组合等，由适配器编码成字符串）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowId(pub String);

/// 引擎侧运行实例 id。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunId(pub String);

/// 统一的运行状态。适配器负责把引擎私有状态映射到这几档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RunState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

/// 一次运行的状态快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStatus {
    pub run_id: RunId,
    pub workflow_id: WorkflowId,
    pub state: RunState,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub message: Option<String>,
    /// 引擎原始响应，供调用方读取适配器未映射的字段。
    pub raw: Value,
}

/// `list_runs` 的过滤条件。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunFilter {
    pub workflow_id: Option<WorkflowId>,
    pub state: Option<RunState>,
    pub limit: Option<u32>,
}

/// 引擎能力声明：适配器必须如实标注，调用方据此决定是否调用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowCapabilities {
    pub create: bool,
    pub trigger: bool,
    pub cancel: bool,
    pub list_runs: bool,
}
