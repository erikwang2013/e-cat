// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! ClickHouse sink 单测：SQL 值转义、稳定校验和、抽样回读与元数据表。
//!
//! `write_chunk`/`verify_chunk` 走真实 `ClickhouseClient`，但后端是本地 axum 假端点：
//! 记录收到的 SQL，并按 SQL 里的表名返回预设的 JSONEachRow 响应。
use super::*;
use crate::sink::render_sql_value;
use ecat_data::SqlExecutor;
use ecat_data_clickhouse::ClickhouseClient;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const ID_COL: &str = "id";

fn row(id: i64, payload: &str) -> Row {
    Row::new(
        vec![ID_COL.into(), "payload".into()],
        vec![json!(id), json!(payload)],
    )
}

fn rows(ids: &[i64]) -> Vec<Row> {
    ids.iter().map(|id| row(*id, &format!("p{id}"))).collect()
}

/// 预设响应 + 收到的 SQL。
#[derive(Default)]
struct FakeState {
    sql: Vec<String>,
    metadata: String,
    cold: String,
}

struct Fake {
    url: String,
    state: Arc<Mutex<FakeState>>,
}

impl Fake {
    async fn spawn() -> Self {
        let state = Arc::new(Mutex::new(FakeState::default()));
        let app_state = Arc::clone(&state);
        let app =
            axum::Router::new().fallback(move |req: axum::http::Request<axum::body::Body>| {
                let state = Arc::clone(&app_state);
                async move {
                    let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                        .await
                        .unwrap_or_default();
                    let sql = String::from_utf8_lossy(&body).into_owned();
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    let response = if sql.contains("FROM ecat_tier_checksums") {
                        state.metadata.clone()
                    } else if sql.contains("SELECT *") {
                        state.cold.clone()
                    } else {
                        String::new()
                    };
                    state.sql.push(sql);
                    axum::response::Response::new(axum::body::Body::from(response))
                }
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}"),
            state,
        }
    }

    fn sink(&self, sample: Option<f32>) -> ClickhouseSink {
        ClickhouseSink::new(
            ClickhouseClient::new(self.url.as_str(), "default"),
            vec![ClickhouseTable {
                name: "events".into(),
                id_col: ID_COL.into(),
            }],
            sample,
        )
        .unwrap()
    }

    fn set_metadata(&self, digest: &ChunkDigest) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .metadata = json!({
            "checksum": digest.checksum,
            "sample_checksum": digest.sample_checksum,
            "sample_rows": digest.sample_rows,
            "low_id": digest.low_id,
            "high_id": digest.high_id,
        })
        .to_string();
    }

    fn set_cold(&self, rows: &[Row]) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).cold = cold_lines(rows);
    }

    /// ClickHouse JSONEachRow 默认把 64 位整数写成十进制字符串
    /// （`output_format_json_quote_64bit_integers=1`）。
    fn set_metadata_quoted(&self, digest: &ChunkDigest) {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .metadata = json!({
            "checksum": digest.checksum,
            "sample_checksum": digest.sample_checksum,
            "sample_rows": digest.sample_rows.to_string(),
            "low_id": digest.low_id.to_string(),
            "high_id": digest.high_id.to_string(),
        })
        .to_string();
    }

    fn set_cold_quoted_ids(&self, rows: &[Row]) {
        let mut out = String::new();
        for row in rows {
            let mut map = serde_json::Map::new();
            for (col, value) in row.iter() {
                let value = match (col, value.as_i64()) {
                    (ID_COL, Some(id)) => Value::String(id.to_string()),
                    _ => value.clone(),
                };
                map.insert(col.to_string(), value);
            }
            out.push_str(&serde_json::to_string(&map).unwrap());
            out.push('\n');
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).cold = out;
    }

    fn sql(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sql
            .clone()
    }
}

/// 与 `ClickhouseClient::query` 一致的 JSONEachRow 响应（键序无关，校验前会排序）。
fn cold_lines(rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows {
        let mut map = serde_json::Map::new();
        for (col, value) in row.iter() {
            map.insert(col.to_string(), value.clone());
        }
        out.push_str(&serde_json::to_string(&map).unwrap());
        out.push('\n');
    }
    out
}

