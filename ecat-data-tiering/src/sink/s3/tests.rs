// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! S3 sink 单测：内存 `FakeStorage` 记录对象、操作顺序与 GET 次数。
use super::*;
use ecat_data_s3::{S3Client, S3Config};
use ecat_errors::{Error, ErrorCode};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

const BUCKET: &str = "bucket";
const PREFIX: &str = "cold";
const TABLE: &str = "events";
const PARTITION: &str = "p2026_01";
const ID_COL: &str = "id";

/// 锁中毒不该掩盖真正失败的断言。
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct FakeStorage {
    objects: Mutex<HashMap<(String, String), Vec<u8>>>,
    ops: Mutex<Vec<String>>,
    gets: Mutex<HashMap<String, usize>>,
}

impl FakeStorage {
    fn insert(&self, bucket: &str, key: &str, data: &[u8]) {
        lock(&self.objects).insert((bucket.to_string(), key.to_string()), data.to_vec());
    }

    fn object(&self, key: &str) -> Option<Vec<u8>> {
        lock(&self.objects)
            .get(&(BUCKET.to_string(), key.to_string()))
            .cloned()
    }

    fn ops(&self) -> Vec<String> {
        lock(&self.ops).clone()
    }

    fn get_count(&self, key: &str) -> usize {
        lock(&self.gets).get(key).copied().unwrap_or(0)
    }

    fn reset(&self) {
        lock(&self.ops).clear();
        lock(&self.gets).clear();
    }
}

#[async_trait]
impl StorageClient for FakeStorage {
    async fn put(&self, bucket: &str, key: &str, data: &[u8]) -> Result<(), Error> {
        lock(&self.ops).push(format!("put {key}"));
        self.insert(bucket, key, data);
        Ok(())
    }

    async fn get(&self, bucket: &str, key: &str) -> Result<Vec<u8>, Error> {
        lock(&self.ops).push(format!("get {key}"));
        *lock(&self.gets).entry(key.to_string()).or_default() += 1;
        lock(&self.objects)
            .get(&(bucket.to_string(), key.to_string()))
            .cloned()
            .ok_or_else(|| Error::new(ErrorCode::Internal, "fake", format!("missing `{key}`")))
    }

    async fn delete(&self, bucket: &str, key: &str) -> Result<(), Error> {
        lock(&self.ops).push(format!("delete {key}"));
        lock(&self.objects).remove(&(bucket.to_string(), key.to_string()));
        Ok(())
    }

    async fn list(&self, bucket: &str, prefix: &str) -> Result<Vec<String>, Error> {
        lock(&self.ops).push(format!("list {prefix}"));
        let mut keys: Vec<String> = lock(&self.objects)
            .keys()
            .filter(|(b, k)| b.as_str() == bucket && k.starts_with(prefix))
            .map(|(_, k)| k.to_string())
            .collect();
        keys.sort();
        Ok(keys)
    }
}

fn make_sink(storage: &Arc<FakeStorage>, sample: Option<f32>) -> S3Sink {
    let storage: Arc<dyn StorageClient> = storage.clone();
    let tables = vec![S3Table {
        name: TABLE.into(),
        id_col: ID_COL.into(),
    }];
    S3Sink::new(storage, BUCKET, PREFIX, tables, sample).expect("valid s3 sink config")
}

fn row(id: i64, payload: &str) -> Row {
    let columns = vec![ID_COL.into(), "payload".into()];
    Row::new(columns, vec![json!(id), json!(payload)])
}

fn rows(ids: &[i64]) -> Vec<Row> {
    ids.iter().map(|id| row(*id, &format!("p{id}"))).collect()
}

fn manifest(storage: &FakeStorage, sink: &S3Sink, chunk: u64) -> Value {
    let key = sink.manifest_key(TABLE, PARTITION, chunk);
    let object = storage.object(&key).expect("manifest object");
    serde_json::from_slice(&object).expect("manifest json")
}

