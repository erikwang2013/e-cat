// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! [`SqlxRoutingConfig`] 与 [`build_routing`] 的测试。

use super::*;
use ecat_data::{RdbmsError, SqlExecutor};

#[test]
fn routing_config_rejects_unknown_key() {
    let json = r#"{"primary": {"url": "sqlite::memory:"}, "replica": []}"#;
    assert!(
        serde_json::from_str::<SqlxRoutingConfig>(json).is_err(),
        "未知顶层键必须被 deny_unknown_fields 拒绝"
    );
}

#[test]
fn routing_config_defaults_fallback_to_primary() {
    let cfg: SqlxRoutingConfig =
        serde_json::from_str(r#"{"primary": {"url": "sqlite::memory:"}}"#).unwrap();
    assert!(cfg.fallback_to_primary);
    assert!(cfg.replicas.is_empty());
    assert_eq!(cfg.max_replica_lag_secs, None);
}

#[tokio::test]
async fn build_routing_without_replicas_reads_primary() {
    let cfg: SqlxRoutingConfig =
        serde_json::from_str(r#"{"primary": {"url": "sqlite::memory:"}}"#).unwrap();
    let routing = build_routing(&cfg).await.unwrap();
    assert_eq!(routing.query("SELECT 1").await.unwrap().len(), 1);
}

/// 与上一个用例成对：`fallback_to_primary = false` 时无副本可用的读必须快速
/// 失败，证明确实把配置项接到了路由上（默认的 `true` 无法区分「接了」与「没接」）。
#[tokio::test]
async fn build_routing_applies_fallback_to_primary_off() {
    let cfg: SqlxRoutingConfig = serde_json::from_str(
        r#"{"primary": {"url": "sqlite::memory:"}, "fallback_to_primary": false}"#,
    )
    .unwrap();
    let routing = build_routing(&cfg).await.unwrap();
    let err = routing.query("SELECT 1").await.unwrap_err();
    assert!(matches!(err, RdbmsError::NoAvailableReplica));
}
