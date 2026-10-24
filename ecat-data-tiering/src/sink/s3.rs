// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! S3 冷层 sink：整对象 NDJSON + 每 chunk 一份 manifest（提交点）+ 下载字节的 SHA-256。
//!
//! 幂等靠固定 key：写前用 `list` 判存在，已存在的 chunk 不重传；manifest 每次都写，
//! 它是 commit point，也要能把「chunk 已写、manifest 丢了」的重试补齐。
//! 抽样校验和只依赖按 id 排序后的行文本，输入行序不影响结果。
use async_trait::async_trait;
use ecat_data::{Row, StorageClient};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::archive::ColdSink;
use crate::{TieringError, validate_ident};

/// `StorageClient` 整对象在内存；brief 指定单 chunk 上限 64MB。
const MAX_CHUNK_BYTES: usize = 64 * 1024 * 1024;
/// 每 chunk 一份 manifest：分区级单文件会被后续 chunk 覆盖，重试校验旧 chunk 就失效。
const MANIFEST_SUFFIX: &str = ".manifest.json";

/// 冷表白名单条目：表名 + 主键列（抽样按主键排序与选样）。
#[derive(Debug, Clone)]
pub struct S3Table {
    pub name: String,
    pub id_col: String,
}

pub struct S3Sink {
    client: Arc<dyn StorageClient>,
    bucket: String,
    /// 归一化为「无首尾 `/`，非空时带尾 `/`」的 key 根。
    prefix: String,
    tables: Vec<S3Table>,
    verify_sample: Option<f32>,
}

/// 每 chunk 一份的提交点；抽样字段是回读重算的依据。
#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    table: String,
    partition: String,
    chunk: u64,
    checksum: String,
    bytes: u64,
    rows: u64,
    sample_step: u64,
    sample_checksum: String,
    sample_rows: u64,
}

/// 单个 chunk 的落盘字节 + manifest 字段。
struct ChunkObject {
    ndjson: Vec<u8>,
    checksum: String,
    rows: u64,
    sample_step: u64,
    sample_checksum: String,
    sample_rows: u64,
}

impl S3Sink {
    /// `verify_sample` 为 `None`/`0` 时只校验全量 checksum；否则按 id 抽样回读。
    pub fn new(
        client: Arc<dyn StorageClient>,
        bucket: impl Into<String>,
        prefix: impl Into<String>,
        tables: Vec<S3Table>,
        verify_sample: Option<f32>,
    ) -> Result<Self, TieringError> {
        let bucket = bucket.into();
        if bucket.is_empty() {
            return Err(TieringError::Config("s3 bucket must not be empty".into()));
        }
        for table in &tables {
            validate_ident(&table.name)?;
            validate_ident(&table.id_col)?;
        }
        let verify_sample = match verify_sample {
            Some(rate) if !rate.is_finite() || !(0.0..=1.0).contains(&rate) => {
                return Err(TieringError::Config(format!(
                    "verify_sample must be in [0, 1], got {rate}"
                )));
            }
            Some(0.0) => None,
            other => other,
        };
        let prefix = prefix.into();
        let prefix = prefix.trim_matches('/');
        let prefix = if prefix.is_empty() {
            String::new()
        } else {
            format!("{prefix}/")
        };
        Ok(Self {
            client,
            bucket,
            prefix,
            tables,
            verify_sample,
        })
    }

    /// 白名单查找 + 二次标识符校验；`ColdSink::write_chunk` 只给表名，主键列只能来自配置。
    fn table_spec(&self, table: &str) -> Result<&S3Table, TieringError> {
        validate_ident(table)?;
        self.tables.iter().find(|t| t.name == table).ok_or_else(|| {
            TieringError::Cold(format!("table `{table}` is not in the S3 sink whitelist"))
        })
    }

    fn base_key(&self, table: &str, partition: &str) -> String {
        format!("{}{table}/{partition}", self.prefix)
    }

    fn chunk_key(&self, table: &str, partition: &str, chunk: u64) -> String {
        format!("{}/{chunk}.ndjson", self.base_key(table, partition))
    }