#[tokio::test]
async fn s3_write_chunk_puts_ndjson_and_manifest() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 7, &rows(&[2, 1]))
        .await
        .expect("write");
    let key = sink.chunk_key(TABLE, PARTITION, 7);
    let object = storage.object(&key).expect("chunk object");
    // 每行是 `serde_json::to_string(row)`，行按 id 排序（抽样按 id 序才可复现）。
    let text = String::from_utf8(object.clone()).expect("utf-8 ndjson");
    assert_eq!(
        text,
        "{\"id\":1,\"payload\":\"p1\"}\n{\"id\":2,\"payload\":\"p2\"}\n"
    );
    assert_eq!(checksum, sha256_hex(&object));

    let manifest = manifest(&storage, &sink, 7);
    assert_eq!(manifest["table"], TABLE);
    assert_eq!(manifest["partition"], PARTITION);
    assert_eq!(manifest["chunk"], 7);
    assert_eq!(manifest["checksum"], checksum.as_str());
    assert_eq!(manifest["rows"], 2);
    assert_eq!(manifest["bytes"], object.len());
    assert_eq!(manifest["sample_rows"], 0);
    assert_eq!(manifest["sample_checksum"], "");
    assert_eq!(manifest["sample_step"], 0);
}

#[tokio::test]
async fn s3_manifest_written_last() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    sink.write_chunk(TABLE, PARTITION, 0, &rows(&[1]))
        .await
        .expect("write");
    let ops = storage.ops();
    let chunk_put = format!("put {}", sink.chunk_key(TABLE, PARTITION, 0));
    let manifest_put = format!("put {}", sink.manifest_key(TABLE, PARTITION, 0));
    let chunk_at = ops
        .iter()
        .position(|op| *op == chunk_put)
        .expect("chunk put");
    let manifest_at = ops
        .iter()
        .position(|op| *op == manifest_put)
        .expect("manifest put");
    assert!(chunk_at < manifest_at);
    assert_eq!(ops.last().expect("ops"), &manifest_put);
}

#[tokio::test]
async fn s3_existing_key_skips_rewrite() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    let key = sink.chunk_key(TABLE, PARTITION, 0);
    let existing = b"{\"id\":1,\"payload\":\"p1\"}\n";
    storage.insert(BUCKET, &key, existing);
    storage.reset();

    sink.write_chunk(TABLE, PARTITION, 0, &rows(&[1]))
        .await
        .expect("write");
    let ops = storage.ops();
    assert!(
        !ops.iter().any(|op| *op == format!("put {key}")),
        "已存在的 chunk 不重传"
    );
    let manifest_put = format!("put {}", sink.manifest_key(TABLE, PARTITION, 0));
    assert!(ops.contains(&manifest_put), "manifest 是提交点，仍要写");
    assert_eq!(storage.object(&key).as_deref(), Some(existing.as_slice()));
}

#[tokio::test]
async fn s3_checksum_matches_downloaded_bytes() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 0, &rows(&[1, 2]))
        .await
        .expect("write");
    assert!(
        sink.verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
    assert!(
        !sink
            .verify_chunk(TABLE, PARTITION, 0, "deadbeef")
            .await
            .expect("verify")
    );
    storage.insert(BUCKET, &sink.chunk_key(TABLE, PARTITION, 0), b"tampered\n");
    assert!(
        !sink
            .verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
}

#[tokio::test]
async fn s3_sample_checksum_is_stable_and_keeps_one_row() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, Some(0.5));
    // 输入行序不同，落盘的 NDJSON 与抽样校验和必须相同。
    sink.write_chunk(TABLE, PARTITION, 0, &rows(&[3, 1, 2]))
        .await
        .expect("write");
    sink.write_chunk(TABLE, PARTITION, 1, &rows(&[2, 3, 1]))
        .await
        .expect("write");
    let first = manifest(&storage, &sink, 0);
    let second = manifest(&storage, &sink, 1);
    assert_eq!(first["checksum"], second["checksum"]);
    assert_eq!(first["sample_checksum"], second["sample_checksum"]);
    assert_eq!(first["sample_step"], 2);
    assert_eq!(first["sample_rows"], 1); // step 2：id 1..3 里只有 id 2 命中

    // rate 0.01 -> ceil(1/rate) = 101，大于所有 id -> 回退最小 id 行，仍 ≥ 1 行。
    let sparse = make_sink(&storage, Some(0.01));
    sparse
        .write_chunk(TABLE, PARTITION, 2, &rows(&[1, 2, 3]))
        .await
        .expect("write");
    let sparse_manifest = manifest(&storage, &sparse, 2);
    assert!(sparse_manifest["sample_step"].as_u64().expect("step") > 3);
    assert_eq!(sparse_manifest["sample_rows"], 1);
    assert_ne!(sparse_manifest["sample_checksum"], "");
}

