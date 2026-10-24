// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 冷热分层配置：顶层 `tiering:` 段 + 每表保留策略。
use ecat_data::RdbmsError;
use serde::Deserialize;

fn default_hot_days() -> u32 {
    180
}

fn default_partition_months() -> u32 {
    1
}

fn default_batch_size() -> u32 {
    1000
}

fn default_retain_s3_days() -> u32 {
    7
}

/// 顶层 `tiering:` 配置段；`enabled` 默认关闭，旧配置零变化。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TieringConfig {
    #[serde(default)]
    pub enabled: bool,
    /// 热数据保留天数，超过即进入归档候选；必须 > 0。
    #[serde(default = "default_hot_days")]
    pub hot_days: u32,
    /// 分区粒度（月）；必须 > 0。
    #[serde(default = "default_partition_months")]
    pub partition_months: u32,
    #[serde(default)]
    pub tables: Vec<TableTieringConfig>,
    /// 每批读取的行数。
    #[serde(default = "default_batch_size")]
    pub batch_size: u32,
    /// 归档速率上限；`None` = 不限速。
    #[serde(default)]
    pub max_rows_per_sec: Option<u32>,
    /// 校验抽样比例；`None` 由使用方按设计取 1% 默认值。
    #[serde(default)]
    pub verify_sample: Option<f32>,
    /// S3 冷数据在 `DROP PARTITION` 前的保留天数。
    #[serde(default = "default_retain_s3_days")]
    pub retain_s3_days: u32,
    #[serde(default)]
    pub dry_run: bool,
}

/// 单表的分层策略。
#[derive(Debug, Clone, Deserialize)]
pub struct TableTieringConfig {
    pub name: String,
    pub time_col: String,
    pub id_col: String,
    /// 冷数据总保留天数，必须 >= 顶层 `hot_days`。
    pub retention_days: u32,
    pub target: ColdTarget,
    /// 可选的白名单过滤条件（如 `tenant_id = 1`）。
    #[serde(default)]
    pub filter: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdTarget {
    Clickhouse,
    S3,
}

#[derive(Debug, thiserror::Error)]
pub enum TieringError {
    #[error("tiering config error: {0}")]
    Config(String),
    #[error(transparent)]
    Db(#[from] RdbmsError),
    #[error("cold storage error: {0}")]
    Cold(String),
    #[error("archive state error: {0}")]
    State(String),
    #[error("unsupported database dialect")]
    UnsupportedDialect,
}

/// 标识符白名单：`^[A-Za-z_][A-Za-z0-9_]*$`。DDL 里的表名/列名/分区名拼接前必须过它。
pub fn validate_ident(s: &str) -> Result<(), TieringError> {
    let mut chars = s.chars();
    let head_ok = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_');
    if !head_ok || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(TieringError::Config(format!(
            "invalid identifier `{s}`: expected ^[A-Za-z_][A-Za-z0-9_]*$"
        )));
    }
    Ok(())
}

impl TieringConfig {
    pub fn validate(&self) -> Result<(), TieringError> {
        if self.hot_days == 0 {
            return Err(TieringError::Config("hot_days must be > 0".into()));
        }
        if self.partition_months == 0 {
            return Err(TieringError::Config("partition_months must be > 0".into()));
        }
        for table in &self.tables {
            validate_ident(&table.name)?;
            validate_ident(&table.time_col)?;
            validate_ident(&table.id_col)?;
            if table.retention_days < self.hot_days {
                return Err(TieringError::Config(format!(
                    "table `{}`: retention_days ({}) must be >= hot_days ({})",
                    table.name, table.retention_days, self.hot_days
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
