// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! ClickHouse 冷层 sink：规范化 NDJSON 的 SHA-256、`ecat_tier_checksums` 元数据、抽样回读。
//!
//! 校验和是规范化 NDJSON 的 SHA-256：按主键排序、列名按字典序、逐行
//! `serde_json::to_string`。这样热库 `SELECT *` 的列序与 ClickHouse JSONEachRow
//! 反序列化后的列序（字典序）不会改变结果。
use async_trait::async_trait;
use ecat_data::{Row, SqlExecutor};
use ecat_data_clickhouse::ClickhouseClient;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{render_sql_value, sql_string};
use crate::archive::ColdSink;
use crate::{TieringError, validate_ident};

/// 每个 chunk 一行：全量校验和 + 抽样回读所需字段。
const CHECKSUM_TABLE: &str = "ecat_tier_checksums";
// `ReplacingMergeTree` + SELECT 里的 `FINAL`：重试写同一 (table, partition, chunk)
// 时只留最新一行（内容相同则两行等价，内容不同则按插入顺序保留后者）。
const CHECKSUM_TABLE_DDL: &str = "CREATE TABLE IF NOT EXISTS ecat_tier_checksums \
     (`table` String, `partition` String, `chunk` UInt64, `checksum` String, \
     `sample_checksum` String, `sample_rows` UInt64, `low_id` Int64, `high_id` Int64) \
     ENGINE = ReplacingMergeTree ORDER BY (`table`, `partition`, `chunk`)";

/// 冷表白名单条目：表名 + 主键列（抽样校验按主键排序与取范围）。
#[derive(Debug, Clone)]
pub struct ClickhouseTable {
    pub name: String,
    pub id_col: String,
}

pub struct ClickhouseSink {
    client: ClickhouseClient,
    tables: Vec<ClickhouseTable>,
    verify_sample: Option<f32>,
    /// 元数据表只建一次；建表失败不缓存，下次写入重试。
    metadata_ready: tokio::sync::OnceCell<()>,
}

impl ClickhouseSink {
    /// `verify_sample` 为 `None`/`0` 时只校验全量 checksum；否则按比例抽样回读。
    pub fn new(
        client: ClickhouseClient,
        tables: Vec<ClickhouseTable>,
        verify_sample: Option<f32>,
    ) -> Result<Self, TieringError> {
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
        Ok(Self {
            client,
            tables,
            verify_sample,
            metadata_ready: tokio::sync::OnceCell::new(),
        })
    }

    /// 白名单查找 + 二次标识符校验；`ColdSink::write_chunk` 只给表名，主键列只能来自配置。
    fn table_spec(&self, table: &str) -> Result<&ClickhouseTable, TieringError> {
        validate_ident(table)?;
        self.tables.iter().find(|t| t.name == table).ok_or_else(|| {
            TieringError::Cold(format!(
                "table `{table}` is not in the ClickHouse sink whitelist"
            ))
        })
    }

