//! 编译期公开 API 冒烟测试：只引用签名，不执行任何 I/O。
//!
//! 任何破坏性改动（改签名、删 trait、删默认方法、改 `Dialect` 变体）都会让
//! 本文件编译失败，从而在 CI 上红灯。`cargo semver-checks` 需要 7.1.1
//! baseline tag，落地前以此文件作为最低兼容门。

use ecat_data::{Cache, Dialect, RdbmsClient, Row, SqlExecutor};

fn _assert_row(row: &Row) {
    let _: Option<&serde_json::Value> = row.get("id");
}

fn _assert_row_ctor() {
    let row = Row::new(vec!["id".into()], vec![serde_json::Value::Null]);
    let _ = row.columns().len() + row.values().len() + row.iter().count();
}

/// 泛型体在定义时即被类型检查：引用方法签名但从不实例化，故不会触达 I/O。
async fn _assert_executor<E: SqlExecutor>(_: &E) {
    let _ = E::dialect;
    let _ = E::execute;
    let _ = E::query;
    let _ = E::execute_with;
    let _ = E::query_with;
    let _ = E::query_write;
    let _ = E::execute_then_query;
}

async fn _assert_client<C: RdbmsClient>(_: &C) {
    let _ = C::transaction;
    let _ = C::replica_lag;
}

async fn _assert_cache<C: Cache>(_: &C) {
    let _ = C::get;
    let _ = C::set;
    let _ = C::delete;
    // 默认方法也必须存在。
    let _ = C::increment;
    let _ = C::ttl;
    let _ = C::multi_get;
}

#[test]
fn public_api_compiles() {
    let _ = Dialect::Standard;
    let _ = Dialect::Sqlite;
    let _ = Dialect::Postgres;
    let _ = Dialect::MySql;
    let _ = Dialect::Mssql;
}
