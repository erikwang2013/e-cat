//! 跨 crate 回归：**同一个进程里**两个 RDBMS 后端各自注册后，四个
//! `ecat_rdbms_*` 家族必须都在，且**两组 backend 标签的样本都能取到**。
//!
//! 为什么必须放在一个测试二进制里：这正是缺陷的触发条件。单 crate 的
//! `cargo test -p ecat-data-sqlx --features metrics` 永远看不到另一个 crate
//! 的注册，所以它在两边都是绿的 —— 只有两个都链进来才复现。
//!
//! 两个池都不需要真库：sqlite 走 `sqlite::memory:`（进程内），MSSQL 走 deadpool
//! 的惰性建连（建池不连库）。

use ecat_data_mssql::MssqlClient;
use ecat_data_sqlx::SqlxClient;

const FAMILIES: [&str; 4] = [
    "ecat_rdbms_pool_connections",
    "ecat_rdbms_pool_timeouts_total",
    "ecat_rdbms_query_timeout_total",
    "ecat_rdbms_transactions_leaked_total",
];

#[tokio::test]
async fn both_backends_keep_their_samples_in_one_process() {
    let sqlx = SqlxClient::connect("sqlite::memory:").await.unwrap();
    let mssql = MssqlClient::connect("mssql://sa:pw@127.0.0.1:1433/db")
        .await
        .unwrap();

    ecat_data_sqlx::register_pool_metrics("regression-sqlx", sqlx.pool());
    ecat_data_mssql::register_pool_metrics("regression-mssql", mssql.pool());

    let text = ecat_metrics::metrics_text();

    for family in FAMILIES {
        for backend in ["regression-sqlx", "regression-mssql"] {
            assert!(
                text.contains(&format!("{family}{{backend=\"{backend}\"")),
                "缺 {family}{{backend=\"{backend}\"}} —— 后注册者的整份 collector \
                 被 AlreadyReg 顶掉了。实际输出:\n{text}"
            );
        }
    }
}