    fn manifest_key(&self, table: &str, partition: &str, chunk: u64) -> String {
        format!(
            "{}/{chunk}{MANIFEST_SUFFIX}",
            self.base_key(table, partition)
        )
    }

    async fn put(&self, key: &str, data: &[u8]) -> Result<(), TieringError> {
        self.client
            .put(&self.bucket, key, data)
            .await
            .map_err(|e| TieringError::Cold(format!("s3 put `{key}`: {e}")))
    }

    async fn get(&self, key: &str) -> Result<Vec<u8>, TieringError> {
        self.client
            .get(&self.bucket, key)
            .await
            .map_err(|e| TieringError::Cold(format!("s3 get `{key}`: {e}")))
    }

    /// 精确 key 存在性：用同 key 的前缀 list，避免把整对象下载下来。
    async fn exists(&self, key: &str) -> Result<bool, TieringError> {
        let keys = self
            .client
            .list(&self.bucket, key)
            .await
            .map_err(|e| TieringError::Cold(format!("s3 list `{key}`: {e}")))?;
        Ok(keys.iter().any(|existing| existing.as_str() == key))
    }
}

#[async_trait]
impl ColdSink for S3Sink {
    async fn write_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        rows: &[Row],
    ) -> Result<String, TieringError> {
        let spec = self.table_spec(table)?;
        validate_ident(partition)?;
        let object = build_chunk(rows, &spec.id_col, self.verify_sample)?;
        ensure_chunk_size(object.ndjson.len())?;

        let key = self.chunk_key(table, partition, chunk);
        if !self.exists(&key).await? {
            self.put(&key, &object.ndjson).await?;
        }
        let manifest = Manifest {
            table: table.to_string(),
            partition: partition.to_string(),
            chunk,
            checksum: object.checksum.clone(),
            bytes: object.ndjson.len() as u64,
            rows: object.rows,
            sample_step: object.sample_step,
            sample_checksum: object.sample_checksum.clone(),
            sample_rows: object.sample_rows,
        };
        let encoded = serde_json::to_vec(&manifest)
            .map_err(|e| TieringError::Cold(format!("s3 manifest encode: {e}")))?;
        self.put(&self.manifest_key(table, partition, chunk), &encoded)
            .await?;
        Ok(object.checksum)
    }

    async fn verify_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        checksum: &str,
    ) -> Result<bool, TieringError> {
        let spec = self.table_spec(table)?;
        validate_ident(partition)?;
        let bytes = self.get(&self.chunk_key(table, partition, chunk)).await?;
        if sha256_hex(&bytes) != checksum {
            return Ok(false);
        }
        // 不抽样时只比全量 checksum：manifest 都不读，也就没有额外对象读。
        if self.verify_sample.is_none() {
            return Ok(true);
        }

        let manifest_key = self.manifest_key(table, partition, chunk);
        if !self.exists(&manifest_key).await? {
            return Ok(false);
        }
        let manifest: Manifest = serde_json::from_slice(&self.get(&manifest_key).await?)
            .map_err(|e| TieringError::Cold(format!("s3 manifest `{manifest_key}`: {e}")))?;
        if manifest.table != table
            || manifest.partition != partition
            || manifest.chunk != chunk
            || manifest.checksum != checksum
            || manifest.sample_step == 0
            || manifest.bytes != bytes.len() as u64
        {
            return Ok(false);
        }
        let keyed = parse_keyed_lines(&bytes, &spec.id_col)?;
        if keyed.len() as u64 != manifest.rows {
            return Ok(false);
        }
        let picked = pick_sample(&keyed, manifest.sample_step);
        let mut sample = String::new();
        for line in &picked {
            sample.push_str(line);
            sample.push('\n');
        }
        Ok(picked.len() as u64 == manifest.sample_rows
            && sha256_hex(sample.as_bytes()) == manifest.sample_checksum)
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        self.client
            .list(&self.bucket, &self.prefix)
            .await
            .map_err(|e| TieringError::Cold(format!("s3 list `{}`: {e}", self.prefix)))?;
        Ok(())
    }
}