#[test]
fn clickhouse_sql_value_escapes_quotes_and_backslashes() {
    assert_eq!(render_sql_value(&json!("O'Brien")).unwrap(), r"'O\'Brien'");
    assert_eq!(
        render_sql_value(&json!(r"C:\tmp\x")).unwrap(),
        r"'C:\\tmp\\x'"
    );
    assert_eq!(render_sql_value(&json!(["a'b"])).unwrap(), r#"'["a\'b"]'"#);
    assert_eq!(
        render_sql_value(&json!({"k": r"a\b"})).unwrap(),
        r#"'{"k":"a\\\\b"}'"#
    );
}

#[test]
fn clickhouse_sql_value_renders_null_and_numbers() {
    assert_eq!(render_sql_value(&Value::Null).unwrap(), "NULL");
    assert_eq!(render_sql_value(&json!(true)).unwrap(), "true");
    assert_eq!(render_sql_value(&json!(false)).unwrap(), "false");
    assert_eq!(render_sql_value(&json!(42)).unwrap(), "42");
    assert_eq!(render_sql_value(&json!(-1.5)).unwrap(), "-1.5");
}

#[test]
fn clickhouse_validate_ident_rejects_semicolon_and_dash() {
    assert!(crate::validate_ident("events").is_ok());
    assert!(crate::validate_ident("events_2026").is_ok());
    for bad in [
        "events;DROP TABLE x",
        "events-2026",
        "1events",
        "",
        "events`",
    ] {
        assert!(
            matches!(crate::validate_ident(bad), Err(TieringError::Config(_))),
            "{bad}"
        );
    }
}

#[test]
fn clickhouse_sink_rejects_invalid_config() {
    let client = || ClickhouseClient::new("http://127.0.0.1:1", "default");
    let table = |name: &str, id_col: &str| {
        vec![ClickhouseTable {
            name: name.into(),
            id_col: id_col.into(),
        }]
    };
    assert!(ClickhouseSink::new(client(), table("bad;name", ID_COL), None).is_err());
    assert!(ClickhouseSink::new(client(), table("events", "id-1"), None).is_err());
    assert!(ClickhouseSink::new(client(), table("events", ID_COL), Some(1.5)).is_err());
    assert!(ClickhouseSink::new(client(), table("events", ID_COL), Some(f32::NAN)).is_err());
    assert!(ClickhouseSink::new(client(), table("events", ID_COL), Some(-0.1)).is_err());
}

#[tokio::test]
async fn clickhouse_write_chunk_returns_stable_sha256() {
    let fake = Fake::spawn().await;
    let sink = fake.sink(None);
    let first = sink
        .write_chunk("events", "p202601", 0, &rows(&[3, 1, 2]))
        .await
        .unwrap();
    let second = sink
        .write_chunk("events", "p202601", 0, &rows(&[2, 3, 1]))
        .await
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first.chars().all(|c| c.is_ascii_hexdigit()));

    let sql = fake.sql();
    assert!(
        sql.iter()
            .any(|s| s.starts_with("INSERT INTO `events` VALUES (")),
        "{sql:?}"
    );
    assert!(
        sql.iter()
            .any(|s| s.starts_with("INSERT INTO ecat_tier_checksums ")),
        "{sql:?}"
    );
    assert!(
        sql.iter()
            .any(|s| s.starts_with("CREATE TABLE IF NOT EXISTS ecat_tier_checksums")),
        "{sql:?}"
    );

    // 内容变则校验和变。
    let changed = chunk_digest(&[row(1, "other")], ID_COL, None).unwrap();
    assert_ne!(changed.checksum, first);
}

