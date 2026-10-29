// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 无 I/O 的映射与解析辅助。

use ecat_workflow::{RunState, WorkflowError};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::macros::format_description;

/// DS `WorkflowExecutionStatus` -> 统一状态。
///
/// `PAUSE` 没有对应变体，归入 [`RunState::Unknown`]；其余未列出的状态同理。
pub(crate) fn map_state(state: &str) -> RunState {
    match state {
        "SUBMITTED_SUCCESS" | "DELAY_EXECUTION" | "SERIAL_WAIT" | "READY_BLOCK" | "BLOCK"
        | "WAIT_TO_RUN" => RunState::Queued,
        // READY_STOP / READY_PAUSE 是停止/暂停命令已下发、尚未生效。
        "RUNNING_EXECUTION" | "READY_PAUSE" | "READY_STOP" => RunState::Running,
        "SUCCESS" => RunState::Succeeded,
        "FAILURE" => RunState::Failed,
        "STOP" => RunState::Cancelled,
        _ => RunState::Unknown,
    }
}

/// 统一状态 -> DS `stateType` 过滤值（DS 一次只能过滤一个状态）。
pub(crate) fn engine_state(state: RunState) -> Result<&'static str, WorkflowError> {
    match state {
        RunState::Queued => Ok("SUBMITTED_SUCCESS"),
        RunState::Running => Ok("RUNNING_EXECUTION"),
        RunState::Succeeded => Ok("SUCCESS"),
        RunState::Failed => Ok("FAILURE"),
        RunState::Cancelled => Ok("STOP"),
        RunState::Unknown => Err(WorkflowError::Config(
            "cannot filter DolphinScheduler runs by RunState::Unknown".to_string(),
        )),
    }
}

/// 取 JSON 里的整数（DS 的 code / id 序列化成数字）。
pub(crate) fn json_i64(value: &Value, what: &str) -> Result<i64, WorkflowError> {
    match value {
        Value::Number(n) => n
            .as_i64()
            .ok_or_else(|| WorkflowError::Parse(format!("{what} is not an integer"))),
        Value::String(s) => s
            .parse()
            .map_err(|_| WorkflowError::Parse(format!("{what} is not an integer"))),
        _ => Err(WorkflowError::Parse(format!("{what} is missing"))),
    }
}

/// 解析调用方传入的十进制字符串。
pub(crate) fn parse_i64(raw: &str, what: &str) -> Result<i64, WorkflowError> {
    raw.parse().map_err(|_| {
        WorkflowError::Config(format!("{what} must be a decimal integer, got {raw:?}"))
    })
}

/// create 的必填 JSON 字段。
pub(crate) fn required<'a>(
    definition: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Value, WorkflowError> {
    definition
        .get(key)
        .ok_or_else(|| WorkflowError::Config(format!("DolphinScheduler definition needs `{key}`")))
}

/// DS 的 JSON 字段值是字符串；调用方给数组/对象时重新序列化。
pub(crate) fn form_json(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// start 接口的 `scheduleTime`（DS 用 `yyyy-MM-dd HH:mm:ss`）。
pub(crate) fn now_schedule_time() -> String {
    OffsetDateTime::now_utc()
        .format(&format_description!(
            "[year]-[month]-[day] [hour]:[minute]:[second]"
        ))
        .unwrap_or_default()
}
