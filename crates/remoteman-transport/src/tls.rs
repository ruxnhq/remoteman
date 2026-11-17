//! TLS 1.3 where a certificate is trusted for its fingerprint and nothing else: no names, no
//! authorities, no expiry. The handshake signature is still checked, so holding the certificate
//! isn't enough; the peer must hold its key.

use std::sync::Arc;

use anyhow::Result;
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::identity::{Identity, Trust, fingerprint};
use crate::Expect;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct Clients {
    trust: Arc<Trust>,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for Clients {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        if self.trust.pairing() || self.trust.contains(&fingerprint(cert)) {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General("this device isn't paired".into()))
        }
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

#[derive(Debug)]
struct Host {
    /// `None`: pairing, accept whoever answers.
    expect: Option<String>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Host {
    fn verify_server_cert(&self, cert: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        match &self.expect {
            Some(fp) if *fp != fingerprint(cert) => Err(rustls::Error::General("this isn't the host that was paired".into())),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

pub fn server(identity: &Identity, trust: Arc<Trust>) -> Result<QuicServerConfig> {
    let provider = provider();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(Clients { trust, provider }))
        .with_single_cert(vec![identity.cert.clone()], identity.key())?;
    tls.alpn_protocols = vec![remoteman_proto::ALPN.to_vec()];
    Ok(QuicServerConfig::try_from(tls)?)
}

pub fn client(identity: &Identity, expect: Expect) -> Result<QuicClientConfig> {
    let provider = provider();
    let expect = match expect {
        Expect::Fingerprint(fp) => Some(fp),
        Expect::Anyone => None,
    };
    let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Host { expect, provider }))
        .with_client_auth_cert(vec![identity.cert.clone()], identity.key())?;
    tls.alpn_protocols = vec![remoteman_proto::ALPN.to_vec()];
    Ok(QuicClientConfig::try_from(tls)?)
}
