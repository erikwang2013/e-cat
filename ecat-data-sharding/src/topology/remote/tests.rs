// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 远程拓扑源测试：mock HTTP，不连真 Consul/etcd。
use std::time::Duration;

use crate::TopologySource;

#[cfg(feature = "consul")]
use crate::ConsulTopologySource;
#[cfg(feature = "etcd")]
use crate::EtcdTopologySource;

const ORDERS: &str = r#"{"tables":[{"name":"orders","key":"id","pk":"id","algorithm":{"hash_mod":{"shards":2}},"shards":[{"name":"s0"},{"name":"s1"}]}]}"#;
const CUSTOMERS: &str = r#"{"tables":[{"name":"customers","key":"id","pk":"id","algorithm":{"hash_mod":{"shards":2}},"shards":[{"name":"s0"},{"name":"s1"}]}]}"#;
const DUPLICATE_TABLE: &str = r#"{"tables":[{"name":"orders","key":"id","pk":"id","algorithm":{"hash_mod":{"shards":2}},"shards":[{"name":"s0"},{"name":"s1"}]},{"name":"orders","key":"id","pk":"id","algorithm":{"hash_mod":{"shards":2}},"shards":[{"name":"s0"},{"name":"s1"}]}]}"#;

#[cfg(feature = "consul")]
mod consul {
    use super::*;

    use std::collections::HashMap;

    use axum::Router;
    use axum::extract::{Query, State};
    use axum::routing::get;
    use base64::Engine as _;
    use tokio::sync::watch;

    type MockState = (u64, Vec<(String, String)>);

