// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 迁移执行器测试：SQLite 内存库覆盖回填/校验/切流/回滚；fence 用内存实现。
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ecat_data::RdbmsClient;
use ecat_data_sqlx::{PoolParams, SqlxClient};
use ecat_lock::{DistributedLock, FenceGuard, FencedLock, LockError};
use serde_json::json;

use crate::{ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardRouter, ShardTable};

use super::{KeyRange, ShardMigrator, ShardMove, ShardPlan};

async fn db() -> Arc<dyn RdbmsClient> {
    let params = PoolParams {
        max_connections: 1,
        ..PoolParams::default()
    };
    Arc::new(
        SqlxClient::connect_with_params("sqlite::memory:", &params)
            .await
            .unwrap(),
    )
}

async fn schema(db: &Arc<dyn RdbmsClient>) {
    db.execute("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER)")
        .await
        .unwrap();
}

async fn insert_row(db: &Arc<dyn RdbmsClient>, id: i64, amount: i64) {
    db.execute_with(
        "INSERT INTO orders (id, amount) VALUES (?, ?)",
        &[json!(id), json!(amount)],
    )
    .await
    .unwrap();
}

async fn row_count(db: &Arc<dyn RdbmsClient>) -> i64 {
    db.query("SELECT COUNT(*) AS n FROM orders").await.unwrap()[0]
        .get("n")
        .unwrap()
        .as_i64()
        .unwrap()
}

async fn rows(db: &Arc<dyn RdbmsClient>) -> Vec<(i64, i64)> {
    db.query("SELECT id, amount FROM orders ORDER BY id")
        .await
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get("id").unwrap().as_i64().unwrap(),
                row.get("amount").unwrap().as_i64().unwrap(),
            )
        })
        .collect()
}

fn table(algorithm: ShardAlgorithm, shards: &[&str]) -> ShardTable {
    ShardTable {
        name: "orders".into(),
        key: "id".into(),
        pk: "id".into(),
        algorithm,
        shards: shards
            .iter()
            .map(|name| ShardEndpoint {
                name: (*name).into(),
                url: None,
            })
            .collect(),
    }
}

fn range_catalog(boundaries: &[i64], shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table(
            ShardAlgorithm::Range {
                boundaries: boundaries.to_vec(),
            },
            shards,
        )],
    }
}

fn hash_catalog(shards: &[&str]) -> ShardCatalog {
    ShardCatalog {
        tables: vec![table(
            ShardAlgorithm::HashMod {
                shards: shards.len() as u32,
            },
            shards,
        )],
    }
}

fn boundaries(catalog: &ShardCatalog) -> Vec<i64> {
    match &catalog.tables[0].algorithm {
        ShardAlgorithm::Range { boundaries } => boundaries.clone(),
        other => panic!("expected range algorithm, got {other:?}"),
    }
}

struct Fixture {
    migrator: ShardMigrator,
    source: Arc<dyn RdbmsClient>,
    target: Arc<dyn RdbmsClient>,
    router: ShardRouter,
    old: ShardCatalog,
    new: ShardCatalog,
}

async fn fixture() -> Fixture {
    let source = db().await;
    schema(&source).await;
    for id in 1..=5 {
        insert_row(&source, id, id * 10).await;
    }
    let target = db().await;
    schema(&target).await;
    let state = db().await;

    let old = range_catalog(&[100], &["s0", "s1"]);
    let new = range_catalog(&[0], &["s0", "s1"]);
    let plan = ShardPlan {
        moves: vec![ShardMove {
            table: "orders".into(),
            from: "s0".into(),
            to: "s1".into(),
            key_range: Some(KeyRange {
                low: Some(0),
                high: Some(100),
            }),
        }],
    };
    let mut clients: HashMap<String, Arc<dyn RdbmsClient>> = HashMap::new();
    clients.insert("s0".into(), source.clone());
    clients.insert("s1".into(), target.clone());
    let migrator = ShardMigrator::new(plan, clients.clone(), state, old.clone(), new.clone())
        .unwrap()
        .with_chunk_size(2);
    let router = ShardRouter::new(old.clone(), clients).unwrap();
    Fixture {
        migrator,
        source,
        target,
        router,
        old,
        new,
    }
}

#[test]
fn plan_diff_detects_range_moves() {
    let old = range_catalog(&[100], &["s0", "s1"]);
    let new = range_catalog(&[50], &["s0", "s1"]);
    let plan = ShardPlan::diff(&old, &new).unwrap();
    assert_eq!(plan.moves.len(), 1, "moves: {:?}", plan.moves);
    let mv = &plan.moves[0];
    assert_eq!((mv.from.as_str(), mv.to.as_str()), ("s0", "s1"));
    assert_eq!(
        mv.key_range,
        Some(KeyRange {
            low: Some(50),
            high: Some(100)
        })
    );
}