/// 行按 id 排序后逐行 `serde_json::to_string(row)`，尾行也带 `\n`。
fn build_chunk(
    rows: &[Row],
    id_col: &str,
    verify_sample: Option<f32>,
) -> Result<ChunkObject, TieringError> {
    if rows.is_empty() {
        return Err(TieringError::Cold("cannot write an empty chunk".into()));
    }
    let mut keyed: Vec<(i64, String)> = rows
        .iter()
        .map(|row| {
            let id = row.get(id_col).and_then(json_i64).ok_or_else(|| {
                TieringError::Cold(format!("cold sink row has no integer `{id_col}`"))
            })?;
            Ok((id, row_json(row)?))
        })
        .collect::<Result<_, TieringError>>()?;
    keyed.sort_by_key(|(id, _)| *id);

    let mut ndjson = String::new();
    for (_, line) in &keyed {
        ndjson.push_str(line);
        ndjson.push('\n');
    }
    let (sample_step, sample_checksum, sample_rows) = match verify_sample.filter(|r| *r > 0.0) {
        Some(rate) => {
            let step = sample_step(rate);
            let picked = pick_sample(&keyed, step);
            let mut sample = String::new();
            for line in &picked {
                sample.push_str(line);
                sample.push('\n');
            }
            (step, sha256_hex(sample.as_bytes()), picked.len() as u64)
        }
        None => (0, String::new(), 0),
    };
    Ok(ChunkObject {
        checksum: sha256_hex(ndjson.as_bytes()),
        ndjson: ndjson.into_bytes(),
        rows: rows.len() as u64,
        sample_step,
        sample_checksum,
        sample_rows,
    })
}

fn row_json(row: &Row) -> Result<String, TieringError> {
    let mut map = serde_json::Map::new();
    for (col, value) in row.columns().iter().zip(row.values()) {
        map.insert(col.clone(), value.clone());
    }
    serde_json::to_string(&map)
        .map_err(|e| TieringError::Cold(format!("failed to serialize S3 chunk row: {e}")))
}

fn ensure_chunk_size(len: usize) -> Result<(), TieringError> {
    if len > MAX_CHUNK_BYTES {
        return Err(TieringError::Cold(format!(
            "chunk is {len} bytes, exceeding the {MAX_CHUNK_BYTES} byte S3 sink limit"
        )));
    }
    Ok(())
}

/// 回读路径：解析已落盘的 NDJSON 行并按 id 排序，行文本原样参与样本哈希。
fn parse_keyed_lines<'a>(
    bytes: &'a [u8],
    id_col: &str,
) -> Result<Vec<(i64, &'a str)>, TieringError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|e| TieringError::Cold(format!("s3 chunk is not utf-8: {e}")))?;
    let mut keyed = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        let value: Value = serde_json::from_str(line)
            .map_err(|e| TieringError::Cold(format!("s3 chunk line is not json: {e}")))?;
        let id = value.get(id_col).and_then(json_i64).ok_or_else(|| {
            TieringError::Cold(format!("s3 chunk line has no integer `{id_col}`"))
        })?;
        keyed.push((id, line));
    }
    keyed.sort_by_key(|(id, _)| *id);
    Ok(keyed)
}

/// 主键可能是十进制字符串（与 ClickHouse sink 同口径），数字与字符串两种形态都接受。
fn json_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// 抽样步长 `ceil(1/rate)`；`rate = 1.0` 抽全量。非法 rate 退化为 1（全量）。
fn sample_step(rate: f32) -> u64 {
    let step = (1.0 / f64::from(rate)).ceil();
    if step.is_finite() && step >= 1.0 {
        step as u64
    } else {
        1
    }
}

/// 按 id 取模选样，保证至少 1 行：规则只依赖 id 值，不依赖行在输入里的名次。
fn pick_sample<T: AsRef<str>>(keyed: &[(i64, T)], step: u64) -> Vec<&str> {
    let step = i64::try_from(step).unwrap_or(i64::MAX).max(1);
    let picked: Vec<&str> = keyed
        .iter()
        .filter(|(id, _)| id.rem_euclid(step) == 0)
        .map(|(_, line)| line.as_ref())
        .collect();
    if picked.is_empty() {
        keyed
            .iter()
            .take(1)
            .map(|(_, line)| line.as_ref())
            .collect()
    } else {
        picked
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
