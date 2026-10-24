// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 跨边界查询：热/冷单侧、跨边界合并去重分页、冷层失败与方言约束。
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};
use serde_json::{Value, json};
use time::{Duration, OffsetDateTime};

use super::{ColdQuery, TieredQuery, TieredTable};
use crate::TieringError;

const HOT_DAYS: u32 = 180;

fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

fn table() -> TieredTable {
    TieredTable {
        name: "events".into(),
        time_col: "ts".into(),
        id_col: "id".into(),
        hot_days: HOT_DAYS,
    }
}

fn row(id: i64) -> Row {
    Row::new(
        vec!["id".into(), "ts".into()],
        vec![json!(id), json!("2025-01-01 00:00:00")],
    )
}

fn ids(rows: &[Row]) -> Vec<i64> {
    rows.iter()
        .map(|r| r.get("id").and_then(Value::as_i64).unwrap())
        .collect()
}

/// 记录 SQL/参数的热层假客户端；`execute`/`query` 一律报错，钉住「只走 query_with」。
struct FakeHot {
    dialect: Dialect,
    rows: Vec<Row>,
    queries: Mutex<Vec<(String, Vec<Value>)>>,
}

impl FakeHot {
    fn new(dialect: Dialect, rows: Vec<Row>) -> Arc<Self> {
        Arc::new(Self {
            dialect,
            rows,
            queries: Mutex::new(Vec::new()),
        })
    }

    fn queries(&self) -> Vec<(String, Vec<Value>)> {
        self.queries.lock().unwrap().clone()
    }
}

#[async_trait]
impl SqlExecutor for FakeHot {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database(
            "tiered query must use query_with".into(),
        ))
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database(
            "tiered query must use query_with".into(),
        ))
    }

    async fn query_with(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>, RdbmsError> {
        self.queries
            .lock()
            .unwrap()
            .push((sql.to_owned(), params.to_vec()));
        Ok(self.rows.clone())
    }

    fn dialect(&self) -> Dialect {
        self.dialect
    }
}

#[async_trait]
impl RdbmsClient for FakeHot {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }
}

#[derive(Debug)]
struct ColdCall {
    table: String,
    from: OffsetDateTime,
    to: OffsetDateTime,
    params: Vec<Value>,
    limit: u64,
    offset: u64,
}

struct FakeCold {
    rows: Vec<Row>,
    error: Option<String>,
    calls: Mutex<Vec<ColdCall>>,
}

impl FakeCold {
    fn new(rows: Vec<Row>) -> Arc<Self> {
        Arc::new(Self {
            rows,
            error: None,
            calls: Mutex::new(Vec::new()),
        })
    }

    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            rows: Vec::new(),
            error: Some(message.into()),
            calls: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<ColdCall> {
        self.calls.lock().unwrap().drain(..).collect()
    }
}

#[async_trait]
impl ColdQuery for FakeCold {
    async fn query_range(
        &self,
        table: &str,
        from: OffsetDateTime,
        to: OffsetDateTime,
        params: &[Value],
        limit: u64,
        offset: u64,
    ) -> Result<Vec<Row>, TieringError> {
        self.calls.lock().unwrap().push(ColdCall {
            table: table.into(),
            from,
            to,
            params: params.to_vec(),
            limit,
            offset,
        });
        match &self.error {
            Some(message) => Err(TieringError::Cold(message.clone())),
            None => Ok(self.rows.clone()),
        }
    }
}

fn query(hot: Arc<FakeHot>, cold: Arc<FakeCold>) -> TieredQuery {
    TieredQuery::new(hot, cold, vec![table()])
}

#[tokio::test]
async fn query_hot_only_range_reads_hot() {
    let hot = FakeHot::new(Dialect::MySql, vec![row(1), row(2)]);
    let cold = FakeCold::new(vec![row(99)]);
    let q = query(hot.clone(), cold.clone());
    let now = now();

    let rows = q
        .query_range("events", now - Duration::days(1), now, &[], 10, 0)
        .await
        .unwrap();

    assert_eq!(ids(&rows), vec![1, 2]);
    assert!(cold.calls().is_empty(), "全热区间不得访问冷层");
    let queries = hot.queries();
    assert_eq!(queries.len(), 1);
    let (sql, params) = &queries[0];
    assert!(sql.contains("SELECT * FROM `events`"), "got: {sql}");
    assert!(sql.contains("`ts` >= ? AND `ts` <= ?"), "got: {sql}");
    assert!(sql.contains("ORDER BY `id`"), "got: {sql}");
    assert_eq!(sql.matches('?').count(), 4, "got: {sql}");
    assert_eq!(params.len(), 4);
    assert_eq!(params[2], json!(10));
    assert_eq!(params[3], json!(0));
}

#[tokio::test]
async fn query_cold_only_range_reads_cold() {
    let hot = FakeHot::new(Dialect::MySql, vec![row(1)]);
    let cold = FakeCold::new(vec![row(7), row(8)]);
    let q = query(hot.clone(), cold.clone());
    let now = now();
    let from = now - Duration::days(400);
    let to = now - Duration::days(200);
    let params = vec![json!({"tenant": 1})];

    let rows = q
        .query_range("events", from, to, &params, 5, 2)
        .await
        .unwrap();

    assert_eq!(ids(&rows), vec![7, 8]);
    assert!(hot.queries().is_empty(), "全冷区间不得访问热层");
    let calls = cold.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].table, "events");
    assert_eq!(calls[0].from, from);
    assert_eq!(calls[0].to, to);
    assert_eq!(calls[0].params, params);
    assert_eq!(calls[0].limit, 5);
    assert_eq!(calls[0].offset, 2);
}

