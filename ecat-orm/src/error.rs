// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz

/// `ecat-orm` 的错误类型。
#[derive(Debug, thiserror::Error)]
pub enum OrmError {
    /// 后端返回的错误，原样透传（含超时、连接、语法错误）。
    #[error(transparent)]
    Rdbms(#[from] ecat_data::RdbmsError),

    /// 列名/表名不在实体元数据里。**这是安全边界**：白名单未命中的标识符
    /// 绝不拼进 SQL，见 spec §5.5(a)。
    #[error("unknown column or table: {0}")]
    UnknownColumn(String),

    /// 乐观锁冲突：`UPDATE ... WHERE id = ? AND version = ?` 影响了 0 行。
    #[error("optimistic lock conflict")]
    OptimisticLockConflict,

    /// 迁移不可逆（未提供反向 SQL）。
    #[error("migration is not reversible: {0}")]
    MigrationIrreversible(String),

    /// 元数据与数据不匹配（如 `from_row` 拿到 NULL 但字段非 `Option`）。
    #[error("column `{column}` is NULL but the field is not optional")]
    UnexpectedNull { column: &'static str },

    /// 值无法转换为目标 Rust 类型。
    #[error("column `{column}`: cannot convert value to {expected}")]
    TypeMismatch {
        column: &'static str,
        expected: &'static str,
    },

    /// 迁移失败。携带版本号便于定位是哪一个。
    #[error("migration {version} ({name}) failed: {source}")]
    Migration {
        version: i64,
        name: String,
        #[source]
        source: Box<OrmError>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_column_names_the_column() {
        let e = OrmError::UnknownColumn("naem".into());
        assert!(e.to_string().contains("naem"), "got: {e}");
    }

    #[test]
    fn rdbms_error_converts_via_from() {
        let e: OrmError = ecat_data::RdbmsError::Database("boom".into()).into();
        assert!(matches!(e, OrmError::Rdbms(_)));
        assert!(e.to_string().contains("boom"), "got: {e}");
    }

    #[test]
    fn optimistic_lock_conflict_is_distinguishable() {
        // 调用方必须能靠 matches! 分辨「乐观锁冲突」与一般错误，
        // 否则只能靠字符串匹配 —— 那是脆的。
        let e = OrmError::OptimisticLockConflict;
        assert!(matches!(e, OrmError::OptimisticLockConflict));
    }
}
