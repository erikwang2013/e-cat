// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//!
//! `crud` 的测试夹具：假 executor（含事务侧）与测试实体。
//!
//! **单独成文件**是为了守住项目硬规则「每个源文件 < 500 行」——
//! 夹具与断言分开后各自都在上限内。夹具要跨模块用，故标 `pub(crate)`。

use crate::entity::*;
use crate::error::OrmError;
use async_trait::async_trait;
use ecat_data::{
    Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction, TransactionInner,
};
use serde_json::json;
use std::sync::Arc;
use std::sync::Mutex;

/// 记录收到的 `(sql, params)` 与**调用归属**的假 executor。
///
/// 记下的每一次调用：`(sql, params)`。
type Calls = Arc<Mutex<Vec<(String, Vec<serde_json::Value>)>>>;

/// `events` 是给「MySQL 两步式必须在同一事务里」用的：光看 SQL 文本
/// 分辨不出语句是在事务里发的还是直连发的 —— 那正是静默错值的来源。
#[derive(Clone)]
pub(crate) struct Spy {
    pub(crate) calls: Calls,
    /// `db` = 直连执行器，`tx` = 事务内，`transaction` / `commit` = 事务边界。
    pub(crate) events: Arc<Mutex<Vec<&'static str>>>,
    pub(crate) rows: Arc<Mutex<Vec<Row>>>,
    pub(crate) affected: u64,
    pub(crate) dialect: Dialect,
}

impl Default for Spy {
    fn default() -> Self {
        Self {
            calls: Arc::default(),
            events: Arc::default(),
            rows: Arc::default(),
            affected: 0,
            dialect: Dialect::Sqlite,
        }
    }
}

impl Spy {
    /// 记一条语句。`origin` 标明它是直连还是事务内。
    fn record(&self, origin: &'static str, sql: &str, params: &[serde_json::Value]) {
        self.calls
            .lock()
            .unwrap()
            .push((sql.to_string(), params.to_vec()));
        self.events.lock().unwrap().push(origin);
    }

    pub(crate) fn events(&self) -> Vec<&'static str> {
        self.events.lock().unwrap().clone()
    }

    pub(crate) fn calls(&self) -> Vec<(String, Vec<serde_json::Value>)> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl SqlExecutor for Spy {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        self.record("db", sql, &[]);
        Ok(self.affected)
    }
    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.record("db", sql, &[]);
        Ok(self.rows.lock().unwrap().clone())
    }
    async fn execute_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<u64, RdbmsError> {
        self.record("db", sql, p);
        Ok(self.affected)
    }
    async fn query_with(&self, sql: &str, p: &[serde_json::Value]) -> Result<Vec<Row>, RdbmsError> {
        self.record("db", sql, p);
        Ok(self.rows.lock().unwrap().clone())
    }
    /// 客户端路径的覆写：**自己开一个事务**包住两条语句并提交 —— 与
    /// `SqlxClient` 同形。不覆写就会走默认的「不支持」，两步式（MySQL）
    /// 在客户端路径上直接报错，而那不是 `insert` 的错。
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
        self.dialect
    }
}

/// 建一个**真实**的内存 SQLite 客户端。单连接 + 共享缓存：多连接池下每条
/// 连接是各自独立的空库，前一条连接建的表后续连接看不到。
/// 只给「数据真的落库了吗」这类测试用（假 executor 答不了这个问题）。
pub(crate) async fn mem_sqlite(name: &str) -> ecat_data_sqlx::SqlxClient {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let params = ecat_data_sqlx::PoolParams {
        max_connections: 1,
        ..ecat_data_sqlx::PoolParams::default()
    };
    ecat_data_sqlx::SqlxClient::connect_with_params(
        &format!("sqlite:ecat-orm-fixture-{name}{n}?mode=memory&cache=shared"),
        &params,
    )
    .await
    .expect("in-memory sqlite must connect")
}

/// 事务内层：把语句记进**同一个** Spy 的日志，归属标记为 `tx`。
/// 「归属」就是这样被断言的 —— 事务里的语句不可能记成 `db`。
struct SpyTx {
    spy: Spy,
}

#[async_trait]
impl TransactionInner for SpyTx {
    async fn execute(&mut self, sql: &str) -> Result<u64, RdbmsError> {
        self.spy.record("tx", sql, &[]);
        Ok(self.spy.affected)
    }
    async fn query(&mut self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        self.spy.record("tx", sql, &[]);
        Ok(self.spy.rows.lock().unwrap().clone())
    }
    async fn execute_with(
        &mut self,
        sql: &str,
        p: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        self.spy.record("tx", sql, p);
        Ok(self.spy.affected)
    }
    async fn query_with(
        &mut self,
        sql: &str,
        p: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        self.spy.record("tx", sql, p);
        Ok(self.spy.rows.lock().unwrap().clone())
    }
    fn dialect(&self) -> Dialect {
        self.spy.dialect
    }
    async fn commit(&mut self) -> Result<(), RdbmsError> {
        self.spy.events.lock().unwrap().push("commit");
        Ok(())
    }
    async fn rollback(&mut self) -> Result<(), RdbmsError> {
        self.spy.events.lock().unwrap().push("rollback");
        Ok(())
    }
}