    async fn ensure_metadata_table(&self) -> Result<(), TieringError> {
        self.metadata_ready
            .get_or_try_init(|| async {
                self.client.execute(CHECKSUM_TABLE_DDL).await?;
                Ok::<(), TieringError>(())
            })
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ColdSink for ClickhouseSink {
    async fn write_chunk(
        &self,
        table: &str,
        partition: &str,
        chunk: u64,
        rows: &[Row],
    ) -> Result<String, TieringError> {
        let spec = self.table_spec(table)?;
        validate_ident(partition)?;
        let digest = chunk_digest(rows, &spec.id_col, self.verify_sample)?;

        // 先用值拼出 INSERT；所有值都经 render_sql_value，表名已在白名单内。
        let mut values = String::new();
        for (i, row) in rows.iter().enumerate() {
            if i > 0 {
                values.push_str(", ");
            }
            values.push('(');
            for (j, (_, value)) in row.iter().enumerate() {
                if j > 0 {
                    values.push_str(", ");
                }
                values.push_str(&render_sql_value(value)?);
            }
            values.push(')');
        }
        self.ensure_metadata_table().await?;
        self.client
            .execute(&format!("INSERT INTO `{table}` VALUES {values}"))
            .await?;

        let metadata = format!(
            "INSERT INTO {CHECKSUM_TABLE} (`table`, `partition`, `chunk`, `checksum`, \
             `sample_checksum`, `sample_rows`, `low_id`, `high_id`) VALUES ({}, {}, {chunk}, {}, {}, {}, {}, {})",
            sql_string(table),
            sql_string(partition),
            sql_string(&digest.checksum),
            sql_string(&digest.sample_checksum),
            digest.sample_rows,
            digest.low_id,
            digest.high_id,
        );
        self.client.execute(&metadata).await?;
        Ok(digest.checksum)
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
        let rows = self
            .client
            .query(&format!(
                "SELECT `checksum`, `sample_checksum`, `sample_rows`, `low_id`, `high_id` \
                 FROM {CHECKSUM_TABLE} FINAL WHERE `table` = {} AND `partition` = {} AND `chunk` = {chunk}",
                sql_string(table),
                sql_string(partition),
            ))
            .await?;
        // 没有元数据行说明该 chunk 从未成功写入：按校验失败处理（不 DROP）。
        let Some(meta) = rows.first() else {
            return Ok(false);
        };
        let stored = meta
            .get("checksum")
            .and_then(Value::as_str)
            .ok_or_else(|| TieringError::Cold("checksum row has no `checksum`".into()))?;
        if stored != checksum {
            return Ok(false);
        }
        let Some(step) = self.verify_sample.map(sample_step) else {
            return Ok(true);
        };
        let sample_checksum = meta
            .get("sample_checksum")
            .and_then(Value::as_str)
            .ok_or_else(|| TieringError::Cold("checksum row has no `sample_checksum`".into()))?;
        let sample_rows = meta
            .get("sample_rows")
            .and_then(Value::as_u64)
            .ok_or_else(|| TieringError::Cold("checksum row has no `sample_rows`".into()))?;
        let low_id = meta
            .get("low_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| TieringError::Cold("checksum row has no `low_id`".into()))?;
        let high_id = meta
            .get("high_id")
            .and_then(Value::as_i64)
            .ok_or_else(|| TieringError::Cold("checksum row has no `high_id`".into()))?;

        // 只回读该 chunk 的 id 区间；chunk 按 `id > cursor ORDER BY id` 切分，
        // 同表不同 chunk 的 id 区间不重叠，所以区间内只有本 chunk 的行。
        let id = &spec.id_col;
        let cold = self
            .client
            .query(&format!(
                "SELECT * FROM `{table}` WHERE `{id}` >= {low_id} AND `{id}` <= {high_id} \
                 ORDER BY `{id}`"
            ))
            .await?;
        let keyed = sorted_by_id(&cold, id)?;
        let picked = pick_sample(&keyed, step);
        if picked.len() as u64 != sample_rows {
            return Ok(false);
        }
        let recomputed = sha256_hex(canonical_ndjson(&picked)?.as_bytes());
        Ok(recomputed == sample_checksum)
    }

    async fn health_check(&self) -> Result<(), TieringError> {
        self.client.query("SELECT 1").await?;
        Ok(())
    }
}

/// 单个 chunk 的校验和 + 抽样回读所需元数据。
#[derive(Debug, Default, PartialEq, Eq)]
struct ChunkDigest {
    checksum: String,
    sample_checksum: String,
    sample_rows: u64,
    low_id: i64,
    high_id: i64,
}

fn chunk_digest(
    rows: &[Row],
    id_col: &str,
    verify_sample: Option<f32>,
) -> Result<ChunkDigest, TieringError> {
    let keyed = sorted_by_id(rows, id_col)?;
    let (Some((low_id, _)), Some((high_id, _))) = (keyed.first(), keyed.last()) else {
        return Err(TieringError::Cold("cannot write an empty chunk".into()));
    };
    let ordered: Vec<&Row> = keyed.iter().map(|(_, row)| *row).collect();
    let checksum = sha256_hex(canonical_ndjson(&ordered)?.as_bytes());
    let (sample_checksum, sample_rows) = match verify_sample.filter(|rate| *rate > 0.0) {
        Some(rate) => {
            let picked = pick_sample(&keyed, sample_step(rate));
            (
                sha256_hex(canonical_ndjson(&picked)?.as_bytes()),
                picked.len() as u64,
            )
        }
        None => (String::new(), 0),
    };
    Ok(ChunkDigest {
        checksum,
        sample_checksum,
        sample_rows,
        low_id: *low_id,
        high_id: *high_id,
    })
}

fn sorted_by_id<'a>(rows: &'a [Row], id_col: &str) -> Result<Vec<(i64, &'a Row)>, TieringError> {
    let mut keyed: Vec<(i64, &Row)> = rows
        .iter()
        .map(|row| {
            let id = row.get(id_col).and_then(Value::as_i64).ok_or_else(|| {
                TieringError::Cold(format!("cold sink row has no integer `{id_col}`"))
            })?;
            Ok((id, row))
        })
        .collect::<Result<_, TieringError>>()?;
    keyed.sort_by_key(|(id, _)| *id);
    Ok(keyed)
}

/// 规范化 NDJSON：列名按字典序，值用 serde_json 序列化（字符串/数值/布尔/NULL 直出）。
fn canonical_ndjson(rows: &[&Row]) -> Result<String, TieringError> {
    let mut out = String::new();
    for row in rows {
        out.push('{');
        let mut columns: Vec<(&str, &Value)> = row.iter().collect();
        columns.sort_unstable_by(|a, b| a.0.cmp(b.0));
        for (i, (col, value)) in columns.into_iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let key = serde_json::to_string(col).map_err(|e| TieringError::Cold(e.to_string()))?;
            let value =
                serde_json::to_string(value).map_err(|e| TieringError::Cold(e.to_string()))?;
            out.push_str(&key);
            out.push(':');
            out.push_str(&value);
        }
        out.push_str("}\n");
    }
    Ok(out)
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

/// 按 id 取模选样，保证至少 1 行：取模规则只依赖 id 值，冷表里有过滤缺口时
/// 两侧仍选中同一批行；无命中时回退到最小 id 行（该行必然已写入冷层）。
fn pick_sample<'a>(keyed: &[(i64, &'a Row)], step: u64) -> Vec<&'a Row> {
    let step = i64::try_from(step).unwrap_or(i64::MAX);
    let picked: Vec<&Row> = keyed
        .iter()
        .filter(|(id, _)| id.rem_euclid(step) == 0)
        .map(|(_, row)| *row)
        .collect();
    if picked.is_empty() {
        keyed.iter().take(1).map(|(_, row)| *row).collect()
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
