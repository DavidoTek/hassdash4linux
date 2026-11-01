//! TLS configuration: system root certificates plus an optional pinned
//! certificate that the user trusted explicitly (self-signed setups).
//!
//! There is intentionally no "ignore certificate errors" mode.

use std::sync::{Arc, Mutex};

use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, SignatureScheme};

use crate::HaError;

/// Remembers the fingerprint of the last certificate that failed verification,
/// so the UI can offer to trust it.
#[derive(Debug, Clone, Default)]
pub struct CertCapture(Arc<Mutex<Option<String>>>);

impl CertCapture {
    fn set(&self, fp: String) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(fp);
    }

    /// Take the captured fingerprint (`sha256:<hex>`).
    pub fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// A ready-to-use client TLS configuration.
#[derive(Clone)]
pub struct Tls {
    pub config: Arc<ClientConfig>,
    pub capture: CertCapture,
}

/// SHA-256 fingerprint of a DER certificate as `sha256:<lowercase hex>`.
pub fn fingerprint(der: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, der);
    let hex: String = d.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Format a fingerprint for humans: `AB:CD:…`.
pub fn display_fingerprint(fp: &str) -> String {
    let hex = fp
        .strip_prefix("sha256:")
        .unwrap_or(fp)
        .to_ascii_uppercase();
    hex.as_bytes()
        .chunks(2)
        .map(|c| std::str::from_utf8(c).unwrap_or("??"))
        .collect::<Vec<_>>()
        .join(":")
}

#[derive(Debug)]
struct Verifier {
    webpki: Option<Arc<WebPkiServerVerifier>>,
    pin: Option<String>,
    capture: CertCapture,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());
        if self.pin.as_deref() == Some(fp.as_str()) {
            return Ok(ServerCertVerified::assertion());
        }
        let result = match &self.webpki {
            Some(v) => {
                v.verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            }
            None => Err(rustls::Error::General(
                "no trusted root certificates available".into(),
            )),
        };
        if result.is_err() {
            self.capture.set(fp);
        }
        result
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
            &self.provider.signature_verification_algorithms,
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
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn load_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    let res = rustls_native_certs::load_native_certs();
    for e in &res.errors {
        tracing::debug!("loading system certificates: {e}");
    }
    let (added, ignored) = roots.add_parsable_certificates(res.certs);
    tracing::debug!("loaded {added} system root certificates ({ignored} ignored)");
    roots
}

/// Build the TLS configuration. `pin` is an optional `sha256:<hex>` fingerprint
/// of a certificate the user trusted explicitly.
pub fn build(pin: Option<&str>) -> Result<Tls, HaError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let roots = load_roots();
    let webpki = if roots.is_empty() {
        None
    } else {
        Some(
            WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
                .build()
                .map_err(|e| HaError::Local(format!("TLS setup failed: {e}")))?,
        )
    };
    if webpki.is_none() && pin.is_none() {
        tracing::warn!("no system root certificates found; only pinned certificates will work");
    }
    let capture = CertCapture::default();
    let verifier = Verifier {
        webpki,
        pin: pin.map(str::to_owned),
        capture: capture.clone(),
        provider: provider.clone(),
    };
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| HaError::Local(format!("TLS setup failed: {e}")))?
        .dangerous() // custom verifier: system roots OR the user's explicit pin
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    Ok(Tls {
        config: Arc::new(config),
        capture,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn display() {
        assert_eq!(super::display_fingerprint("sha256:0aff"), "0A:FF");
        assert!(super::fingerprint(b"x").starts_with("sha256:"));
        assert_eq!(super::fingerprint(b"x").len(), 7 + 64);
    }
}
