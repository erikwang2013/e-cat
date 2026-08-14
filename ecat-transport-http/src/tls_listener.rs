// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

/// 安装默认 rustls CryptoProvider（ring）。
/// 同时编译 aws-lc-rs 与 ring features 时，rustls 无法自动选择 provider，
/// 构造 ClientConfig/ServerConfig 会 panic；此处用 OnceLock 保证只安装一次。
pub(crate) fn ensure_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = rustls::crypto::CryptoProvider::install_default(
            rustls::crypto::ring::default_provider(),
        );
    });
}

/// 从 TlsConfig 构建 rustls 服务端配置：加载 cert/key，ca_cert_path +
/// require_client_auth 时要求并校验客户端证书（mTLS）。
pub(crate) fn build_server_config(
    tls: &ecat_transport::TlsConfig,
) -> Result<rustls::ServerConfig, Box<dyn std::error::Error + Send + Sync>> {
    ensure_crypto_provider();
    let certs = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(
        &tls.cert_path,
    )?))
    .collect::<Result<Vec<_>, _>>()?;
    if certs.is_empty() {
        return Err(format!("no certificates found in {}", tls.cert_path.display()).into());
    }
    let key = rustls_pemfile::private_key(&mut std::io::BufReader::new(std::fs::File::open(
        &tls.key_path,
    )?))?
    .ok_or_else(|| format!("no private key found in {}", tls.key_path.display()))?;

    let builder = rustls::ServerConfig::builder();
    let config = if tls.require_client_auth {
        let ca_path = tls
            .ca_cert_path
            .as_ref()
            .ok_or("require_client_auth requires ca_cert_path")?;
        let ca_certs = rustls_pemfile::certs(&mut std::io::BufReader::new(std::fs::File::open(
            ca_path,
        )?))
        .collect::<Result<Vec<_>, _>>()?;
        if ca_certs.is_empty() {
            return Err(format!("no CA certificates found in {}", ca_path.display()).into());
        }
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(ca_certs);
        let verifier = rustls::server::WebPkiClientVerifier::builder(Arc::new(roots)).build()?;
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let mut server_config = config.with_single_cert(certs, key)?;
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(server_config)
}

/// TLS 握手超时：慢速/僵尸连接超过该时间即断开，避免长期占用握手任务。
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// axum::serve::Listener：TCP 连接由后台 accept 循环接收，握手在各连接
/// 自己的 spawn 任务中异步完成（带 HANDSHAKE_TIMEOUT），accept() 只从通道
/// 取已握手连接。修复前握手在 accept() 内同步完成，axum::serve 串行调用
/// accept()，批量慢速/僵尸连接会阻塞整个 accept 循环（S1 DoS）。
pub(crate) struct TlsListener {
    rx: mpsc::Receiver<(std::io::Result<TlsStream>, SocketAddr)>,
    local_addr: SocketAddr,
    shutdown_tx: watch::Sender<()>,
}

type TlsStream = tokio_rustls::server::TlsStream<TcpStream>;

impl TlsListener {
    pub(crate) fn new(listener: TcpListener, acceptor: tokio_rustls::TlsAcceptor) -> Self {
        let local_addr = listener.local_addr().expect("listener has local addr");
        let (tx, rx) = mpsc::channel::<(std::io::Result<TlsStream>, SocketAddr)>(64);
        let (shutdown_tx, shutdown_rx) = watch::channel(());
        tokio::spawn(accept_loop(listener, acceptor, tx, shutdown_rx));
        Self {
            rx,
            local_addr,
            shutdown_tx,
        }
    }
}

/// 后台 accept 循环：只负责接收 TCP 连接并把握手派给各自 spawn 的任务，
/// 自身不参与握手；TlsListener 释放（服务停止）时 watch 信号触发退出。
async fn accept_loop(
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    tx: mpsc::Sender<(std::io::Result<TlsStream>, SocketAddr)>,
    mut shutdown_rx: watch::Receiver<()>,
) {
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            res = listener.accept() => {
                let (stream, addr) = match res {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(error = %e, "tcp accept failed");
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let result = match tokio::time::timeout(
                        HANDSHAKE_TIMEOUT,
                        acceptor.accept(stream),
                    )
                    .await
                    {
                        Ok(Ok(tls)) => Ok(tls),
                        Ok(Err(e)) => Err(e),
                        Err(_) => {
                            tracing::warn!(
                                addr = %addr,
                                timeout_secs = HANDSHAKE_TIMEOUT.as_secs(),
                                "tls handshake timed out"
                            );
                            Err(std::io::Error::new(
                                std::io::ErrorKind::TimedOut,
                                "tls handshake timed out",
                            ))
                        }
                    };
                    // 接收端已关闭（服务停止）时投递失败，连接随任务结束释放。
                    let _ = tx.send((result, addr)).await;
                });
            }
        }
    }
}

impl Drop for TlsListener {
    fn drop(&mut self) {
        // 通知后台 accept 循环退出，避免监听器随任务泄漏、端口被持续占用。
        let _ = self.shutdown_tx.send(());
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let (result, addr) = match self.rx.recv().await {
                Some(pair) => pair,
                // 通道关闭只发生在 TlsListener 释放之后（watch 信号先让 accept
                // 循环退出），届时 axum::serve 已不再调用 accept()，属不可达分支。
                None => panic!("tls accept loop exited unexpectedly"),
            };
            match result {
                Ok(tls) => return (tls, addr),
                Err(e) => {
                    tracing::warn!(error = %e, "tls handshake failed");
                }
            }
        }
    }

    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Ok(self.local_addr)
    }
}
