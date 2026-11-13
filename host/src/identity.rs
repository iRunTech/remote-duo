//! The host's TLS identity: a self-signed certificate made on first run and
//! kept in the config dir. Phones pin its SHA-256 fingerprint, which they
//! learn from the pairing QR code or the Bonjour TXT record.

use std::path::PathBuf;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub struct Identity {
    pub acceptor: TlsAcceptor,
    /// Lowercase hex SHA-256 of the certificate (DER).
    pub fingerprint: String,
}

impl Identity {
    pub fn load_or_create() -> anyhow::Result<Self> {
        let dir = config_dir();
        let (cert_path, key_path) = (dir.join("identity.der"), dir.join("identity-key.der"));
        let (cert, key) = match (std::fs::read(&cert_path), std::fs::read(&key_path)) {
            (Ok(cert), Ok(key)) => (cert, key),
            _ => {
                let made = rcgen::generate_simple_self_signed(vec!["remote-duo-host".to_string()])?;
                let (cert, key) = (made.cert.der().to_vec(), made.signing_key.serialize_der());
                std::fs::create_dir_all(&dir)?;
                std::fs::write(&cert_path, &cert)?;
                write_private(&key_path, &key)?;
                (cert, key)
            }
        };

        let fingerprint = Sha256::digest(&cert).iter().map(|b| format!("{b:02x}")).collect();
        let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(cert)],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            )?;
        Ok(Self { acceptor: TlsAcceptor::from(Arc::new(config)), fingerprint })
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("remote-duo")
}

fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}
