//! 编译期公开 API 兼容性冒烟测试：只引用签名，不执行任何 I/O。
//!
//! 覆盖项（参数与返回类型都钉住）：`Row::new` 的构造参数、`Row::get` 的返回类型、
//! `SqlExecutor` 全部 7 个方法、`RdbmsClient::transaction/replica_lag`、`Cache`
//! 三个必需方法与三个默认方法的返回值、`WideColumnClient` 四个方法、`Dialect` 五个变体。
//!
//! 所有调用都写在从未实例化的泛型函数里 —— 泛型体在定义时即做类型检查，运行时
//! 不会发生，因此不触达网络/磁盘。参数用显式类型绑定（而非 `&[]` 之类的可推断
//! 字面量），改参数类型同样会红灯。
//!
//! 任何破坏性改动（改签名、参数或返回类型，删 trait、删默认方法、删 `Dialect`
//! 变体）都会让本文件编译失败，从而在 CI 上红灯。`cargo semver-checks` 需要
//! 7.1.1 baseline tag，落地前以此文件作为最低兼容门。

use ecat_data::{
    Cache, Dialect, Error, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, WideColumnClient,
};
use std::collections::BTreeMap;
use std::time::Duration;

fn _assert_row(row: &Row) {
    let _: Option<&serde_json::Value> = row.get("id");
    let _: &[String] = row.columns();
    let _: &[serde_json::Value] = row.values();
}

fn _assert_row_ctor() {
    let columns: Vec<String> = vec!["id".into()];
    let values: Vec<serde_json::Value> = vec![serde_json::Value::Null];
    let row: Row = Row::new(columns, values);
    let _: usize = row.iter().count();
}

async fn _assert_executor<E: SqlExecutor>(e: &E) {
    let params: &[serde_json::Value] = &[];
    let _: Result<u64, RdbmsError> = e.execute("SELECT 1").await;
    let _: Result<Vec<Row>, RdbmsError> = e.query("SELECT 1").await;
    let _: Result<u64, RdbmsError> = e.execute_with("SELECT 1", params).await;
    let _: Result<Vec<Row>, RdbmsError> = e.query_with("SELECT 1", params).await;
    let _: Result<Vec<Row>, RdbmsError> = e.query_write("SELECT 1", params).await;
    let _: Result<Vec<Row>, RdbmsError> =
        e.execute_then_query("SELECT 1", params, "SELECT 1").await;
    let _: Dialect = e.dialect();
}

async fn _assert_client<C: RdbmsClient>(c: &C) {
    let _: Result<Transaction, RdbmsError> = c.transaction().await;
    let _: Option<Duration> = c.replica_lag().await;
}

// 冒烟测试的职责就是钉住完整的公开签名，不抽 type alias。
#[allow(clippy::type_complexity)]
async fn _assert_wide_column<C: WideColumnClient>(c: &C) {
    let cells: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    let _: Result<(), Error> = c.put("t", "r", &cells).await;
    let _: Result<Option<BTreeMap<String, serde_json::Value>>, Error> = c.get("t", "r").await;
    let _: Result<Vec<(String, BTreeMap<String, serde_json::Value>)>, Error> =
        c.scan("t", "prefix", 10).await;
    let _: Result<(), Error> = c.delete("t", "r").await;
}

async fn _assert_cache<C: Cache>(c: &C) {
    let _: Result<Option<Vec<u8>>, Error> = c.get("k").await;
    let _: Result<(), Error> = c.set("k", b"v", Duration::from_secs(1)).await;
    let _: Result<(), Error> = c.delete("k").await;
    // 默认方法：必须存在且返回类型不变。
    let _: Result<i64, Error> = c.increment("k", 1).await;
    let _: Result<Option<Duration>, Error> = c.ttl("k").await;
    let _: Result<Vec<Option<Vec<u8>>>, Error> = c.multi_get(&["k"]).await;
}

#[test]
fn public_api_compiles() {
    let _ = Dialect::Standard;
    let _ = Dialect::Sqlite;
    let _ = Dialect::Postgres;
    let _ = Dialect::MySql;
    let _ = Dialect::Mssql;
}
