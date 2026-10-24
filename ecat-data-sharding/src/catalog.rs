// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 分片目录：拓扑校验与按分片键路由。
use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value;

#[cfg(test)]
mod tests;

/// 分片目录；整个 `sharding:` 配置的表拓扑。
#[derive(Debug, Clone, Deserialize)]
pub struct ShardCatalog {
    pub tables: Vec<ShardTable>,
}

/// 单表的分片方式与端点。
#[derive(Debug, Clone, Deserialize)]
pub struct ShardTable {
    pub name: String,
    /// 分片键列名。
    pub key: String,
    /// 主键列名。
    pub pk: String,
    pub algorithm: ShardAlgorithm,
    pub shards: Vec<ShardEndpoint>,
}

/// 一个分片端点；`url` 只在 `Client` 模式使用。
#[derive(Debug, Clone, Deserialize)]
pub struct ShardEndpoint {
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// 分片算法。
#[derive(Debug, Clone, Deserialize)]
pub enum ShardAlgorithm {
    /// 取模分片。哈希必须与代理侧配置的算法逐字节一致；代理若用 Java
    /// `hashCode` 等不可移植算法，A 阶段按设计 §4.1 重新分片，不假设天然一致。
    HashMod { shards: u32 },
    /// 区间分片；`boundaries` 是后续分片的下界，长度必须等于分片数 - 1。
    Range { boundaries: Vec<i64> },
}

/// 分片配置与路由错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ShardError {
    #[error("sharding config error: {0}")]
    Config(String),
    #[error("missing shard key")]
    MissingKey,
    #[error("unknown shard table: {0}")]
    UnknownTable(String),
    #[error("shard count mismatch")]
    ShardCountMismatch,
    #[error("partial failure on shards: {failed:?}")]
    PartialFailure { failed: Vec<String> },
    #[error("clock rollback")]
    ClockRollback,
    #[error("id out of range")]
    IdOutOfRange,
    #[error("unsupported database dialect")]
    UnsupportedDialect,
}

/// 对分片键做 FNV-1a 64 位哈希。
///
/// 字符串键取原始 UTF-8 字节；其它键先规范化（对象按键名排序）再哈希。
/// 结果必须与代理侧配置的算法一致；不可移植时 A 阶段按设计 §4.1 重新分片。
pub fn stable_hash64(key: &Value) -> Result<u64, ShardError> {
    match key {
        Value::Null => Err(ShardError::MissingKey),
        Value::String(text) => Ok(fnv1a64(text.as_bytes())),
        other => {
            let mut canonical = String::new();
            write_canonical(other, &mut canonical);
            Ok(fnv1a64(canonical.as_bytes()))
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// 规范化 JSON 文本：对象键排序、数组保持顺序，嵌套字符串按 JSON 转义。
fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => out.push_str(&Value::String(text.clone()).to_string()),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(key, _)| *key);
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(item, out);
            }
            out.push('}');
        }
    }
}

impl ShardCatalog {
    /// 校验表名/键/主键、分片数量、端点唯一性与区间边界。
    pub fn validate(&self) -> Result<(), ShardError> {
        let mut table_names = HashSet::new();
        for table in &self.tables {
            if [&table.name, &table.key, &table.pk]
                .iter()
                .any(|field| field.trim().is_empty())
            {
                return Err(ShardError::Config(format!(
                    "table `{}` has an empty name, key, or pk",
                    table.name
                )));
            }
            if !table_names.insert(table.name.as_str()) {
                return Err(ShardError::Config(format!(
                    "duplicate table name `{}`",
                    table.name
                )));
            }
            if table.shards.is_empty() {
                return Err(ShardError::Config(format!(
                    "table `{}` has no shards",
                    table.name
                )));
            }
            let mut shard_names = HashSet::new();
            for shard in &table.shards {
                if !shard_names.insert(shard.name.as_str()) {
                    return Err(ShardError::Config(format!(
                        "table `{}` has duplicate shard name `{}`",
                        table.name, shard.name
                    )));
                }
            }
            match &table.algorithm {
                ShardAlgorithm::HashMod { shards } => {
                    if *shards == 0 || *shards as usize != table.shards.len() {
                        return Err(ShardError::ShardCountMismatch);
                    }
                }
                ShardAlgorithm::Range { boundaries } => {
                    if boundaries.len() + 1 != table.shards.len() {
                        return Err(ShardError::ShardCountMismatch);
                    }
                    if boundaries.windows(2).any(|pair| pair[0] >= pair[1]) {
                        return Err(ShardError::Config(format!(
                            "table `{}` range boundaries must be strictly increasing",
                            table.name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// 返回表 `table` 上键 `key` 命中的分片下标；`Null` 键直接报错，不猜。
    pub fn shard_for(&self, table: &str, key: &Value) -> Result<usize, ShardError> {
        let table = self.table(table)?;
        if key.is_null() {
            return Err(ShardError::MissingKey);
        }
        match &table.algorithm {
            ShardAlgorithm::HashMod { shards } => {
                if *shards == 0 || *shards as usize != table.shards.len() {
                    return Err(ShardError::ShardCountMismatch);
                }
                Ok((stable_hash64(key)? % u64::from(*shards)) as usize)
            }
            ShardAlgorithm::Range { boundaries } => {
                if boundaries.len() + 1 != table.shards.len() {
                    return Err(ShardError::ShardCountMismatch);
                }
                let Some(id) = key.as_i64() else {
                    return Err(ShardError::Config(format!(
                        "table `{}` range shard key must be an integer",
                        table.name
                    )));
                };
                Ok(boundaries.partition_point(|boundary| *boundary <= id))
            }
        }
    }

    /// 按名字查表。
    pub fn table(&self, name: &str) -> Result<&ShardTable, ShardError> {
        self.tables
            .iter()
            .find(|table| table.name == name)
            .ok_or_else(|| ShardError::UnknownTable(name.to_owned()))
    }
}