#[tokio::test]
async fn s3_sample_mismatch_returns_false() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, Some(0.5));
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 0, &rows(&[1, 2, 3]))
        .await
        .expect("write");
    assert!(
        sink.verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );

    // 全量 checksum 仍对得上（对象没动），只有样本元数据被改 -> false。
    let key = sink.manifest_key(TABLE, PARTITION, 0);
    let mut manifest = manifest(&storage, &sink, 0);
    let sample_checksum = manifest["sample_checksum"].clone();
    manifest["sample_checksum"] = json!("deadbeef");
    storage.insert(
        BUCKET,
        &key,
        &serde_json::to_vec(&manifest).expect("manifest"),
    );
    assert!(
        !sink
            .verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );

    manifest["sample_checksum"] = sample_checksum;
    manifest["sample_rows"] = json!(99);
    storage.insert(
        BUCKET,
        &key,
        &serde_json::to_vec(&manifest).expect("manifest"),
    );
    assert!(
        !sink
            .verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
}

#[tokio::test]
async fn s3_sample_disabled_reads_object_once() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 0, &rows(&[1, 2]))
        .await
        .expect("write");
    storage.reset();
    assert!(
        sink.verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
    assert_eq!(storage.get_count(&sink.chunk_key(TABLE, PARTITION, 0)), 1);
    let gets = storage
        .ops()
        .iter()
        .filter(|op| op.starts_with("get "))
        .count();
    assert_eq!(gets, 1, "不抽样时只读一次对象，不碰 manifest");
}

#[tokio::test]
async fn s3_sample_enabled_reads_object_once() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, Some(0.5));
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 0, &rows(&[1, 2, 3]))
        .await
        .expect("write");
    storage.reset();
    assert!(
        sink.verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
    let chunk_gets = storage.get_count(&sink.chunk_key(TABLE, PARTITION, 0));
    assert_eq!(chunk_gets, 1, "抽样复用同一次对象下载，不额外再 GET");
    assert_eq!(
        storage.get_count(&sink.manifest_key(TABLE, PARTITION, 0)),
        1
    );
}

#[tokio::test]
async fn s3_sample_verify_without_manifest_returns_false() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, Some(0.5));
    let checksum = sink
        .write_chunk(TABLE, PARTITION, 0, &rows(&[1, 2, 3]))
        .await
        .expect("write");
    let key = sink.manifest_key(TABLE, PARTITION, 0);
    storage.delete(BUCKET, &key).await.expect("delete");
    assert!(
        !sink
            .verify_chunk(TABLE, PARTITION, 0, &checksum)
            .await
            .expect("verify")
    );
}

#[tokio::test]
async fn s3_health_check_lists_prefix() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    sink.health_check().await.expect("health");
    assert!(storage.ops().contains(&"list cold/".to_string()));
}

#[test]
fn s3_constructor_rejects_bad_config() {
    let storage: Arc<dyn StorageClient> = Arc::new(FakeStorage::default());
    let table = |name: &str, id_col: &str| S3Table {
        name: name.into(),
        id_col: id_col.into(),
    };
    let new = |tables: Vec<S3Table>, rate: Option<f32>| {
        S3Sink::new(Arc::clone(&storage), BUCKET, PREFIX, tables, rate)
    };
    assert!(matches!(
        new(vec![table("bad-name", ID_COL)], None),
        Err(TieringError::Config(_))
    ));
    assert!(matches!(
        new(vec![table(TABLE, "1bad")], None),
        Err(TieringError::Config(_))
    ));
    assert!(matches!(
        new(vec![table(TABLE, ID_COL)], Some(1.5)),
        Err(TieringError::Config(_))
    ));
    // rate 0 归一为「不抽样」。
    assert!(new(vec![table(TABLE, ID_COL)], Some(0.0)).is_ok());
    let empty_bucket = S3Sink::new(
        Arc::clone(&storage),
        "",
        PREFIX,
        vec![table(TABLE, ID_COL)],
        None,
    );
    assert!(matches!(empty_bucket, Err(TieringError::Config(_))));
}

