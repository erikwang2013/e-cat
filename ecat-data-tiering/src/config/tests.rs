// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 配置解析与校验：默认值、未知键拒绝、标识符白名单、每表保留期。
use serde_json::json;

use super::{ColdTarget, TieringConfig, TieringError, validate_ident};

fn table(name: &str, time_col: &str, id_col: &str, retention_days: u32) -> serde_json::Value {
    json!({
        "name": name,
        "time_col": time_col,
        "id_col": id_col,
        "retention_days": retention_days,
        "target": "clickhouse",
    })
}

#[test]
fn config_defaults_are_off_and_180_days() {
    let cfg: TieringConfig = serde_json::from_value(json!({})).unwrap();

    assert!(!cfg.enabled);
    assert_eq!(cfg.hot_days, 180);
    assert_eq!(cfg.partition_months, 1);
    assert_eq!(cfg.batch_size, 1000);
    assert_eq!(cfg.retain_s3_days, 7);
    assert!(!cfg.dry_run);
    assert!(cfg.tables.is_empty());
    assert!(cfg.max_rows_per_sec.is_none());
    cfg.validate().unwrap();
}

#[test]
fn config_rejects_unknown_key() {
    let err = serde_json::from_value::<TieringConfig>(json!({"hotdays": 180})).unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

#[test]
fn config_rejects_zero_hot_days() {
    let cfg: TieringConfig = serde_json::from_value(json!({"hot_days": 0})).unwrap();
    assert!(matches!(cfg.validate(), Err(TieringError::Config(_))));
}

#[test]
fn config_rejects_zero_partition_months() {
    let cfg: TieringConfig = serde_json::from_value(json!({"partition_months": 0})).unwrap();
    assert!(matches!(cfg.validate(), Err(TieringError::Config(_))));
}

#[test]
fn config_rejects_empty_table_name_or_columns() {
    for t in [
        table("", "ts", "id", 180),
        table("events", "", "id", 180),
        table("events", "ts", "", 180),
    ] {
        let cfg: TieringConfig = serde_json::from_value(json!({"tables": [t]})).unwrap();
        assert!(matches!(cfg.validate(), Err(TieringError::Config(_))));
    }
}

#[test]
fn config_rejects_retention_below_hot_days() {
    let cfg: TieringConfig = serde_json::from_value(
        json!({"hot_days": 180, "tables": [table("events", "ts", "id", 179)]}),
    )
    .unwrap();
    assert!(matches!(cfg.validate(), Err(TieringError::Config(_))));

    let ok: TieringConfig = serde_json::from_value(
        json!({"hot_days": 180, "tables": [table("events", "ts", "id", 180)]}),
    )
    .unwrap();
    ok.validate().unwrap();
}

#[test]
fn config_accepts_clickhouse_and_s3_targets() {
    let mut s3 = table("audit", "created_at", "id", 365);
    s3["target"] = json!("s3");
    let cfg: TieringConfig = serde_json::from_value(json!({
        "hot_days": 30,
        "tables": [table("events", "ts", "id", 90), s3],
    }))
    .unwrap();

    cfg.validate().unwrap();
    assert_eq!(cfg.tables[0].target, ColdTarget::Clickhouse);
    assert_eq!(cfg.tables[1].target, ColdTarget::S3);
}

#[test]
fn config_validate_ident_enforces_ascii_pattern() {
    for ok in ["t", "_t", "events_2024", "A1"] {
        validate_ident(ok).unwrap();
    }
    for bad in ["", "1t", "a-b", "a;DROP", "表", "a b"] {
        assert!(matches!(validate_ident(bad), Err(TieringError::Config(_))));
    }
}
