// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! `TsdbClient` 实现。独立成文件是因为 `lib.rs` 紧贴 500 行硬上限
//! （项目约定，与批次 4 拆 `ecat-circuit-breaker` 同因）。
//!
//! 三个方法各经一次 `guarded_tsdb`（许可 → 熔断 → 超时）；
//! 内部的 `create_table` / `post` / `build_insert_body` **不再单独包装** ——
//! 它们在 `write` 的包装之内，再包一次会重复计数超时、重复取许可。
use crate::{ClickhouseClient, build_insert_body, field_type, quote_ident};
use async_trait::async_trait;
use ecat_data::{DataPoint, Error, TsdbClient};
use ecat_errors::ErrorCode;

#[async_trait]
impl TsdbClient for ClickhouseClient {
    async fn write(&self, points: &[DataPoint]) -> Result<(), Error> {
        self.guarded_tsdb(async {
            // 按 measurement 分组，保持首见顺序；分组只存引用，避免克隆整点
            let mut order: Vec<&str> = Vec::new();
            let mut groups: std::collections::HashMap<&str, Vec<&DataPoint>> =
                std::collections::HashMap::new();
            for p in points {
                if !groups.contains_key(p.measurement.as_str()) {
                    order.push(&p.measurement);
                }
                groups.entry(&p.measurement).or_default().push(p);
            }

            for measurement in order {
                let pts = &groups[&measurement];
                // 列集合取本批全部点；同名 field 类型不一致时先见者胜（文档注明）
                let tag_keys: Vec<String> = pts
                    .iter()
                    .flat_map(|p| p.tags.keys().cloned())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                let field_cols: Vec<(String, &'static str)> = {
                    let mut m: std::collections::BTreeMap<String, &'static str> =
                        std::collections::BTreeMap::new();
                    for p in pts {
                        for (k, v) in &p.fields {
                            m.entry(k.clone()).or_insert_with(|| field_type(v));
                        }
                    }
                    m.into_iter().collect()
                };
                let field_keys: Vec<String> = field_cols.iter().map(|(k, _)| k.clone()).collect();

                // 建表（按 client 缓存 + TTL；CREATE IF NOT EXISTS 幂等）。
                // 列类型由首批点的字段类型决定并固定；后续批次若出现同名不同型的字段，
                // ClickHouse 不会自动 ALTER 列，写入会以服务端错误失败（调用方需保证类型一致）。
                if self.table_needs_create(measurement) {
                    self.create_table(measurement, &tag_keys, &field_cols)
                        .await?;
                }

                let body = build_insert_body(pts, &tag_keys, &field_keys);
                let cols: Vec<String> = tag_keys
                    .iter()
                    .chain(field_keys.iter())
                    .chain(std::iter::once(&"timestamp".to_string()))
                    .map(|c| quote_ident(c))
                    .collect();
                // JSONEachRow 数据随请求体放在语句之后（ClickHouse HTTP 接口标准用法）
                let insert = format!(
                    "INSERT INTO {} ({}) FORMAT JSONEachRow\n{}",
                    quote_ident(measurement),
                    cols.join(", "),
                    body
                );
                let resp = self.post(&insert, &[]).send().await.map_err(|e| {
                    Error::new(ErrorCode::Internal, "clickhouse", format!("ch write: {e}"))
                })?;
                if !resp.status().is_success() {
                    let text = resp.text().await.unwrap_or_default();
                    // 表被外部 drop/改表：清缓存重新建表后重试一次
                    if text.contains("doesn't exist") || text.contains("Unknown table") {
                        self.created
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(measurement);
                        self.create_table(measurement, &tag_keys, &field_cols)
                            .await?;
                        let resp = self.post(&insert, &[]).send().await.map_err(|e| {
                            Error::new(ErrorCode::Internal, "clickhouse", format!("ch write: {e}"))
                        })?;
                        if !resp.status().is_success() {
                            return Err(Error::new(
                                ErrorCode::Internal,
                                "clickhouse",
                                format!(
                                    "ch write failed: {}",
                                    resp.text().await.unwrap_or_default()
                                ),
                            ));
                        }
                    } else {
                        return Err(Error::new(
                            ErrorCode::Internal,
                            "clickhouse",
                            format!("ch write failed: {text}"),
                        ));
                    }
                }
            }
            Ok(())
        })
        .await
    }

    async fn query(&self, query: &str) -> Result<serde_json::Value, Error> {
        self.guarded_tsdb(async {
            let resp = self
                .post(query, &[("default_format", "JSONEachRow".to_string())])
                .send()
                .await
                .map_err(|e| {
                    Error::new(ErrorCode::Internal, "clickhouse", format!("ch query: {e}"))
                })?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "clickhouse",
                    format!("ch query failed: {}", resp.text().await.unwrap_or_default()),
                ));
            }
            let text = resp.text().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "clickhouse", format!("ch read: {e}"))
            })?;
            let mut rows = Vec::new();
            for line in text.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let v: serde_json::Value = serde_json::from_str(line).map_err(|e| {
                    Error::new(
                        ErrorCode::Internal,
                        "clickhouse",
                        format!("ch query parse: {e}"),
                    )
                })?;
                rows.push(v);
            }
            Ok(serde_json::json!(rows))
        })
        .await
    }

    async fn delete(&self, query: &str) -> Result<(), Error> {
        self.guarded_tsdb(async {
            // ClickHouse 轻量删除语法：ALTER TABLE <t> DELETE WHERE ...
            let resp = self.post(query, &[]).send().await.map_err(|e| {
                Error::new(ErrorCode::Internal, "clickhouse", format!("ch delete: {e}"))
            })?;
            if !resp.status().is_success() {
                return Err(Error::new(
                    ErrorCode::Internal,
                    "clickhouse",
                    format!(
                        "ch delete failed: {}",
                        resp.text().await.unwrap_or_default()
                    ),
                ));
            }
            Ok(())
        })
        .await
    }
}
