// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! Livy REST objects and statement-output parsing.
//!
//! Field names follow <https://livy.apache.org/docs/latest/rest-api.html>
//! (checked 2026-10-29): a Session has `id`/`state`; a Statement has
//! `id`/`state`/`output`; `output.data` maps a MIME type to its payload.

use ecat_data::{RdbmsError, Row};
use serde::Deserialize;

/// `POST /sessions` and `POST /sessions/{id}/statements` only need the id.
#[derive(Debug, Deserialize)]
pub(crate) struct EntityId {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionState {
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Statement {
    pub id: i64,
    pub state: String,
    #[serde(default)]
    pub output: Option<Output>,
}

impl Statement {
    /// `Ok(None)` = still running, `Ok(Some(rows))` = finished, `Err` = failed.
    ///
    /// Livy statement states are `waiting`, `running`, `available`, `error`,
    /// `cancelling`, `cancelled`; anything else is treated as a protocol error
    /// rather than polled forever.
    pub(crate) fn phase(self) -> Result<Option<Vec<Row>>, RdbmsError> {
        match self.state.as_str() {
            "waiting" | "running" => Ok(None),
            "available" => match self.output {
                Some(output) => output.rows().map(Some),
                None => Err(RdbmsError::Database(format!(
                    "spark: statement {} is available without output",
                    self.id
                ))),
            },
            "error" => Err(match self.output {
                Some(output) => output.failure(),
                None => RdbmsError::Database(format!(
                    "spark: statement {} failed without output",
                    self.id
                )),
            }),
            "cancelling" | "cancelled" => Err(RdbmsError::Database(format!(
                "spark: statement {} was cancelled",
                self.id
            ))),
            other => Err(RdbmsError::Database(format!(
                "spark: statement {} entered unexpected state {other:?}",
                self.id
            ))),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Output {
    pub status: String,
    #[serde(default)]
    pub evalue: Option<String>,
    #[serde(default)]
    pub traceback: Option<Vec<String>>,
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

impl Output {
    fn rows(&self) -> Result<Vec<Row>, RdbmsError> {
        match self.status.as_str() {
            "ok" => rows_from_data(self.data.as_ref()),
            "error" => Err(self.failure()),
            other => Err(RdbmsError::Database(format!(
                "spark: statement output status {other:?}"
            ))),
        }
    }

    fn failure(&self) -> RdbmsError {
        RdbmsError::Database(format!("spark: statement failed: {}", self.error_text()))
    }

    /// Livy puts the message in `evalue`; `traceback` is the fallback.
    fn error_text(&self) -> String {
        if let Some(e) = self.evalue.as_deref().filter(|e| !e.trim().is_empty()) {
            return e.to_string();
        }
        if let Some(t) = &self.traceback {
            let joined = t.join("\n");
            if !joined.is_empty() {
                return joined;
            }
        }
        "unknown error".to_string()
    }
}

/// Rows live under `application/json` (a JSON-encoded array of row objects,
/// or the array itself) or under `text/plain`. An empty `data` object is what
/// DDL returns — no rows, not an error.
fn rows_from_data(data: Option<&serde_json::Value>) -> Result<Vec<Row>, RdbmsError> {
    let data = match data {
        None => return Ok(Vec::new()),
        Some(data) => data.as_object().ok_or_else(|| {
            RdbmsError::Database(format!("spark: output.data is not an object: {data}"))
        })?,
    };
    if let Some(value) = data.get("application/json") {
        return json_rows(value);
    }
    if let Some(text) = data.get("text/plain").and_then(|v| v.as_str()) {
        return text_rows(text);
    }
    if data.is_empty() {
        return Ok(Vec::new());
    }
    Err(RdbmsError::Database(format!(
        "spark: unsupported output MIME types: {:?}",
        data.keys().collect::<Vec<_>>()
    )))
}

fn json_rows(value: &serde_json::Value) -> Result<Vec<Row>, RdbmsError> {
    let parsed;
    let value = match value {
        serde_json::Value::String(text) => {
            parsed = serde_json::from_str::<serde_json::Value>(text).map_err(|e| {
                RdbmsError::Database(format!("spark: output is not valid JSON: {e}"))
            })?;
            &parsed
        }
        other => other,
    };
    let rows = value.as_array().ok_or_else(|| {
        RdbmsError::Database(format!("spark: expected a JSON array of rows, got {value}"))
    })?;
    rows.iter()
        .map(|row| {
            let object = row.as_object().ok_or_else(|| {
                RdbmsError::Database(format!("spark: result row is not an object: {row}"))
            })?;
            Ok(Row::new(
                object.keys().cloned().collect(),
                object.values().cloned().collect(),
            ))
        })
        .collect()
}

/// `text/plain` is either the same JSON payload serialised as text (some Livy
/// builds) or plain lines; a plain line becomes one single-column row.
fn text_rows(text: &str) -> Result<Vec<Row>, RdbmsError> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text)
        && value.is_array()
    {
        return json_rows(&value);
    }
    Ok(text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            Row::new(
                vec!["value".to_string()],
                vec![serde_json::Value::String(line.to_string())],
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn output(value: serde_json::Value) -> Output {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn json_string_payload_becomes_rows() {
        let rows = output(json!({
            "status": "ok",
            "data": {"application/json": r#"[{"id":1,"name":"alice"},{"id":2,"name":"bob"}]"#}
        }))
        .rows()
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&json!(1)));
        assert_eq!(rows[1].get("name"), Some(&json!("bob")));
    }

    #[test]
    fn raw_json_array_payload_becomes_rows() {
        let rows = output(json!({
            "status": "ok",
            "data": {"application/json": [{"id": 7}]}
        }))
        .rows()
        .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("id"), Some(&json!(7)));
    }

    #[test]
    fn text_plain_lines_become_single_column_rows() {
        let rows = output(json!({"status": "ok", "data": {"text/plain": "alpha\n\nbeta"}}))
            .rows()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("value"), Some(&json!("alpha")));
        assert_eq!(rows[1].get("value"), Some(&json!("beta")));
    }

    #[test]
    fn text_plain_json_array_reuses_json_parsing() {
        let rows = output(json!({
            "status": "ok",
            "data": {"text/plain": r#"[{"v": 1}]"#}
        }))
        .rows()
        .unwrap();
        assert_eq!(rows[0].get("v"), Some(&json!(1)));
    }

    #[test]
    fn empty_data_is_no_rows() {
        assert!(output(json!({"status": "ok"})).rows().unwrap().is_empty());
        assert!(
            output(json!({"status": "ok", "data": {}}))
                .rows()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn error_status_uses_evalue() {
        let err = output(json!({"status": "error", "evalue": "Table not found"}))
            .rows()
            .unwrap_err();
        assert!(err.to_string().contains("Table not found"), "got: {err}");
    }

    #[test]
    fn unknown_mime_type_is_an_error() {
        let err = output(json!({"status": "ok", "data": {"image/png": "..."}}))
            .rows()
            .unwrap_err();
        assert!(
            err.to_string().contains("unsupported output MIME"),
            "got: {err}"
        );
    }

    #[test]
    fn non_object_row_is_an_error() {
        let err = output(json!({
            "status": "ok",
            "data": {"application/json": "[[1,2]]"}
        }))
        .rows()
        .unwrap_err();
        assert!(err.to_string().contains("not an object"), "got: {err}");
    }

    #[test]
    fn statement_phase_classifies_states() {
        let running: Statement =
            serde_json::from_value(json!({"id": 1, "state": "running"})).unwrap();
        assert!(running.phase().unwrap().is_none());
        let done: Statement = serde_json::from_value(json!({
            "id": 2, "state": "available",
            "output": {"status": "ok", "data": {}}
        }))
        .unwrap();
        assert!(done.phase().unwrap().unwrap().is_empty());
        let cancelled: Statement =
            serde_json::from_value(json!({"id": 3, "state": "cancelled"})).unwrap();
        assert!(cancelled.phase().is_err());
    }
}
