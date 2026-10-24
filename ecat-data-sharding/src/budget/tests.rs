// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 连接池预算测试：全部用假客户端，纯计算、不连库。
use std::sync::Arc;

use async_trait::async_trait;
use ecat_data::{Dialect, RdbmsClient, RdbmsError, Row, SqlExecutor, Transaction};

use super::{PoolBudget, validate_pool_budget};
use crate::ShardError;

/// 假客户端：`pool_size` 为 `None` 即模拟「后端不报告池大小」。
struct FakeClient {
    pool_size: Option<u32>,
}

#[async_trait]
impl SqlExecutor for FakeClient {
    async fn execute(&self, _sql: &str) -> Result<u64, RdbmsError> {
        Err(RdbmsError::Database("budget checks do not run SQL".into()))
    }

    async fn query(&self, _sql: &str) -> Result<Vec<Row>, RdbmsError> {
        Err(RdbmsError::Database("budget checks do not run SQL".into()))
    }

    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }
}

#[async_trait]
impl RdbmsClient for FakeClient {
    async fn transaction(&self) -> Result<Transaction, RdbmsError> {
        Ok(Transaction::new())
    }

    fn pool_size(&self) -> Option<u32> {
        self.pool_size
    }
}

fn clients(sizes: &[Option<u32>]) -> Vec<Arc<dyn RdbmsClient>> {
    sizes
        .iter()
        .map(|&pool_size| Arc::new(FakeClient { pool_size }) as Arc<dyn RdbmsClient>)
        .collect()
}

fn budget(max_total_connections: u32) -> PoolBudget {
    PoolBudget {
        max_total_connections,
    }
}

#[test]
fn pool_budget_rejects_oversubscription() {
    let err = validate_pool_budget(&clients(&[Some(10), Some(10), Some(10)]), 1, &budget(20))
        .unwrap_err();
    assert!(matches!(err, ShardError::Config(_)), "got: {err}");
    let msg = err.to_string();
    assert!(msg.contains("30"), "错误信息要带上实际总和，got: {msg}");
    assert!(msg.contains("20"), "错误信息要带上预算上限，got: {msg}");
}

#[test]
fn pool_budget_accepts_exact() {
    assert_eq!(
        validate_pool_budget(&clients(&[Some(10), Some(10), Some(10)]), 1, &budget(30)),
        Ok(())
    );
}

#[test]
fn pool_budget_multiplies_by_instance_count() {
    let shards = clients(&[Some(10), Some(10), Some(10)]);
    let err = validate_pool_budget(&shards, 2, &budget(50)).unwrap_err();
    assert!(err.to_string().contains("60"), "got: {err}");
    assert_eq!(validate_pool_budget(&shards, 2, &budget(60)), Ok(()));
}

#[test]
fn pool_budget_ignores_unknown_pool_size() {
    // None 不参与求和：已知部分 10 + 10 在预算 20 之内。
    assert_eq!(
        validate_pool_budget(&clients(&[Some(10), None, Some(10)]), 1, &budget(20)),
        Ok(())
    );
}

#[test]
fn pool_budget_error_names_unknown_clients() {
    // 超预算时必须注明有客户端被跳过：「未知 = 0」的假通过否则无从察觉。
    let err = validate_pool_budget(&clients(&[Some(30), None]), 1, &budget(20)).unwrap_err();
    assert!(err.to_string().contains("unknown"), "got: {err}");
}