#[test]
fn clickhouse_cold_rows_preserve_value_wire_format() {
    let record = Row::new(
        vec!["id".into(), "ts".into(), "payload".into(), "day".into()],
        vec![
            json!(1),
            json!("2026-01-02T03:04:05Z"),
            json!("aGVsbG8="),
            json!("2026-01-02"),
        ],
    );
    let line = canonical_ndjson(&[&record], ID_COL).unwrap();
    assert_eq!(
        line,
        "{\"day\":\"2026-01-02\",\"id\":1,\"payload\":\"aGVsbG8=\",\"ts\":\"2026-01-02T03:04:05Z\"}\n"
    );
    let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
    assert_eq!(
        parsed,
        json!({"id": 1, "ts": "2026-01-02T03:04:05Z", "payload": "aGVsbG8=", "day": "2026-01-02"})
    );
    // 模拟 ClickHouse JSONEachRow 往返：键按字典序重排后再规范化必须逐字节一致。
    let object = parsed.as_object().unwrap();
    let roundtrip = Row::new(
        object.keys().cloned().collect(),
        object.values().cloned().collect(),
    );
    assert_eq!(canonical_ndjson(&[&roundtrip], ID_COL).unwrap(), line);
    // 64 位整数被引用的服务器：主键 "1" 必须与 1 规范化成同一行。
    let quoted = Row::new(
        vec!["id".into(), "ts".into(), "payload".into(), "day".into()],
        vec![
            json!("1"),
            json!("2026-01-02T03:04:05Z"),
            json!("aGVsbG8="),
            json!("2026-01-02"),
        ],
    );
    assert_eq!(canonical_ndjson(&[&quoted], ID_COL).unwrap(), line);
}

#[test]
fn clickhouse_sample_checksum_is_stable() {
    let a = chunk_digest(&rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), ID_COL, Some(0.2)).unwrap();
    let b = chunk_digest(&rows(&[10, 5, 1, 7, 3, 9, 2, 8, 4, 6]), ID_COL, Some(0.2)).unwrap();
    assert_eq!(a.checksum, b.checksum);
    assert_eq!(a.sample_checksum, b.sample_checksum);
    assert_eq!(a.sample_rows, 2); // step = ceil(1/0.2) = 5 → id 5、10
    assert_eq!(a.low_id, 1);
    assert_eq!(a.high_id, 10);
    assert_ne!(a.sample_checksum, a.checksum);

    // 全量抽样：样本即全量。
    let all = chunk_digest(&rows(&[1, 2, 3]), ID_COL, Some(1.0)).unwrap();
    assert_eq!(all.sample_rows, 3);
    assert_eq!(all.sample_checksum, all.checksum);

    // 关闭抽样：不产生样本。
    for off in [None, Some(0.0)] {
        let off = chunk_digest(&rows(&[1, 2, 3]), ID_COL, off).unwrap();
        assert_eq!(off.sample_rows, 0);
        assert!(off.sample_checksum.is_empty());
    }

    assert!(chunk_digest(&[], ID_COL, None).is_err());
}

#[tokio::test]
async fn clickhouse_verify_chunk_detects_sample_mismatch() {
    let fake = Fake::spawn().await;
    let sink = fake.sink(Some(0.2));
    let chunk = rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let digest = chunk_digest(&chunk, ID_COL, Some(0.2)).unwrap();
    fake.set_metadata(&digest);
    fake.set_cold(&chunk);
    // 已知阳性对照：未篡改的冷行必须通过，否则下面返回 false 不能说明是抽样发现的。
    assert!(
        sink.verify_chunk("events", "p202601", 0, &digest.checksum)
            .await
            .unwrap()
    );

    let mut tampered = rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    tampered[4] = row(5, "tampered"); // id=5 是被抽样行
    fake.set_cold(&tampered);
    assert!(
        !sink
            .verify_chunk("events", "p202601", 0, &digest.checksum)
            .await
            .unwrap()
    );

    // 不在样本里的行被篡改：抽样校验不覆盖它（1% 抽样的语义边界）。
    let mut unsampled = rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    unsampled[0] = row(1, "tampered");
    fake.set_cold(&unsampled);
    assert!(
        sink.verify_chunk("events", "p202601", 0, &digest.checksum)
            .await
            .unwrap()
    );
}

