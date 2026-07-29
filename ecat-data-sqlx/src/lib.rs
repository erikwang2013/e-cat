// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{RdbmsClient, RdbmsError, Row};
use sqlx::any::AnyRow;
use sqlx::{AnyPool, Column as SqlxColumn, Row as SqlxRow};

pub struct SqlxClient {
    pool: AnyPool,
}

impl SqlxClient {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        let pool = AnyPool::connect(url).await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: AnyPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RdbmsClient for SqlxClient {
    async fn execute(&self, sql: &str) -> Result<u64, RdbmsError> {
        sqlx::query(sql)
            .execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| RdbmsError::Database(e.to_string()))
    }

    async fn query(&self, sql: &str) -> Result<Vec<Row>, RdbmsError> {
        let rows: Vec<AnyRow> = sqlx::query(sql)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RdbmsError::Database(e.to_string()))?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let columns: Vec<String> = rows[0]
            .columns()
            .iter()
            .map(|c| c.name().to_string())
            .collect();

        let result = rows
            .iter()
            .map(|row| {
                let values: Vec<serde_json::Value> = columns
                    .iter()
                    .map(|col| {
                        // Try string first, then i64, then f64, then fallback to Null
                        row.try_get::<String, _>(col.as_str())
                            .map(serde_json::Value::String)
                            .or_else(|_| {
                                row.try_get::<i64, _>(col.as_str())
                                    .map(|n| serde_json::Value::Number(n.into()))
                            })
                            .or_else(|_| {
                                row.try_get::<f64, _>(col.as_str())
                                    .ok()
                                    .and_then(serde_json::Number::from_f64)
                                    .map(serde_json::Value::Number)
                                    .ok_or(())
                            })
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect();

                Row::new(columns.clone(), values)
            })
            .collect();

        Ok(result)
    }

    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        Err(RdbmsError::Database(
            "transactions not yet implemented".into(),
        ))
    }
}
