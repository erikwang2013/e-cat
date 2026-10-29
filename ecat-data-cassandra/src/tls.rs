// Copyright (c) 2026 erik <erik@erik.xyz> — https://erik.xyz
//! rustls setup for Cassandra TLS connections.
//!
//! `scylla` only accepts an `Arc<rustls::ClientConfig>`, so the certificate
//! material from [`TlsClientConfig`] is assembled here instead of going
//! through `ecat-tls`'s reqwest builder.

use ecat_data::RdbmsError;
use ecat_tls::TlsClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, RootCertStore, SignatureScheme};
use std::sync::{Arc, OnceLock};

/// rustls refuses to pick a provider when both ring and aws-lc-rs are in the
/// dependency tree (the workspace enables both), so install one explicitly.
/// First install wins — if another crate got there first, that provider is used.
fn ensure_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = CryptoProvider::install_default(rustls::crypto::ring::default_provider());
    });
}

fn provider() -> Arc<CryptoProvider> {
    CryptoProvider::get_default()
        .cloned()
        .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()))
}

pub(crate) fn build_client_config(
    tls: &TlsClientConfig,
) -> Result<Arc<rustls::ClientConfig>, RdbmsError> {
    // Same contradiction check as `ecat-tls`: a trust anchor plus "skip
    // verification" is silently disabling verification, so reject it.
    if tls.skip_verify == Some(true) && tls.ca_cert.is_some() {
        return Err(config_error(
            "skip_verify=true cannot be combined with ca_cert",
        ));
    }
    match (&tls.client_cert, &tls.client_key) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => {
            return Err(config_error(
                "client_cert and client_key must be configured together",
            ));
        }
    }
    ensure_crypto_provider();

    let builder = if tls.skip_verify == Some(true) {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
    } else {
        rustls::ClientConfig::builder().with_root_certificates(root_store(tls)?)
    };

    let config = match (&tls.client_cert, &tls.client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_pem = std::fs::read(cert_path)
                .map_err(|e| config_error(format!("read client_cert {cert_path}: {e}")))?;
            let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| config_error(format!("parse client_cert: {e}")))?;
            let key_pem = std::fs::read(key_path)
                .map_err(|e| config_error(format!("read client_key {key_path}: {e}")))?;
            let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
                .map_err(|e| config_error(format!("parse client_key: {e}")))?
                .ok_or_else(|| config_error("client_key contains no private key"))?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|e| config_error(format!("client auth: {e}")))?
        }
        _ => builder.with_no_client_auth(),
    };
    Ok(Arc::new(config))
}

fn root_store(tls: &TlsClientConfig) -> Result<RootCertStore, RdbmsError> {
    let mut roots = RootCertStore::empty();
    match &tls.ca_cert {
        Some(path) => {
            let pem = std::fs::read(path)
                .map_err(|e| config_error(format!("read ca_cert {path}: {e}")))?;
            let certs = rustls_pemfile::certs(&mut pem.as_slice())
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| config_error(format!("parse ca_cert: {e}")))?;
            for cert in certs {
                roots
                    .add(cert)
                    .map_err(|e| config_error(format!("add ca_cert {path}: {e}")))?;
            }
        }
        // No explicit CA: trust the platform store, matching reqwest's default.
        None => {
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert);
            }
        }
    }
    Ok(roots)
}

fn config_error(message: impl Into<String>) -> RdbmsError {
    RdbmsError::Config(format!("cassandra TLS: {}", message.into()))
}

/// Accepts any server certificate. Only reachable with `skip_verify: true`;
/// signature checks still run so the handshake stays verifiable.
#[derive(Debug)]
struct NoVerifier;

impl ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique scratch directory; the test cleans it up itself.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ecat-cassandra-tls-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &std::path::Path, name: &str, contents: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        path.display().to_string()
    }

    #[test]
    fn builds_config_with_ca_and_client_identity() {
        let dir = scratch("ca-and-identity");
        let ca = ecat_tls::generate_ca("TestOrg").unwrap();
        let identity = ecat_tls::generate_client_cert("client").unwrap();
        let tls = TlsClientConfig {
            ca_cert: Some(write(&dir, "ca.pem", &ca.cert_pem)),
            client_cert: Some(write(&dir, "client.pem", &identity.cert_pem)),
            client_key: Some(write(&dir, "client.key", &identity.key_pem)),
            skip_verify: None,
        };

        assert!(build_client_config(&tls).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn skip_verify_builds_without_trust_anchor() {
        let tls = TlsClientConfig {
            ca_cert: None,
            client_cert: None,
            client_key: None,
            skip_verify: Some(true),
        };
        assert!(build_client_config(&tls).is_ok());
    }

    #[test]
    fn skip_verify_with_ca_cert_is_rejected() {
        let tls = TlsClientConfig {
            ca_cert: Some("/nonexistent/ca.pem".to_owned()),
            client_cert: None,
            client_key: None,
            skip_verify: Some(true),
        };
        let error = build_client_config(&tls).unwrap_err();
        assert!(error.to_string().contains("skip_verify"));
    }
}
