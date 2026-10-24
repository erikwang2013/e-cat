// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use super::*;
use serde::Deserialize;
use serde::de::value::{Error as ValueError, MapDeserializer};

fn from_map(pairs: &[(&'static str, &'static str)]) -> Result<DeploymentConfig, ValueError> {
    DeploymentConfig::deserialize(MapDeserializer::<_, ValueError>::new(pairs.iter().copied()))
}

fn config() -> DeploymentConfig {
    DeploymentConfig {
        mode: DeploymentMode::Standalone,
        node_id: String::new(),
        worker_id: None,
        cache: CacheMode::Memory,
        sharding_enabled: false,
        tiering_enabled: false,
    }
}

#[test]
fn standalone_rejects_sharding() {
    let cfg = DeploymentConfig {
        sharding_enabled: true,
        ..config()
    };
    assert_eq!(cfg.validate(), Err(DeploymentError::StandaloneWithSharding));
}

#[test]
fn standalone_allows_memory_cache() {
    let cfg = from_map(&[("mode", "standalone"), ("node_id", "")]).unwrap();
    assert_eq!(cfg.cache, CacheMode::Memory);
    assert_eq!(cfg.validate(), Ok(()));
}

#[test]
fn distributed_requires_node_id() {
    let cfg = DeploymentConfig {
        mode: DeploymentMode::Distributed,
        cache: CacheMode::Redis,
        ..config()
    };
    assert_eq!(
        cfg.validate(),
        Err(DeploymentError::DistributedWithoutNodeId)
    );
    let blank = DeploymentConfig {
        node_id: "   ".into(),
        ..cfg
    };
    assert_eq!(
        blank.validate(),
        Err(DeploymentError::DistributedWithoutNodeId)
    );
}

#[test]
fn distributed_rejects_memory_cache() {
    let cfg = DeploymentConfig {
        mode: DeploymentMode::Distributed,
        node_id: "node-1".into(),
        ..config()
    };
    assert_eq!(
        cfg.validate(),
        Err(DeploymentError::DistributedWithMemoryCache)
    );
}

#[test]
fn distributed_sharding_requires_worker_id() {
    let cfg = DeploymentConfig {
        mode: DeploymentMode::Distributed,
        node_id: "node-1".into(),
        cache: CacheMode::Redis,
        sharding_enabled: true,
        ..config()
    };
    assert_eq!(cfg.validate(), Err(DeploymentError::InvalidWorkerId));
    let out_of_range = DeploymentConfig {
        worker_id: Some(1024),
        ..cfg
    };
    assert_eq!(
        out_of_range.validate(),
        Err(DeploymentError::InvalidWorkerId)
    );
}

#[test]
fn standalone_allows_tiering() {
    let cfg = DeploymentConfig {
        tiering_enabled: true,
        ..config()
    };
    assert_eq!(cfg.validate(), Ok(()));
}

#[test]
fn distributed_with_node_and_worker_ok() {
    let cfg = DeploymentConfig {
        mode: DeploymentMode::Distributed,
        node_id: "node-1".into(),
        worker_id: Some(1023),
        cache: CacheMode::Redis,
        sharding_enabled: true,
        tiering_enabled: false,
    };
    assert_eq!(cfg.validate(), Ok(()));
}

#[test]
fn unknown_key_is_rejected() {
    let err = from_map(&[("mode", "standalone"), ("node_id", ""), ("bogus", "1")]).unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

#[test]
fn app_builder_rejects_invalid_deployment() {
    let cfg = DeploymentConfig {
        sharding_enabled: true,
        ..config()
    };
    let Err(err) = crate::App::builder().deployment(cfg).build() else {
        panic!("build should reject invalid deployment");
    };
    assert!(matches!(
        err.downcast::<DeploymentError>(),
        Ok(e) if *e == DeploymentError::StandaloneWithSharding
    ));
}
