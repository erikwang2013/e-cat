// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 复制前源表结构预检：`Row` 把 BLOB/BYTEA 表示为 base64 文本，经 `SqlExecutor`
//! 写回目标就会损坏；发现二进制列必须在任何复制前失败。
//!
//! 表名只支持不带引号的单段名或两段 `schema.table` / `db.table`；三段
//! `db.schema.table` 与含点的引号标识符无法安全拆分，直接 fail closed，不做拆名。
use super::*;

impl ShardMigrator {
    /// 检查每个 move 的源表；任一源表含二进制列即 `ShardError::Config`。
    /// `backfill` / `verify` / `cutover` 都在动数据或路由之前调用。
    pub(super) async fn preflight_schema(&self) -> Result<(), ShardError> {
        let mut inspected = std::collections::HashSet::new();
        for mv in &self.plan.moves {
            if !inspected.insert((mv.from.as_str(), mv.table.as_str())) {
                continue;
            }
            let source = self.client(&mv.from)?;
            let dialect = source.dialect();
            for (column, type_name) in source_columns(&source, &mv.table).await? {
                if is_binary_type(dialect, &type_name) {
                    return Err(ShardError::Config(format!(
                        "migration preflight: binary column `{}`.`{column}` has type `{type_name}`; \
                         BLOB/BYTEA would be copied as base64 text and corrupted. \
                         Refusing to copy; use a native dump/restore tool for this table.",
                        mv.table
                    )));
                }
            }
        }
        Ok(())
    }
}

/// 源表列清单 `(列名, 声明类型)`；拿不到结构时也 fail closed。
/// 两段 `schema.table` 会拆成两个元数据参数；未限定则沿用各数据库的默认 schema。
async fn source_columns(
    client: &Arc<dyn RdbmsClient>,
    table: &str,
) -> Result<Vec<(String, String)>, ShardError> {
    let dialect = client.dialect();
    let (schema, table_name) = split_table(table)?;
    let (sql, params) = match dialect {
        Dialect::MySql => match schema {
            Some(schema) => (
                concat!(
                    "SELECT COLUMN_NAME AS name, DATA_TYPE AS data_type ",
                    "FROM information_schema.COLUMNS ",
                    "WHERE TABLE_SCHEMA = ? AND TABLE_NAME = ?"
                ),
                vec![
                    Value::String(schema.to_owned()),
                    Value::String(table_name.to_owned()),
                ],
            ),
            None => (
                concat!(
                    "SELECT COLUMN_NAME AS name, DATA_TYPE AS data_type ",
                    "FROM information_schema.COLUMNS ",
                    "WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?"
                ),
                vec![Value::String(table_name.to_owned())],
            ),
        },
        Dialect::Postgres => match schema {
            Some(schema) => (
                concat!(
                    "SELECT column_name AS name, data_type AS data_type ",
                    "FROM information_schema.columns ",
                    "WHERE table_schema = $1 AND table_name = $2"
                ),
                vec![
                    Value::String(schema.to_owned()),
                    Value::String(table_name.to_owned()),
                ],
            ),
            None => (
                concat!(
                    "SELECT column_name AS name, data_type AS data_type ",
                    "FROM information_schema.columns ",
                    "WHERE table_name = $1 ",
                    "AND table_schema = ANY (current_schemas(false))"
                ),
                vec![Value::String(table_name.to_owned())],
            ),
        },
        // SQLite 没有 information_schema；表值 pragma 参数顺序是 (表, schema)。
        Dialect::Sqlite => match schema {
            Some(schema) => (
                "SELECT name, type AS data_type FROM pragma_table_info(?, ?)",
                vec![
                    Value::String(table_name.to_owned()),
                    Value::String(schema.to_owned()),
                ],
            ),
            None => (
                "SELECT name, type AS data_type FROM pragma_table_info(?)",
                vec![Value::String(table_name.to_owned())],
            ),
        },
        Dialect::Mssql | Dialect::Standard => {
            return Err(ShardError::Config(format!(
                "migration preflight: dialect `{dialect:?}` cannot be inspected for binary \
                 columns; refusing to copy"
            )));
        }
    };
    let rows = client.query_with(sql, &params).await?;
    if rows.is_empty() {
        return Err(ShardError::Config(format!(
            "migration preflight: cannot inspect source schema for table `{table}`; \
             refusing to copy without column metadata"
        )));
    }
    rows.iter()
        .map(|row| {
            Ok((
                text(row, "name")?.to_owned(),
                text(row, "data_type")?.to_owned(),
            ))
        })
        .collect()
}

