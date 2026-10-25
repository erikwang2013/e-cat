// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 拓扑差异 → 搬移计划。区间分片按边界切区间；hash 重映射退回全表双写。
use crate::catalog::{ShardAlgorithm, ShardCatalog, ShardError, ShardTable};

/// 分片键区间；`low` 包含、`high` 不包含，`None` 表示无界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyRange {
    pub low: Option<i64>,
    pub high: Option<i64>,
}

/// 一次分片搬移；`key_range: None` 表示按新拓扑逐行路由的全表复制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardMove {
    pub table: String,
    pub from: String,
    pub to: String,
    pub key_range: Option<KeyRange>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShardPlan {
    pub moves: Vec<ShardMove>,
}

impl ShardPlan {
    /// 按表比较新旧目录；新增/删除表不产生搬移。
    pub fn diff(old: &ShardCatalog, new: &ShardCatalog) -> Result<Self, ShardError> {
        old.validate()?;
        new.validate()?;
        let mut moves = Vec::new();
        for new_table in &new.tables {
            let Some(old_table) = old.tables.iter().find(|t| t.name == new_table.name) else {
                continue;
            };
            if old_table.key != new_table.key || old_table.pk != new_table.pk {
                return Err(ShardError::Config(format!(
                    "table `{}` changes key/pk; migration is unsupported",
                    new_table.name
                )));
            }
            match (&old_table.algorithm, &new_table.algorithm) {
                (ShardAlgorithm::Range { .. }, ShardAlgorithm::Range { .. }) => {
                    moves.extend(range_moves(old_table, new_table)?);
                }
                (ShardAlgorithm::HashMod { .. }, ShardAlgorithm::HashMod { .. }) => {
                    if !same_shard_names(old_table, new_table) {
                        moves.extend(full_copy_moves(old_table, new_table));
                    }
                }
                _ => moves.extend(full_copy_moves(old_table, new_table)),
            }
        }
        Ok(Self { moves })
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }
}

fn same_shard_names(a: &ShardTable, b: &ShardTable) -> bool {
    a.shards
        .iter()
        .map(|s| s.name.as_str())
        .eq(b.shards.iter().map(|s| s.name.as_str()))
}

fn full_copy_moves(old: &ShardTable, new: &ShardTable) -> Vec<ShardMove> {
    let mut moves = Vec::new();
    for from in &old.shards {
        for to in &new.shards {
            moves.push(ShardMove {
                table: old.name.clone(),
                from: from.name.clone(),
                to: to.name.clone(),
                key_range: None,
            });
        }
    }
    moves
}

fn range_moves(old: &ShardTable, new: &ShardTable) -> Result<Vec<ShardMove>, ShardError> {
    let (old_boundaries, new_boundaries) = match (&old.algorithm, &new.algorithm) {
        (ShardAlgorithm::Range { boundaries: old }, ShardAlgorithm::Range { boundaries: new }) => {
            (old, new)
        }
        _ => {
            return Err(ShardError::Config(
                "range_moves called on a non-range algorithm".into(),
            ));
        }
    };
    let mut boundaries: Vec<i64> = old_boundaries
        .iter()
        .chain(new_boundaries.iter())
        .copied()
        .collect();
    boundaries.sort_unstable();
    boundaries.dedup();

    // (low, high, representative key) 覆盖整个 i64 域。
    let mut intervals = Vec::with_capacity(boundaries.len() + 1);
    let mut low = None;
    for boundary in &boundaries {
        if *boundary > i64::MIN {
            intervals.push((low, Some(*boundary), *boundary - 1));
        }
        low = Some(*boundary);
    }
    intervals.push((low, None, low.unwrap_or(0)));

    let mut moves: Vec<ShardMove> = Vec::new();
    for (low, high, representative) in intervals {
        let old_index = range_index(&old.algorithm, representative)?;
        let new_index = range_index(&new.algorithm, representative)?;
        let from = old
            .shards
            .get(old_index)
            .ok_or(ShardError::ShardCountMismatch)?
            .name
            .clone();
        let to = new
            .shards
            .get(new_index)
            .ok_or(ShardError::ShardCountMismatch)?
            .name
            .clone();
        if from == to {
            continue;
        }
        if let Some(last) = moves.last_mut()
            && last.from == from
            && last.to == to
            && let Some(range) = last.key_range.as_mut()
            && range.high == low
        {
            range.high = high;
            continue;
        }
        moves.push(ShardMove {
            table: old.name.clone(),
            from,
            to,
            key_range: Some(KeyRange { low, high }),
        });
    }
    Ok(moves)
}

fn range_index(algorithm: &ShardAlgorithm, key: i64) -> Result<usize, ShardError> {
    match algorithm {
        ShardAlgorithm::Range { boundaries } => {
            Ok(boundaries.partition_point(|boundary| *boundary <= key))
        }
        ShardAlgorithm::HashMod { .. } => Err(ShardError::Config(
            "range_index called on a hash algorithm".into(),
        )),
    }
}