#[test]
fn plan_diff_detects_hash_remap_as_full_copy() {
    let old = hash_catalog(&["s0", "s1"]);
    let new = hash_catalog(&["s0", "s1", "s2"]);
    let plan = ShardPlan::diff(&old, &new).unwrap();
    assert_eq!(plan.moves.len(), 6, "moves: {:?}", plan.moves);
    assert!(plan.moves.iter().all(|mv| mv.key_range.is_none()));
    let pairs: Vec<(&str, &str)> = plan
        .moves
        .iter()
        .map(|mv| (mv.from.as_str(), mv.to.as_str()))
        .collect();
    for expected in [
        ("s0", "s0"),
        ("s0", "s1"),
        ("s0", "s2"),
        ("s1", "s0"),
        ("s1", "s1"),
        ("s1", "s2"),
    ] {
        assert!(pairs.contains(&expected), "missing {expected:?}: {pairs:?}");
    }
}

#[tokio::test]
async fn hash_remap_backfills_verifies_and_cuts_over() {
    let old = hash_catalog(&["s0", "s1"]);
    let new = hash_catalog(&["s0", "s2"]);
    let source0 = db().await;
    let source1 = db().await;
    let target = db().await;
    for db in [&source0, &source1, &target] {
        schema(db).await;
    }
    for id in 1..=20 {
        let index = old.shard_for("orders", &json!(id)).unwrap();
        let db = if index == 0 { &source0 } else { &source1 };
        insert_row(db, id, id * 10).await;
    }
    let moving = row_count(&source1).await;
    assert!(moving > 0, "test ids must hit both old shards");

    let state = db().await;
    let mut clients: HashMap<String, Arc<dyn RdbmsClient>> = HashMap::new();
    clients.insert("s0".into(), source0.clone());
    clients.insert("s1".into(), source1.clone());
    clients.insert("s2".into(), target.clone());
    let plan = ShardPlan::diff(&old, &new).unwrap();
    let migrator = ShardMigrator::new(plan, clients.clone(), state, old.clone(), new.clone())
        .unwrap()
        .with_chunk_size(3);
    let router = ShardRouter::new(old, clients).unwrap();

    migrator.backfill().await.unwrap();
    assert_eq!(
        row_count(&target).await,
        moving,
        "new shard must receive every moved row"
    );
    migrator.verify().await.unwrap();
    migrator.cutover(&router, new).await.unwrap();
    let catalog = router.catalog();
    let shards: Vec<&str> = catalog.tables[0]
        .shards
        .iter()
        .map(|shard| shard.name.as_str())
        .collect();
    assert_eq!(shards, vec!["s0", "s2"]);
}

#[tokio::test]
async fn backfill_is_idempotent() {
    let f = fixture().await;
    let first = f.migrator.backfill().await.unwrap();
    assert_eq!(first.moved_rows, 5);
    assert_eq!(first.chunks, 3, "chunk_size 2 over 5 rows");
    assert_eq!(row_count(&f.target).await, 5);
    assert_eq!(rows(&f.target).await, rows(&f.source).await);

    let second = f.migrator.backfill().await.unwrap();
    assert_eq!(second.moved_rows, 0, "resume must not re-copy");
    assert_eq!(row_count(&f.target).await, 5);
}

#[tokio::test]
async fn verify_mismatch_blocks_cutover() {
    let f = fixture().await;
    f.migrator.backfill().await.unwrap();
    f.target
        .execute("DELETE FROM orders WHERE id = 3")
        .await
        .unwrap();

    let err = f.migrator.verify().await.unwrap_err();
    assert!(err.to_string().contains("verify mismatch"), "got: {err}");
    let err = f
        .migrator
        .cutover(&f.router, f.new.clone())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not verified"), "got: {err}");
    assert_eq!(boundaries(&f.router.catalog()), vec![100]);
}

#[tokio::test]
async fn cutover_reloads_router() {
    let f = fixture().await;
    f.migrator.backfill().await.unwrap();
    f.migrator.verify().await.unwrap();
    f.migrator.cutover(&f.router, f.new.clone()).await.unwrap();
    assert_eq!(boundaries(&f.router.catalog()), vec![0]);
}