    async fn spawn_mock_consul(entries: Vec<(&str, &str)>) -> (String, watch::Sender<MockState>) {
        let entries = entries
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        let (tx, rx) = watch::channel((1u64, entries));
        let app = Router::new()
            .route("/v1/kv/{prefix}", get(kv_handler))
            .with_state(rx);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), tx)
    }

    async fn kv_handler(
        State(mut rx): State<watch::Receiver<MockState>>,
        Query(params): Query<HashMap<String, String>>,
    ) -> axum::response::Response {
        let requested: u64 = params
            .get("index")
            .and_then(|i| i.parse().ok())
            .unwrap_or(0);
        if params.contains_key("index") {
            // Consul 阻塞查询：等状态变化或 1s 超时。
            let deadline = tokio::time::sleep(Duration::from_secs(1));
            tokio::pin!(deadline);
            loop {
                if rx.borrow().0 > requested {
                    break;
                }
                tokio::select! {
                    _ = &mut deadline => break,
                    _ = rx.changed() => {
                        if rx.borrow().0 > requested {
                            break;
                        }
                    }
                }
            }
        }
        let (index, entries) = rx.borrow().clone();
        let body: Vec<serde_json::Value> = entries
            .iter()
            .map(|(key, value)| {
                serde_json::json!({
                    "Key": key,
                    "Value": base64::engine::general_purpose::STANDARD.encode(value),
                })
            })
            .collect();
        let mut resp = axum::response::Response::new(axum::body::Body::from(
            serde_json::to_string(&body).unwrap(),
        ));
        resp.headers_mut()
            .insert("X-Consul-Index", index.to_string().parse().unwrap());
        resp
    }

    #[tokio::test]
    async fn consul_topology_loads_and_validates_catalog() {
        let (url, _tx) = spawn_mock_consul(vec![("app/topology", ORDERS)]).await;
        let catalog = ConsulTopologySource::new(url, "app")
            .load()
            .await
            .expect("valid topology must load");
        assert_eq!(catalog.tables.len(), 1);
        assert_eq!(catalog.tables[0].name, "orders");
        assert_eq!(catalog.tables[0].shards.len(), 2);

        let (bad_url, _tx) = spawn_mock_consul(vec![("app/topology", DUPLICATE_TABLE)]).await;
        let err = ConsulTopologySource::new(bad_url, "app")
            .load()
            .await
            .expect_err("invalid catalog must be rejected");
        assert!(
            err.to_string().contains("duplicate table name"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn bad_json_is_rejected() {
        let (url, _tx) = spawn_mock_consul(vec![("app/topology", "not-json")]).await;
        let err = ConsulTopologySource::new(url, "app")
            .load()
            .await
            .expect_err("malformed topology must be rejected");
        assert!(
            err.to_string().contains("invalid shard topology JSON"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn missing_topology_fails_closed() {
        let (url, _tx) = spawn_mock_consul(vec![("app/other", "{}")]).await;
        let source = ConsulTopologySource::new(url, "app");
        let err = source
            .load()
            .await
            .expect_err("missing topology must fail load");
        assert!(err.to_string().contains("missing `topology`"), "got: {err}");
        let err = source
            .watch()
            .await
            .expect_err("missing topology must fail startup");
        assert!(err.to_string().contains("missing `topology`"), "got: {err}");
    }

    #[tokio::test]
    async fn watch_emits_new_catalog() {
        let (url, tx) = spawn_mock_consul(vec![("app/topology", ORDERS)]).await;
        let source = ConsulTopologySource::new(url, "app");
        let mut rx = source.watch().await.expect("initial topology");
        assert_eq!(rx.borrow().tables[0].name, "orders");

        tx.send((2, vec![("app/topology".to_owned(), CUSTOMERS.to_owned())]))
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                rx.changed().await.expect("watch closed");
                if rx.borrow().tables[0].name == "customers" {
                    break;
                }
            }
        })
        .await
        .expect("new catalog must be emitted");
    }
}

#[cfg(feature = "etcd")]
mod etcd {
    use super::*;

    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::State;
    use axum::routing::post;
    use base64::Engine as _;

    struct MockEtcd {
        fail: bool,
        topology: String,
    }

    async fn spawn_mock_etcd(topology: &str) -> (String, Arc<Mutex<MockEtcd>>) {
        let state = Arc::new(Mutex::new(MockEtcd {
            fail: false,
            topology: topology.to_owned(),
        }));
        let app = Router::new()
            .route("/v3/kv/range", post(range_handler))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}"), state)
    }

    async fn range_handler(State(state): State<Arc<Mutex<MockEtcd>>>) -> axum::response::Response {
        let state = state.lock().unwrap();
        if state.fail {
            return axum::http::Response::builder()
                .status(500)
                .body(axum::body::Body::from("mock failure"))
                .unwrap();
        }
        let body = serde_json::json!({
            "kvs": [{
                "key": base64::engine::general_purpose::STANDARD.encode("app/topology"),
                "value": base64::engine::general_purpose::STANDARD
                    .encode(state.topology.as_bytes()),
            }]
        });
        axum::response::Response::new(axum::body::Body::from(body.to_string()))
    }

    #[tokio::test]
    async fn etcd_topology_loads_and_validates_catalog() {
        let (url, state) = spawn_mock_etcd(ORDERS).await;
        let catalog = EtcdTopologySource::new(vec![url.clone()], "app")
            .load()
            .await
            .expect("valid topology must load");
        assert_eq!(catalog.tables.len(), 1);
        assert_eq!(catalog.tables[0].name, "orders");

        state.lock().unwrap().topology = DUPLICATE_TABLE.to_owned();
        let err = EtcdTopologySource::new(vec![url], "app")
            .load()
            .await
            .expect_err("invalid catalog must be rejected");
        assert!(
            err.to_string().contains("duplicate table name"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn watch_emits_new_catalog() {
        let (url, state) = spawn_mock_etcd(ORDERS).await;
        let source = EtcdTopologySource::new(vec![url], "app")
            .with_watch_interval(Duration::from_millis(50));
        let mut rx = source.watch().await.expect("initial topology");
        assert_eq!(rx.borrow().tables[0].name, "orders");

        state.lock().unwrap().topology = CUSTOMERS.to_owned();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                rx.changed().await.expect("watch closed");
                if rx.borrow().tables[0].name == "customers" {
                    break;
                }
            }
        })
        .await
        .expect("new catalog must be emitted");
    }

    #[tokio::test]
    async fn watch_failure_keeps_last_known() {
        let (url, state) = spawn_mock_etcd(ORDERS).await;
        let source = EtcdTopologySource::new(vec![url], "app")
            .with_watch_interval(Duration::from_millis(50));
        let mut rx = source.watch().await.expect("initial topology");

        state.lock().unwrap().fail = true;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !rx.has_changed().expect("watch open"),
            "failed poll must not emit"
        );
        assert_eq!(rx.borrow().tables[0].name, "orders");

        {
            let mut state = state.lock().unwrap();
            state.fail = false;
            state.topology = CUSTOMERS.to_owned();
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                rx.changed().await.expect("watch closed");
                if rx.borrow().tables[0].name == "customers" {
                    break;
                }
            }
        })
        .await
        .expect("watch must recover after a failed poll");
    }
}
