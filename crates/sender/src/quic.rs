//! Minimal TPU QUIC client (Agave streamer protocol, VERIFIED_FACTS F9):
//! ALPN `solana-tpu`, self-signed client certificate (we are unstaked, so the key is
//! irrelevant to QoS), server certificate not verified (validators use self-signed certs),
//! one unidirectional stream per transaction, stream finished after the bytes are written.
//!
//! Connecting (`probe`) needs no authorization; opening a stream and writing a transaction
//! requires `&LiveAuth`.

use crate::live::LiveAuth;
use anyhow::{Context, Result};
use quinn::crypto::rustls::QuicClientConfig;
use rustls::DigitallySignedStruct;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const ALPN_TPU: &[u8] = b"solana-tpu";

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

pub struct TpuClient {
    endpoint: quinn::Endpoint,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConnectInfo {
    pub addr: SocketAddr,
    pub handshake_us: u64,
    pub rtt_us: u64,
    pub stable_id: usize,
}

pub struct TpuConnection {
    conn: quinn::Connection,
    pub info: ConnectInfo,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct StreamWrite {
    pub stream_id: u64,
}

pub fn self_signed_client_cert() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["solana-leader-window-lab".into()])?;
    let key = PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der());
    Ok((cert.cert.der().clone(), key.into()))
}

impl TpuClient {
    pub fn new() -> Result<Self> {
        crate::rpc::init_crypto();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let (cert, key) = self_signed_client_cert()?;
        let mut tls = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification(provider)))
            .with_client_auth_cert(vec![cert], key)?;
        tls.alpn_protocols = vec![ALPN_TPU.to_vec()];
        let mut cfg = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(tls)?));
        let mut transport = quinn::TransportConfig::default();
        transport.keep_alive_interval(Some(Duration::from_secs(1)));
        transport.max_idle_timeout(Some(Duration::from_secs(10).try_into()?));
        cfg.transport_config(Arc::new(transport));
        let mut endpoint = quinn::Endpoint::client("0.0.0.0:0".parse()?)?;
        endpoint.set_default_client_config(cfg);
        Ok(Self { endpoint })
    }

    /// Open (pre-warm) a connection. No transaction bytes are sent.
    pub async fn connect(&self, addr: SocketAddr) -> Result<TpuConnection> {
        let t = Instant::now();
        let conn = self
            .endpoint
            .connect(addr, "solana-tpu")?
            .await
            .with_context(|| format!("QUIC handshake to {addr}"))?;
        let info = ConnectInfo {
            addr,
            handshake_us: t.elapsed().as_micros() as u64,
            rtt_us: conn.rtt().as_micros() as u64,
            stable_id: conn.stable_id(),
        };
        Ok(TpuConnection { conn, info })
    }
}

impl TpuConnection {
    pub fn rtt_us(&self) -> u64 {
        self.conn.rtt().as_micros() as u64
    }

    /// Opens a uni stream handle. QUIC streams are only announced to the peer by their first
    /// frame, so this is free to do ahead of the burst.
    pub async fn open_stream(&self, _auth: &LiveAuth) -> Result<quinn::SendStream> {
        Ok(self.conn.open_uni().await?)
    }

    pub async fn write_tx(
        _auth: &LiveAuth,
        mut stream: quinn::SendStream,
        bytes: &[u8],
    ) -> Result<StreamWrite> {
        let stream_id = u64::from(stream.id().index());
        stream.write_all(bytes).await?;
        stream.finish()?;
        Ok(StreamWrite { stream_id })
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"done");
    }
}

/// In-process TPU-like QUIC server used by tests: counts streams and records their bytes.
#[cfg(test)]
pub mod test_server {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct Seen {
        pub connections: usize,
        pub streams: Vec<Vec<u8>>,
    }

    pub async fn spawn() -> Result<(SocketAddr, Arc<Mutex<Seen>>)> {
        crate::rpc::init_crypto();
        let (cert, key) = self_signed_client_cert()?;
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)?;
        tls.alpn_protocols = vec![ALPN_TPU.to_vec()];
        let cfg = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(tls)?,
        ));
        let ep = quinn::Endpoint::server(cfg, "127.0.0.1:0".parse()?)?;
        let addr = ep.local_addr()?;
        let seen = Arc::new(Mutex::new(Seen::default()));
        let s2 = seen.clone();
        tokio::spawn(async move {
            while let Some(inc) = ep.accept().await {
                let s3 = s2.clone();
                tokio::spawn(async move {
                    let Ok(conn) = inc.await else { return };
                    s3.lock().unwrap().connections += 1;
                    while let Ok(mut rx) = conn.accept_uni().await {
                        let data = rx.read_to_end(4096).await.unwrap_or_default();
                        s3.lock().unwrap().streams.push(data);
                    }
                });
            }
        });
        Ok((addr, seen))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_connect_opens_no_streams() {
        let (addr, seen) = test_server::spawn().await.unwrap();
        let client = TpuClient::new().unwrap();
        let conn = client.connect(addr).await.unwrap();
        assert!(conn.info.handshake_us > 0);
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.close();
        let s = seen.lock().unwrap();
        assert_eq!(s.connections, 1);
        assert!(s.streams.is_empty());
    }

    #[tokio::test]
    async fn live_write_delivers_exact_bytes_one_stream_per_tx() {
        let (addr, seen) = test_server::spawn().await.unwrap();
        let client = TpuClient::new().unwrap();
        let conn = client.connect(addr).await.unwrap();
        let auth = LiveAuth::for_tests(1);
        for i in 0..3u8 {
            let st = conn.open_stream(&auth).await.unwrap();
            TpuConnection::write_tx(&auth, st, &[i; 100]).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let s = seen.lock().unwrap();
        assert_eq!(s.streams.len(), 3);
        assert!(s.streams.iter().any(|b| b == &vec![2u8; 100]));
    }
}