#[tokio::test]
async fn rebackfill_after_rollback_can_cutover() {
    let f = fixture().await;
    f.migrator.backfill().await.unwrap();
    f.migrator.verify().await.unwrap();
    f.migrator.cutover(&f.router, f.new.clone()).await.unwrap();
    f.migrator.rollback(&f.router).await.unwrap();
    assert_eq!(boundaries(&f.router.catalog()), vec![100]);

    let report = f.migrator.backfill().await.unwrap();
    assert_eq!(report.moved_rows, 0, "cursor is already at the end");
    f.migrator.verify().await.unwrap();
    f.migrator.cutover(&f.router, f.new.clone()).await.unwrap();
    assert_eq!(boundaries(&f.router.catalog()), vec![0]);
}

#[tokio::test]
async fn rollback_restores_old_catalog() {
    let f = fixture().await;
    f.migrator.backfill().await.unwrap();
    f.migrator.verify().await.unwrap();
    f.migrator.cutover(&f.router, f.new.clone()).await.unwrap();
    assert_eq!(boundaries(&f.router.catalog()), vec![0]);

    f.migrator.rollback(&f.router).await.unwrap();
    assert_eq!(boundaries(&f.router.catalog()), vec![100]);
    assert_eq!(f.old.tables[0].name, f.router.catalog().tables[0].name);
}

#[derive(Default)]
struct FenceState {
    holder: Option<String>,
    fence: u64,
    releases: u32,
}

struct FakeFence {
    state: tokio::sync::Mutex<FenceState>,
}

impl FakeFence {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: tokio::sync::Mutex::new(FenceState::default()),
        })
    }

    async fn held(&self) -> bool {
        self.state.lock().await.holder.is_some()
    }

    async fn releases(&self) -> u32 {
        self.state.lock().await.releases
    }
}

#[async_trait]
impl DistributedLock for FakeFence {
    async fn acquire(&self, key: &str, ttl: Duration) -> Result<Option<String>, LockError> {
        Ok(self
            .acquire_fenced(key, ttl)
            .await?
            .map(|guard| guard.token))
    }

    async fn release(&self, _key: &str, token: &str) -> Result<(), LockError> {
        let mut state = self.state.lock().await;
        if state.holder.as_deref() != Some(token) {
            return Err(LockError::Other("token mismatch".into()));
        }
        state.holder = None;
        state.releases += 1;
        Ok(())
    }

    async fn renew(&self, _key: &str, token: &str, _ttl: Duration) -> Result<(), LockError> {
        let state = self.state.lock().await;
        if state.holder.as_deref() != Some(token) {
            return Err(LockError::Other("token mismatch".into()));
        }
        Ok(())
    }
}

#[async_trait]
impl FencedLock for FakeFence {
    async fn acquire_fenced(
        &self,
        _key: &str,
        _ttl: Duration,
    ) -> Result<Option<FenceGuard>, LockError> {
        let mut state = self.state.lock().await;
        state.fence += 1;
        if state.holder.is_some() {
            return Ok(None);
        }
        let token = format!("token-{}", state.fence);
        state.holder = Some(token.clone());
        Ok(Some(FenceGuard {
            token,
            fence: state.fence,
        }))
    }

    async fn validate_fence(&self, _key: &str, fence: u64) -> Result<bool, LockError> {
        let state = self.state.lock().await;
        Ok(state.holder.is_some() && state.fence == fence)
    }
}

#[tokio::test]
async fn backfill_with_fence_acquires_and_releases() {
    let f = fixture().await;
    let fence = FakeFence::new();
    let migrator =
        f.migrator
            .with_fence(fence.clone(), "migration:orders", Duration::from_secs(60));
    let report = migrator.backfill().await.unwrap();
    assert_eq!(report.moved_rows, 5);
    assert!(!fence.held().await, "backfill must release the fence");
    assert_eq!(fence.releases().await, 1);
}

#[tokio::test]
async fn backfill_refuses_when_fence_is_held() {
    let f = fixture().await;
    let fence = FakeFence::new();
    let guard = fence
        .acquire_fenced("migration:orders", Duration::from_secs(60))
        .await
        .unwrap()
        .expect("first holder");
    let migrator =
        f.migrator
            .with_fence(fence.clone(), "migration:orders", Duration::from_secs(60));
    let err = migrator.backfill().await.unwrap_err();
    assert!(
        err.to_string()
            .contains("another migration holds the fence"),
        "got: {err}"
    );
    assert_eq!(row_count(&f.target).await, 0, "no rows may be copied");
    fence
        .release("migration:orders", &guard.token)
        .await
        .unwrap();
}
