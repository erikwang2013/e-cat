// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 代理模式校验测试。
use crate::config::{ShardingConfig, ShardingMode};
use crate::proxy::validate_proxy;
use crate::{ShardAlgorithm, ShardCatalog, ShardEndpoint, ShardError, ShardTable};

fn endpoint(name: &str, url: Option<&str>) -> ShardEndpoint {
    ShardEndpoint {
        name: name.to_owned(),
        url: url.map(str::to_owned),
    }
}

fn single_table(url: Option<&str>) -> ShardTable {
    ShardTable {
        name: "users".to_owned(),
        key: "id".to_owned(),
        pk: "id".to_owned(),
        algorithm: ShardAlgorithm::HashMod { shards: 1 },
        shards: vec![endpoint("s0", url)],
    }
}

fn config(mode: ShardingMode, tables: Vec<ShardTable>) -> ShardingConfig {
    ShardingConfig {
        mode,
        catalog: ShardCatalog { tables },
    }
}

#[test]
fn proxy_rejects_client_mode() {
    // 配置本身满足 Client 模式的 url 规则，报错只能来自模式检查。
    let client = config(
        ShardingMode::Client,
        vec![single_table(Some("mysql://s0/users"))],
    );
    assert_eq!(client.validate(), Ok(()));
    assert!(matches!(
        validate_proxy(&client),
        Err(ShardError::Config(_))
    ));
}

#[test]
fn proxy_rejects_empty_tables() {
    // ShardingConfig::validate 不要求目录非空，这一条是代理模式独有的约束。
    let proxy = config(ShardingMode::Proxy, Vec::new());
    assert_eq!(proxy.validate(), Ok(()));
    assert!(matches!(validate_proxy(&proxy), Err(ShardError::Config(_))));
}

#[test]
fn proxy_rejects_per_shard_urls() {
    let proxy = config(
        ShardingMode::Proxy,
        vec![single_table(Some("mysql://s0/users"))],
    );
    assert!(matches!(validate_proxy(&proxy), Err(ShardError::Config(_))));
}

#[test]
fn proxy_accepts_single_endpoint_catalog() {
    let proxy = config(ShardingMode::Proxy, vec![single_table(None)]);
    assert_eq!(validate_proxy(&proxy), Ok(()));
}
