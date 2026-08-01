// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use async_trait::async_trait;
use ecat_data::{RdbmsClient, RdbmsError, Row, TransactionInner};
use ecat_tls::TlsClientConfig;
use serde::Deserialize;
use sqlx::any::AnyRow;
use sqlx::{AnyPool, Column as SqlxColumn, Row as SqlxRow};

#[derive(Debug, Clone, Deserialize)]
pub struct SqlxConfig {
    pub url: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    /// TLS — SQLx TLS is configured via URL params (e.g. ?sslmode=require).
    /// This field is reserved for future programmatic TLS support.
    #[serde(default)]
    pub tls: Option<TlsClientConfig>,
}

pub struct SqlxClient {
    pool: AnyPool,
}

impl SqlxClient {
    pub async fn connect(url: &str) -> Result<Self, sqlx::Error> {
        let pool = AnyPool::connect(url).await?;
        Ok(Self { pool })
    }

    pub async fn connect_with_auth(
        url: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, sqlx::Error> {
        let url = if url.contains('@') {
            url.to_string()
        } else {
            url.replacen("://", &format!("://{username}:{password}@"), 1)
        };
        Self::connect(&url).await
    }

    pub async fn from_config(cfg: SqlxConfig) -> Result<Self, sqlx::Error> {
        match (&cfg.username, &cfg.password) {
            (Some(u), Some(p)) if !u.is_empty() || !p.is_empty() => {
                Self::connect_with_auth(&cfg.url, u, p).await
            }
            _ => Self::connect(&cfg.url).await,
        }
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

        let columns: std::sync::Arc<Vec<String>> = std::sync::Arc::new(
            rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect(),
        );

        let cols = std::sync::Arc::clone(&columns);
        let result = rows
            .iter()
            .map(|row| {
                let values: Vec<serde_json::Value> = cols
                    .iter()
                    .map(|col| {
                        // Try common DB types: bool → i64 → f64 → String → Null
                        row.try_get::<bool, _>(col.as_str())
                            .map(serde_json::Value::Bool)
                            .or_else(|_| {
                                row.try_get::<i64, _>(col.as_str())
                                    .map(|n| serde_json::Value::Number(n.into()))
                            })
                            .or_else(|_| {
                                row.try_get::<i32, _>(col.as_str())
                                    .map(|n| serde_json::Value::Number((n as i64).into()))
                            })
                            .or_else(|_| {
                                row.try_get::<f64, _>(col.as_str())
                                    .ok()
                                    .and_then(|n| {
                                        if n.is_finite() {
                                            serde_json::Number::from_f64(n)
                                                .map(serde_json::Value::Number)
                                        } else if n.is_nan() {
                                            Some(serde_json::Value::String("NaN".into()))
                                        } else if n > 0.0 {
                                            Some(serde_json::Value::String("Infinity".into()))
                                        } else {
                                            Some(serde_json::Value::String("-Infinity".into()))
                                        }
                                    })
                                    .ok_or(())
                            })
                            .or_else(|_| {
                                row.try_get::<String, _>(col.as_str())
                                    .map(serde_json::Value::String)
                            })
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect();

                Row::new((*cols).clone(), values)
            })
            .collect();

        Ok(result)
    }

    async fn execute_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<u64, RdbmsError> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = match p {
                serde_json::Value::String(s) => q.bind(s.as_str()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        q.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        q.bind(f)
                    } else {
                        q.bind(n.to_string())
                    }
                }
                serde_json::Value::Bool(b) => q.bind(*b),
                serde_json::Value::Null => q.bind(None::<String>),
                _ => q.bind(p.to_string()),
            };
        }
        q.execute(&self.pool)
            .await
            .map(|r| r.rows_affected())
            .map_err(|e| RdbmsError::Database(e.to_string()))
    }

    async fn query_with(
        &self,
        sql: &str,
        params: &[serde_json::Value],
    ) -> Result<Vec<Row>, RdbmsError> {
        let mut q = sqlx::query(sql);
        for p in params {
            q = match p {
                serde_json::Value::String(s) => q.bind(s.as_str()),
                serde_json::Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        q.bind(i)
                    } else if let Some(f) = n.as_f64() {
                        q.bind(f)
                    } else {
                        q.bind(n.to_string())
                    }
                }
                serde_json::Value::Bool(b) => q.bind(*b),
                serde_json::Value::Null => q.bind(None::<String>),
                _ => q.bind(p.to_string()),
            };
        }
        let rows: Vec<sqlx::any::AnyRow> = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| RdbmsError::Database(e.to_string()))?;

        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let columns: std::sync::Arc<Vec<String>> = std::sync::Arc::new(
            rows[0]
                .columns()
                .iter()
                .map(|c| c.name().to_string())
                .collect(),
        );
        let cols = std::sync::Arc::clone(&columns);
        let result = rows
            .iter()
            .map(|row| {
                let values: Vec<serde_json::Value> = cols
                    .iter()
                    .map(|col| {
                        row.try_get::<bool, _>(col.as_str())
                            .map(serde_json::Value::Bool)
                            .or_else(|_| {
                                row.try_get::<i64, _>(col.as_str())
                                    .map(|n| serde_json::Value::Number(n.into()))
                            })
                            .or_else(|_| {
                                row.try_get::<i32, _>(col.as_str())
                                    .map(|n| serde_json::Value::Number((n as i64).into()))
                            })
                            .or_else(|_| {
                                row.try_get::<f64, _>(col.as_str())
                                    .ok()
                                    .and_then(|n| {
                                        if n.is_finite() {
                                            serde_json::Number::from_f64(n)
                                                .map(serde_json::Value::Number)
                                        } else if n.is_nan() {
                                            Some(serde_json::Value::String("NaN".into()))
                                        } else if n > 0.0 {
                                            Some(serde_json::Value::String("Infinity".into()))
                                        } else {
                                            Some(serde_json::Value::String("-Infinity".into()))
                                        }
                                    })
                                    .ok_or(())
                            })
                            .or_else(|_| {
                                row.try_get::<String, _>(col.as_str())
                                    .map(serde_json::Value::String)
                            })
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .collect();
                Row::new((*cols).clone(), values)
            })
            .collect();
        Ok(result)
    }

    async fn transaction(&self) -> Result<ecat_data::Transaction, RdbmsError> {
        let tx = self
            .pool
            .begin()
            .await
            .map_err(|e| RdbmsError::Database(e.to_string()))?;
        Ok(ecat_data::Transaction::with_inner(Box::new(
            SqlxTransactionWrapper { inner: Some(tx) },
        )))
    }
}

struct SqlxTransactionWrapper {
    inner: Option<sqlx::Transaction<'static, sqlx::Any>>,
}

#[async_trait]
impl TransactionInner for SqlxTransactionWrapper {
    async fn commit(&mut self) -> Result<(), RdbmsError> {
        if let Some(tx) = self.inner.take() {
            tx.commit()
                .await
                .map_err(|e| RdbmsError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn rollback(&mut self) -> Result<(), RdbmsError> {
        if let Some(tx) = self.inner.take() {
            tx.rollback()
                .await
                .map_err(|e| RdbmsError::Database(e.to_string()))?;
        }
        Ok(())
    }
}
