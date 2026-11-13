//! Shared by the dev examples: open a TLS WebSocket to a host without
//! verifying its self-signed certificate (prints the fingerprint instead).

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::crypto::{CryptoProvider, ring, verify_tls12_signature, verify_tls13_signature};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use tokio_tungstenite::WebSocketStream;

pub type Ws = WebSocketStream<TlsStream<TcpStream>>;

/// `addr` is host:port, e.g. 127.0.0.1:47800.
pub async fn connect(addr: &str) -> anyhow::Result<Ws> {
    let provider = Arc::new(ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAny(provider)))
        .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await?;
    let tls = TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("remote-duo-host")?, tcp)
        .await?;
    let (ws, _) = tokio_tungstenite::client_async(format!("wss://{addr}"), tls).await?;
    Ok(ws)
}

#[derive(Debug)]
struct AcceptAny(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAny {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        let fp: String = Sha256::digest(end_entity).iter().map(|b| format!("{b:02x}")).collect();
        eprintln!("host fingerprint: {fp}");
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