/// 仅支持不带引号的单段名或两段 `schema.table` / `db.table`；其余无法安全拆分。
fn split_table(table: &str) -> Result<(Option<&str>, &str), ShardError> {
    let quoted =
        table.contains('"') || table.contains('`') || table.contains('[') || table.contains(']');
    if table.matches('.').count() > 1 || (quoted && table.contains('.')) {
        return Err(ShardError::Config(format!(
            "migration preflight: table name `{table}` is not supported; only unquoted `table` \
             or two-part `schema.table` / `db.table` names can be inspected. Three-part \
             `db.schema.table` names and quoted identifiers containing dots are not supported; \
             refusing to copy."
        )));
    }
    Ok(match table.rsplit_once('.') {
        Some((schema, name)) => (Some(schema), name),
        None => (None, table),
    })
}

/// 会以二进制形式取回的列类型；MySQL 的几何类型同样经 `Vec<u8>` 解码。
fn is_binary_type(dialect: Dialect, type_name: &str) -> bool {
    let normalized = type_name.trim().to_ascii_lowercase();
    match dialect {
        Dialect::MySql => matches!(
            normalized.as_str(),
            "binary"
                | "varbinary"
                | "tinyblob"
                | "blob"
                | "mediumblob"
                | "longblob"
                | "bit"
                | "geometry"
                | "point"
                | "linestring"
                | "polygon"
                | "multipoint"
                | "multilinestring"
                | "multipolygon"
                | "geometrycollection"
                | "geomcollection"
                // MySQL 9 的向量列同样以二进制返回。
                | "vector"
        ),
        Dialect::Postgres => normalized == "bytea",
        // SQLite 的 BLOB affinity：声明类型含 BLOB 或未声明类型；显式
        // BINARY/VARBINARY 也按二进制意图 fail closed。
        Dialect::Sqlite => {
            normalized.is_empty() || normalized.contains("blob") || normalized.contains("binary")
        }
        // 调用方在选 SQL 时已拒绝这两个方言。
        Dialect::Mssql | Dialect::Standard => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_binary_type, split_table};
    use ecat_data::Dialect;

    #[test]
    fn table_names_split_only_two_part_unquoted_names() {
        assert_eq!(split_table("orders").unwrap(), (None, "orders"));
        assert_eq!(
            split_table("main.orders").unwrap(),
            (Some("main"), "orders")
        );
        assert_eq!(split_table("db.orders").unwrap(), (Some("db"), "orders"));
        for unsupported in [
            "catalog.main.orders",
            "\"catalog.main\".orders",
            "main.\"order.items\"",
        ] {
            assert!(split_table(unsupported).is_err(), "{unsupported}");
        }
    }

    #[test]
    fn mysql_binary_types_are_detected() {
        for type_name in ["BLOB", "longblob", "VARBINARY", "geometry", "BIT", "vector"] {
            assert!(is_binary_type(Dialect::MySql, type_name), "{type_name}");
        }
        for type_name in ["varchar", "bigint", "decimal", "json", "datetime"] {
            assert!(!is_binary_type(Dialect::MySql, type_name), "{type_name}");
        }
    }

    #[test]
    fn postgres_detects_bytea_only() {
        assert!(is_binary_type(Dialect::Postgres, "bytea"));
        assert!(is_binary_type(Dialect::Postgres, " BYTEA "));
        assert!(!is_binary_type(Dialect::Postgres, "text"));
        assert!(!is_binary_type(Dialect::Postgres, "jsonb"));
    }

    #[test]
    fn sqlite_detects_blob_affinity_and_undeclared_types() {
        for type_name in ["BLOB", "blob", "VARBINARY(16)", ""] {
            assert!(is_binary_type(Dialect::Sqlite, type_name), "{type_name}");
        }
        for type_name in ["INTEGER", "TEXT", "REAL", "NUMERIC"] {
            assert!(!is_binary_type(Dialect::Sqlite, type_name), "{type_name}");
        }
    }
}