#[async_trait]
impl RdbmsClient for Spy {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        self.events.lock().unwrap().push("transaction");
        Ok(Transaction::with_inner(Box::new(SpyTx {
            spy: self.clone(),
        })))
    }
}

// 实体：自增主键 + 一个普通列
static COLS: [ColumnMeta; 2] = [
    ColumnMeta {
        name: "id",
        ty: ColType::I64,
        nullable: false,
        pk: true,
        auto_increment: true,
    },
    ColumnMeta {
        name: "name",
        ty: ColType::Text,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
];
static META: EntityMeta = EntityMeta {
    table: "users",
    pk: "id",
    columns: &COLS,
    relations: &[],
    flags: EntityFlags::NONE,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct U {
    pub(crate) id: i64,
    pub(crate) name: String,
}

impl Entity for U {
    const TABLE: &'static str = "users";
    const PK: &'static str = "id";
    const META: &'static EntityMeta = &META;
    fn from_row(r: &Row) -> Result<Self, OrmError> {
        Ok(U {
            id: crate::value::from_row_col::<i64>(r, "id")?,
            name: crate::value::from_row_col::<String>(r, "name")?,
        })
    }
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
        vec![("id", json!(self.id)), ("name", json!(self.name))]
    }
    fn pk_value(&self) -> serde_json::Value {
        json!(self.id)
    }
    fn set_relation(&mut self, _name: &str, _rows: Vec<Row>) -> Result<(), OrmError> {
        Ok(())
    }
}

pub(crate) fn last(spy: &Spy) -> (String, Vec<serde_json::Value>) {
    spy.calls().pop().expect("no call recorded")
}

/// 指定方言与受影响行数的 spy。批量测试要按方言的上限分块，逐个写字面量太吵。
pub(crate) fn spy(dialect: Dialect, affected: u64) -> Spy {
    Spy {
        dialect,
        affected,
        ..Default::default()
    }
}

// ---- 非自增主键（手工分配）----

pub(crate) static MANUAL_COLS: [ColumnMeta; 2] = [
    ColumnMeta {
        name: "id",
        ty: ColType::I64,
        nullable: false,
        pk: true,
        auto_increment: false,
    },
    ColumnMeta {
        name: "name",
        ty: ColType::Text,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
];
pub(crate) static MANUAL_META: EntityMeta = EntityMeta {
    table: "manual",
    pk: "id",
    columns: &MANUAL_COLS,
    relations: &[],
    flags: EntityFlags::NONE,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct P {
    pub(crate) id: i64,
    pub(crate) name: String,
}

impl Entity for P {
    const TABLE: &'static str = "manual";
    const PK: &'static str = "id";
    const META: &'static EntityMeta = &MANUAL_META;
    fn from_row(r: &Row) -> Result<Self, OrmError> {
        Ok(P {
            id: crate::value::from_row_col::<i64>(r, "id")?,
            name: crate::value::from_row_col::<String>(r, "name")?,
        })
    }
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
        vec![("id", json!(self.id)), ("name", json!(self.name))]
    }
    fn pk_value(&self) -> serde_json::Value {
        json!(self.id)
    }
    fn set_relation(&mut self, _name: &str, _rows: Vec<Row>) -> Result<(), OrmError> {
        Ok(())
    }
}

// ---- 字符串主键（UUID）----

static UUID_COLS: [ColumnMeta; 2] = [
    ColumnMeta {
        name: "id",
        ty: ColType::Text,
        nullable: false,
        pk: true,
        auto_increment: false,
    },
    ColumnMeta {
        name: "name",
        ty: ColType::Text,
        nullable: false,
        pk: false,
        auto_increment: false,
    },
];
static UUID_META: EntityMeta = EntityMeta {
    table: "uuid_rows",
    pk: "id",
    columns: &UUID_COLS,
    relations: &[],
    flags: EntityFlags::NONE,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct S {
    pub(crate) id: String,
    pub(crate) name: String,
}

impl Entity for S {
    const TABLE: &'static str = "uuid_rows";
    const PK: &'static str = "id";
    const META: &'static EntityMeta = &UUID_META;
    fn from_row(r: &Row) -> Result<Self, OrmError> {
        Ok(S {
            id: crate::value::from_row_col::<String>(r, "id")?,
            name: crate::value::from_row_col::<String>(r, "name")?,
        })
    }
    fn to_values(&self) -> Vec<(&'static str, serde_json::Value)> {
        vec![("id", json!(self.id)), ("name", json!(self.name))]
    }
    fn pk_value(&self) -> serde_json::Value {
        json!(self.id)
    }
    fn set_relation(&mut self, _name: &str, _rows: Vec<Row>) -> Result<(), OrmError> {
        Ok(())
    }
}