#[tokio::test]
async fn query_boundary_rows_appear_once() {
    let hot = FakeHot::new(Dialect::MySql, vec![row(42)]);
    let cold = FakeCold::new(vec![row(42)]);
    let q = query(hot, cold);
    let now = now();

    let rows = q
        .query_range(
            "events",
            now - Duration::days(400),
            now - Duration::days(1),
            &[],
            10,
            0,
        )
        .await
        .unwrap();

    assert_eq!(ids(&rows), vec![42]);
}

#[tokio::test]
async fn query_merge_sorts_by_pk_and_paginates() {
    let hot = FakeHot::new(Dialect::MySql, vec![row(9), row(3)]);
    let cold = FakeCold::new(vec![row(8), row(1), row(5)]);
    let q = query(hot.clone(), cold.clone());
    let now = now();

    let rows = q
        .query_range(
            "events",
            now - Duration::days(400),
            now - Duration::days(1),
            &[],
            2,
            1,
        )
        .await
        .unwrap();

    // 全局主键序 [1, 3, 5, 8, 9]，跳过 1 行后取 2 行。
    assert_eq!(ids(&rows), vec![3, 5]);
    let queries = hot.queries();
    assert_eq!(queries.len(), 1);
    assert_eq!(queries[0].1[2], json!(3), "热层下推 limit+offset");
    assert_eq!(queries[0].1[3], json!(0), "下推时 offset 归零");
    let calls = cold.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].limit, 3, "冷层同样下推 limit+offset");
    assert_eq!(calls[0].offset, 0);
    assert!(
        calls[0].from < calls[0].to
            && calls[0].to < now - Duration::days(HOT_DAYS as i64) + Duration::minutes(1)
    );
}

#[tokio::test]
async fn query_cold_failure_returns_error_not_empty() {
    let hot = FakeHot::new(Dialect::MySql, vec![row(1)]);
    let cold = FakeCold::failing("clickhouse down");
    let q = query(hot, cold);
    let now = now();

    let cold_only = q
        .query_range(
            "events",
            now - Duration::days(400),
            now - Duration::days(200),
            &[],
            10,
            0,
        )
        .await;
    assert!(
        matches!(cold_only, Err(TieringError::Cold(ref m)) if m.contains("clickhouse down")),
        "got: {cold_only:?}"
    );

    let crossing = q
        .query_range(
            "events",
            now - Duration::days(400),
            now - Duration::days(1),
            &[],
            10,
            0,
        )
        .await;
    assert!(
        matches!(crossing, Err(TieringError::Cold(_))),
        "跨边界冷层失败不得降级为热层半结果: {crossing:?}"
    );
}

#[tokio::test]
async fn query_mssql_is_unsupported() {
    let hot = FakeHot::new(Dialect::Mssql, vec![row(1)]);
    let cold = FakeCold::new(vec![]);
    let q = query(hot.clone(), cold);
    let now = now();

    let err = q
        .query_range("events", now - Duration::days(1), now, &[], 10, 0)
        .await;

    assert!(
        matches!(err, Err(TieringError::UnsupportedDialect)),
        "got: {err:?}"
    );
    assert!(hot.queries().is_empty());
}

#[tokio::test]
async fn query_hot_sql_uses_dialect_placeholders() {
    let hot = FakeHot::new(Dialect::Postgres, vec![row(1)]);
    let cold = FakeCold::new(vec![]);
    let q = query(hot.clone(), cold);
    let now = now();

    q.query_range("events", now - Duration::days(1), now, &[], 10, 0)
        .await
        .unwrap();

    let queries = hot.queries();
    let (sql, params) = &queries[0];
    for p in ["$1", "$2", "$3", "$4"] {
        assert!(sql.contains(p), "missing {p}: {sql}");
    }
    assert!(!sql.contains('?'), "PostgreSQL must not use `?`: {sql}");
    assert!(sql.contains("FROM \"events\""), "got: {sql}");
    assert_eq!(params[2], json!(10));
    assert_eq!(params[3], json!(0));
    assert!(matches!(params[0].as_str(), Some(s) if !s.is_empty()));
}

#[tokio::test]
async fn query_unknown_table_or_bad_ident_is_config_error() {
    let hot = FakeHot::new(Dialect::MySql, vec![]);
    let cold = FakeCold::new(vec![]);
    let q = TieredQuery::new(
        hot.clone(),
        cold,
        vec![TieredTable {
            name: "events; DROP TABLE x".into(),
            time_col: "ts".into(),
            id_col: "id".into(),
            hot_days: HOT_DAYS,
        }],
    );
    let now = now();

    let unknown = q
        .query_range("missing", now - Duration::days(1), now, &[], 10, 0)
        .await;
    assert!(
        matches!(unknown, Err(TieringError::Config(ref m)) if m.contains("missing")),
        "got: {unknown:?}"
    );

    let bad_ident = q
        .query_range(
            "events; DROP TABLE x",
            now - Duration::days(1),
            now,
            &[],
            10,
            0,
        )
        .await;
    assert!(
        matches!(bad_ident, Err(TieringError::Config(_))),
        "got: {bad_ident:?}"
    );
    assert!(hot.queries().is_empty());
}