/// 真实 ClickHouse 默认 `output_format_json_quote_64bit_integers=1`：
/// 元数据里的 UInt64/Int64 与冷表的 id 都会是十进制字符串，抽样校验仍须通过。
#[tokio::test]
async fn clickhouse_verify_chunk_accepts_quoted_64bit_fields() {
    let fake = Fake::spawn().await;
    let sink = fake.sink(Some(0.2));
    let chunk = rows(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
    let digest = chunk_digest(&chunk, ID_COL, Some(0.2)).unwrap();
    fake.set_metadata_quoted(&digest);
    fake.set_cold_quoted_ids(&chunk);
    assert!(
        sink.verify_chunk("events", "p202601", 0, &digest.checksum)
            .await
            .unwrap()
    );
    // 抽样范围读必须去重（FINAL）且请求服务端不引用 64 位整数。
    let sql = fake.sql();
    let cold_sql = sql.iter().find(|s| s.contains("SELECT *")).unwrap();
    assert!(cold_sql.contains("FINAL"), "{cold_sql}");
    assert!(
        cold_sql.contains("output_format_json_quote_64bit_integers = 0"),
        "{cold_sql}"
    );
}

#[tokio::test]
async fn clickhouse_verify_chunk_skips_sample_read_when_disabled() {
    let fake = Fake::spawn().await;
    let digest = chunk_digest(&rows(&[1, 2, 3]), ID_COL, None).unwrap();
    assert!(digest.sample_checksum.is_empty());
    fake.set_metadata(&digest);
    for sample in [None, Some(0.0)] {
        let sink = fake.sink(sample);
        assert!(
            sink.verify_chunk("events", "p202601", 0, &digest.checksum)
                .await
                .unwrap()
        );
    }
    let sql = fake.sql();
    assert!(
        sql.iter().any(|s| s.contains("FROM ecat_tier_checksums")),
        "metadata SELECT must have been issued: {sql:?}"
    );
    assert!(!sql.iter().any(|s| s.contains("SELECT *")), "{sql:?}");
}

#[tokio::test]
async fn clickhouse_verify_chunk_returns_false_without_metadata() {
    let fake = Fake::spawn().await;
    let sink = fake.sink(None);
    assert!(
        !sink
            .verify_chunk("events", "p202601", 0, "deadbeef")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn clickhouse_health_check_runs_select_1() {
    let fake = Fake::spawn().await;
    fake.sink(None).health_check().await.unwrap();
    assert!(fake.sql().iter().any(|s| s.trim() == "SELECT 1"));
}

/// live 用例：未设置 `ECAT_TEST_CLICKHOUSE_URL` 时跳过（不是失败）。
#[tokio::test]
async fn clickhouse_live_write_verify_roundtrip() {
    let Ok(url) = std::env::var("ECAT_TEST_CLICKHOUSE_URL") else {
        eprintln!("skip clickhouse_live_write_verify_roundtrip: ECAT_TEST_CLICKHOUSE_URL unset");
        return;
    };
    let db = std::env::var("ECAT_TEST_CLICKHOUSE_DB").unwrap_or_else(|_| "default".into());
    let table = "ecat_tiering_live_test";
    let client = ClickhouseClient::new(&url, &db);
    let cleanup = ClickhouseClient::new(&url, &db);
    client
        .execute(&format!("DROP TABLE IF EXISTS `{table}`"))
        .await
        .unwrap();
    client
        .execute(&format!(
            "CREATE TABLE `{table}` (`id` Int64, `payload` String) ENGINE = MergeTree ORDER BY id"
        ))
        .await
        .unwrap();

    let sink = ClickhouseSink::new(
        client,
        vec![ClickhouseTable {
            name: table.into(),
            id_col: ID_COL.into(),
        }],
        Some(0.5),
    )
    .unwrap();
    sink.health_check().await.unwrap();
    let chunk = rows(&[1, 2, 3]);
    let checksum = sink.write_chunk(table, "p1", 0, &chunk).await.unwrap();
    assert!(sink.verify_chunk(table, "p1", 0, &checksum).await.unwrap());
    assert!(!sink.verify_chunk(table, "p1", 0, "deadbeef").await.unwrap());
    cleanup
        .execute(&format!("DROP TABLE IF EXISTS `{table}`"))
        .await
        .unwrap();
}
