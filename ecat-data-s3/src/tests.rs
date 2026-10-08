// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! 单元测试。独立成文件：`lib.rs` 加出站韧性后会顶到 500 行硬上限。

mod resilience;
use super::*;
use std::io::{Read, Write};

#[test]
fn config_deserializes_with_tls() {
    let cfg: S3Config = serde_json::from_value(serde_json::json!({
        "endpoint": "localhost:9000",
        "region": "us-east-1",
        "access_key": "minioadmin",
        "secret_key": "minioadmin",
        "tls": {"skip_verify": true},
    }))
    .unwrap();
    assert_eq!(cfg.region, "us-east-1");
    assert!(cfg.tls.unwrap().skip_verify == Some(true));
}

#[test]
fn client_defaults_to_https_without_scheme() {
    let client = S3Client::from_config(S3Config {
        endpoint: "localhost:9000".into(),
        region: "us-east-1".into(),
        access_key: "minioadmin".into(),
        secret_key: "minioadmin".into(),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap();
    assert_eq!(client.endpoint, "https://localhost:9000");
    assert_eq!(client.host, "localhost:9000");
}

#[test]
fn client_keeps_explicit_http_for_local_dev() {
    let client = S3Client::from_config(S3Config {
        endpoint: "http://localhost:9000".into(),
        region: "us-east-1".into(),
        access_key: "minioadmin".into(),
        secret_key: "minioadmin".into(),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap();
    assert_eq!(client.endpoint, "http://localhost:9000");
    assert_eq!(client.host, "localhost:9000");
}

#[test]
fn client_constructs_https_when_tls_enabled() {
    let client = S3Client::from_config(S3Config {
        endpoint: "localhost:9000".into(),
        region: "us-east-1".into(),
        access_key: "a".into(),
        secret_key: "b".into(),
        tls: Some(TlsClientConfig {
            ca_cert: None,
            client_cert: None,
            client_key: None,
            skip_verify: Some(true),
        }),
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap();
    assert_eq!(client.endpoint, "https://localhost:9000");
}

fn test_client() -> S3Client {
    S3Client::from_config(S3Config {
        endpoint: "localhost:9000".into(),
        region: "us-east-1".into(),
        access_key: "minioadmin".into(),
        secret_key: "minioadmin".into(),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap()
}

#[test]
fn object_path_returns_raw_key() {
    let client = test_client();
    assert_eq!(
        client.object_path("bucket", "a b#c?d%e.txt"),
        "/bucket/a b#c?d%e.txt"
    );
}

#[test]
fn signed_request_url_encodes_path_exactly_once() {
    let client = test_client();
    let path = client.object_path("bucket", "a b#c?d%e.txt");
    let (url, _, _, _) = client.signed_request("PUT", &path, &[], b"data");
    assert!(url.contains("/bucket/a%20b%23c%3Fd%25e.txt"), "url: {url}");
    assert!(!url.contains("%2520"), "double encoding: {url}");
}

#[test]
fn signed_request_returns_headers_matching_signature() {
    let client = test_client();
    let path = client.object_path("bucket", "key");
    let (_, auth, amz_date, payload_hash) = client.signed_request("PUT", &path, &[], b"data");
    // 签名使用的时间与 payload 哈希必须与请求装配值一致（同一来源）。
    let expected_hash = hex(&Sha256::digest(b"data"));
    assert_eq!(payload_hash, expected_hash);
    assert!(
        amz_date.ends_with('Z') && amz_date.len() == 16,
        "amz_date: {amz_date}"
    );
    // Authorization 的 SignedHeaders 与 credential scope 使用同一 amz_date。
    let scope_date = amz_date[..8].to_string();
    assert!(auth.contains(&format!("{scope_date}/us-east-1/s3/aws4_request")));
    assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
    // 空 payload（GET/DELETE/list）哈希固定。
    let (_, _, _, empty_hash) = client.signed_request("GET", &path, &[], b"");
    assert_eq!(
        empty_hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[tokio::test]
async fn put_surfaces_http_error_status() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf);
            let _ = sock.write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
        }
    });
    let client = S3Client::from_config(S3Config {
        endpoint: format!("http://{addr}"),
        region: "us-east-1".into(),
        access_key: "a".into(),
        secret_key: "b".into(),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap();
    let err = client.put("bucket", "key", b"data").await.unwrap_err();
    assert!(err.to_string().contains("HTTP 500"), "got: {err}");
}

#[tokio::test]
async fn requests_carry_all_signed_headers() {
    // 请求装配层：SignedHeaders 列出的头必须实际出现在请求中，
    // 否则真实 S3 对所有操作返回 403 SignatureDoesNotMatch。
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (got_tx, got_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut sock, _)) = listener.accept() {
            let mut buf = [0u8; 8192];
            let n = sock.read(&mut buf).unwrap();
            let _ = got_tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    let client = S3Client::from_config(S3Config {
        endpoint: format!("http://{addr}"),
        region: "us-east-1".into(),
        access_key: "a".into(),
        secret_key: "b".into(),
        tls: None,
        query_timeout_secs: None,
        breaker: None,
        max_concurrency: None,
    })
    .unwrap();
    client.put("bucket", "key", b"data").await.unwrap();
    let raw = got_rx.recv().unwrap();
    let (head, _) = raw.split_once("\r\n\r\n").unwrap();
    let auth = head
        .lines()
        .find_map(|l| l.strip_prefix("authorization: "))
        .or_else(|| head.lines().find_map(|l| l.strip_prefix("Authorization: ")))
        .unwrap();
    let signed = auth
        .split(", ")
        .find_map(|p| p.strip_prefix("SignedHeaders="))
        .unwrap();
    for name in signed.split(';') {
        assert!(
            head.to_ascii_lowercase().contains(&format!("{name}:")),
            "missing signed header {name} in:\n{head}"
        );
    }
    // payload 哈希头与 body 一致。
    assert!(
        head.contains(
            "x-amz-content-sha256: 3a6eb0790f39ac87c94f3856b2dd2c5d110e6811602261a9a923d3bb23adc8b7"
        ),
        "hash mismatch in:\n{head}"
    );
}