#[tokio::test]
async fn s3_rejects_unknown_table_and_invalid_inputs() {
    let storage = Arc::new(FakeStorage::default());
    let sink = make_sink(&storage, None);
    assert!(matches!(
        sink.write_chunk("other", PARTITION, 0, &rows(&[1])).await,
        Err(TieringError::Cold(_))
    ));
    assert!(matches!(
        sink.write_chunk(TABLE, "bad-name", 0, &rows(&[1])).await,
        Err(TieringError::Config(_))
    ));
    assert!(matches!(
        sink.verify_chunk("other", PARTITION, 0, "x").await,
        Err(TieringError::Cold(_))
    ));
    assert!(matches!(
        sink.write_chunk(TABLE, PARTITION, 0, &[]).await,
        Err(TieringError::Cold(_))
    ));
    let no_id = Row::new(vec!["payload".into()], vec![json!("p")]);
    assert!(matches!(
        sink.write_chunk(TABLE, PARTITION, 0, &[no_id]).await,
        Err(TieringError::Cold(_))
    ));
}

#[test]
fn s3_chunk_size_limit_is_64mb() {
    assert_eq!(MAX_CHUNK_BYTES, 64 * 1024 * 1024);
    assert!(ensure_chunk_size(MAX_CHUNK_BYTES).is_ok());
    assert!(matches!(
        ensure_chunk_size(MAX_CHUNK_BYTES + 1),
        Err(TieringError::Cold(_))
    ));
}

/// live 用例：`ECAT_TEST_S3_*` 未设置时跳过（不是失败）。
#[tokio::test]
async fn s3_live_write_verify_roundtrip() {
    let Some((config, bucket)) = live_config() else {
        eprintln!("skip s3_live_write_verify_roundtrip: ECAT_TEST_S3_* unset");
        return;
    };
    let table = "ecat_tiering_live_test";
    let prefix = format!("tiering-live-{}", std::process::id());
    let client = Arc::new(S3Client::from_config(config).expect("s3 client"));
    let tables = vec![S3Table {
        name: table.into(),
        id_col: ID_COL.into(),
    }];
    let sink = S3Sink::new(
        Arc::clone(&client) as Arc<dyn StorageClient>,
        bucket.as_str(),
        prefix.as_str(),
        tables,
        Some(0.5),
    )
    .expect("sink");
    sink.health_check().await.expect("health");
    let checksum = sink
        .write_chunk(table, "p1", 0, &rows(&[1, 2, 3]))
        .await
        .expect("write");
    assert!(
        sink.verify_chunk(table, "p1", 0, &checksum)
            .await
            .expect("verify")
    );
    assert!(
        !sink
            .verify_chunk(table, "p1", 0, "deadbeef")
            .await
            .expect("verify")
    );
    client
        .delete(&bucket, &sink.chunk_key(table, "p1", 0))
        .await
        .expect("cleanup chunk");
    let manifest_key = sink.manifest_key(table, "p1", 0);
    client
        .delete(&bucket, &manifest_key)
        .await
        .expect("cleanup manifest");
}

fn live_config() -> Option<(S3Config, String)> {
    Some((
        S3Config {
            endpoint: std::env::var("ECAT_TEST_S3_ENDPOINT").ok()?,
            region: std::env::var("ECAT_TEST_S3_REGION").unwrap_or_else(|_| "us-east-1".into()),
            access_key: std::env::var("ECAT_TEST_S3_ACCESS_KEY").ok()?,
            secret_key: std::env::var("ECAT_TEST_S3_SECRET_KEY").ok()?,
            tls: None,
            query_timeout_secs: None,
            breaker: None,
            max_concurrency: None,
        },
        std::env::var("ECAT_TEST_S3_BUCKET").ok()?,
    ))
}
