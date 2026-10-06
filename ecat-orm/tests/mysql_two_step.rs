// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! MySQL 两步式主键回填（Task 18 Step 2 第 2 条，Task 13 的核心风险）。
//!
//! `LAST_INSERT_ID()` 是**连接作用域**的：`INSERT` 与取回主键的两条语句若各取
//! 一次连接，可能落到不同连接 —— 取回的是别的会话刚插入的 id，**静默错值**。
//! SQLite 走一步式（`RETURNING`），所以这条在 SQLite 上跑不出真问题，
//! 用**调用序列与归属**代替：谁开的语句、落在哪个连接上，全在日志里。
//!
//! 两个测试分别钉住两半：
//! 1. `mysql_insert_...`：ORM 侧的决策 —— MySQL 方言 → `InsertThen` →
//!    经 `execute_then_query` 发出「INSERT 然后 LAST_INSERT_ID」，且两条
//!    **都归属同一个事务**、以 `commit()` 收尾（用假 executor，无 MySQL 也能跑）。
//! 2. `the_real_client_...`：`ecat-data-sqlx` 侧的实现 —— 真客户端确实把两条
//!    包进一个事务并提交了（真 SQLite 上验证「提交」这一步真的发生）。

use ecat_data::{
    Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
};
use ecat_data_sqlx::SqlxClient;
use ecat_orm::Entity;
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Entity, Debug, Clone)]
#[entity(table = "widgets")]
pub struct Widget {
    #[entity(pk, auto_increment)]
    pub id: i64,
    pub name: String,
}

type Log = Arc<Mutex<Vec<String>>>;

/// 记录**调用序列与归属**的假 executor：`client:` 前缀 = 直连发的，
/// `tx:` = 事务内发的，另有事务边界事件。归属靠前缀分辨 —— 光看 SQL 文本
/// 分辨不出语句是在事务里发的还是直连发的，而那正是静默错值的来源。
#[derive(Clone, Default)]
struct Rec {
    log: Log,
    rows: Arc<Mutex<Vec<Row>>>,
}

impl Rec {
    fn push(&self, event: String) {
        self.log.lock().unwrap().push(event);
    }
    fn events(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl SqlExecutor for Rec {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.push(format!("client:{sql}"));
        Ok(1)
    }
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.push(format!("client:{sql}"));
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn execute_with(&self, sql: &str, _p: &[serde_json::Value]) -> Result<u64, RdbmsError> {
        self.push(format!("client:{sql}"));
        Ok(1)
    }
    async fn query_with(
        &self,
        sql: &str,
        _p: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.push(format!("client:{sql}"));
        Ok(self.rows.lock().unwrap().clone())
    }
    /// 与 `SqlxClient::execute_then_query`（`ecat-data-sqlx/src/lib.rs:349`）
    /// **同形**：开事务 → 第一条 → 第二条 → 提交。这里是「假 executor 只有一条
    /// 连接、抓不到漂移」的那一层替身；真实现由本文件第二个测试单独验证。
    async fn execute_then_query(
        &self,
        first: &str,
        first_params: &[serde_json::Value],
        second: &str,
    ) -> Result<Vec<Row>, RdbmsError> {
        let tx = self.transaction().await?;
        tx.execute_with(first, first_params).await?;
        let rows = tx.query(second).await?;
        tx.commit().await?;
        Ok(rows)
    }
    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
}

#[async_trait::async_trait]
impl RdbmsClient for Rec {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.push("transaction()".into());
        Ok(Transaction::with_inner(Box::new(RecTx {
            owner: self.clone(),
        })))
    }
}

struct RecTx {
    owner: Rec,
}

#[async_trait::async_trait]
impl TransactionInner for RecTx {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
        self.owner.push(format!("tx:{sql}"));
        Ok(1)
    }
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.owner.push(format!("tx:{sql}"));
        Ok(self.owner.rows.lock().unwrap().clone())
    }
    async fn execute_with(
        &mut self,
        sql: &str,
        _p: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.owner.push(format!("tx:{sql}"));
        Ok(1)
    }
    async fn query_with(
        &mut self,
        sql: &str,
        _p: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.owner.push(format!("tx:{sql}"));
        Ok(self.owner.rows.lock().unwrap().clone())
    }
    fn dialect(&self) -> Dialect {
        Dialect::MySql
    }
    async fn commit(&mut self) -> Result<(), RdbmsError> {
        self.owner.push("commit()".into());
        Ok(())
    }
    async fn rollback(&mut self) -> Result<(), RdbmsError> {
        self.owner.push("rollback()".into());
        Ok(())
    }
}

// ---- ① ORM 侧：决策与归属 ----

#[tokio::test]
async fn mysql_insert_is_two_statements_inside_one_transaction() {
    let rec = Rec::default();
    *rec.rows.lock().unwrap() = vec![Row::new(vec!["id".into()], vec![json!(7)])];

    let id = Widget::insert(
        &rec,
        &Widget {
            id: 0,
            name: "alice".into(),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        id, 7,
        "返回的必须是第二步取回的主键，不是 INSERT 的影响行数"
    );
    assert_eq!(
        rec.events(),
        vec![
            "transaction()".to_string(),
            "tx:INSERT INTO `widgets` (`name`) VALUES (?)".to_string(),
            "tx:SELECT LAST_INSERT_ID()".to_string(),
            "commit()".to_string(),
        ],
        "两步式的**顺序与归属**：两条都必须在同一个事务里，且以 commit 收尾 —— \
         任一条记成 `client:` 就是直连发（池下会取回别的会话的 id，静默错值）"
    );
}

// ---- ② 真客户端：那层包装真的提交了 ----

/// 真 `SqlxClient::execute_then_query` 在真库上确实把两条语句包进一个事务并
/// 提交。「提交」用**另一条连接**读来验证 —— 同池复用连接的话，未提交的数据
/// 在源连接上照样看得到，断言对「没提交」恒真。
#[tokio::test]
async fn the_real_client_wraps_and_commits_two_statements() {
    let path = std::env::temp_dir().join(format!(
        "ecat-orm-e2e-two-step-{}-{}.db",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite:{}?mode=rwc", path.display());

    let writer = SqlxClient::connect(&url).await.unwrap();
    writer
        .execute("CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, v TEXT)")
        .await
        .unwrap();

    let rows = writer
        .execute_then_query(
            "INSERT INTO t (v) VALUES (?)",
            &[json!("a")],
            "SELECT last_insert_rowid() AS id",
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "第二条语句的结果必须被返回");
    let id = rows[0].get("id").and_then(|v| v.as_i64()).unwrap();
    assert!(id > 0);

    let reader = SqlxClient::connect(&url).await.unwrap();
    let seen = reader
        .query(&format!("SELECT v FROM t WHERE id = {id}"))
        .await
        .unwrap();
    assert_eq!(seen.len(), 1, "另一条连接必须读得到 —— 说明事务真的提交了");
    assert_eq!(seen[0].get("v").and_then(|v| v.as_str()), Some("a"));

    let _ = std::fs::remove_file(&path);
}
