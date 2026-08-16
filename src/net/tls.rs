use std::{fs, path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};
use sha2::{Digest, Sha256};

pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    hex::encode(Sha256::digest(cert.as_ref()))
}

pub fn load_or_generate_server_cert(
    data_dir: &Path,
) -> Result<(CertificateDer<'static>, PrivatePkcs8KeyDer<'static>)> {
    let cert_path = data_dir.join("tls-cert.der");
    let key_path = data_dir.join("tls-key.der");
    if cert_path.exists() && key_path.exists() {
        let cert = CertificateDer::from(fs::read(&cert_path).context("reading TLS certificate")?);
        let key = PrivatePkcs8KeyDer::from(fs::read(&key_path).context("reading TLS key")?);
        return Ok((cert, key));
    }

    let mut names = vec!["localhost".to_owned()];
    if let Ok(host) = hostname::get()
        && let Ok(host) = host.into_string()
        && !host.is_empty()
        && host != "localhost"
    {
        names.push(host);
    }
    let certified =
        rcgen::generate_simple_self_signed(names).context("generating TLS certificate")?;
    let cert = CertificateDer::from(certified.cert);
    let key = PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der());
    fs::create_dir_all(data_dir).context("creating data directory")?;
    fs::write(&cert_path, cert.as_ref()).context("writing TLS certificate")?;
    fs::write(&key_path, key.secret_pkcs8_der()).context("writing TLS key")?;
    Ok((cert, key))
}

pub fn server_crypto(
    cert: CertificateDer<'static>,
    key: PrivatePkcs8KeyDer<'static>,
) -> Result<quinn::crypto::rustls::QuicServerConfig> {
    install_provider();
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key.into())
        .context("building TLS server config")?;
    config.alpn_protocols = vec![super::protocol::ALPN.to_vec()];
    quinn::crypto::rustls::QuicServerConfig::try_from(config)
        .context("building QUIC server TLS config")
}

pub fn client_crypto(
    expected: Option<String>,
) -> Result<(quinn::crypto::rustls::QuicClientConfig, Arc<TofuVerifier>)> {
    install_provider();
    let verifier = Arc::new(TofuVerifier {
        expected,
        provider: rustls::crypto::ring::default_provider(),
        seen: std::sync::Mutex::new(None),
    });
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    config.alpn_protocols = vec![super::protocol::ALPN.to_vec()];
    Ok((
        quinn::crypto::rustls::QuicClientConfig::try_from(config)
            .context("building QUIC client TLS config")?,
        verifier,
    ))
}

#[derive(Debug)]
pub struct TofuVerifier {
    expected: Option<String>,
    provider: rustls::crypto::CryptoProvider,
    pub seen: std::sync::Mutex<Option<String>>,
}

impl TofuVerifier {
    pub fn fingerprint(&self) -> Option<String> {
        self.seen.lock().ok().and_then(|guard| guard.clone())
    }
}

impl ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let found = fingerprint(end_entity);
        if let Some(expected) = &self.expected
            && expected != &found
        {
            return Err(TlsError::General(format!(
                "server certificate fingerprint {found} does not match pinned {expected}"
            )));
        }
        if let Ok(mut seen) = self.seen.lock() {
            *seen = Some(found);
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls12_signature(
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
    ) -> Result<HandshakeSignatureValid, TlsError> {
        verify_tls13_signature(
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

pub fn install_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn require_dir(path: &Path) -> Result<()> {
    if path.exists() && !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("creating {}", path.display()))
        .map(|_| ())
}
