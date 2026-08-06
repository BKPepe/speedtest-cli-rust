//! HTTP client built directly on hyper so that socket binding options
//! (`--source`, `--interface`, `--fwmark`) can be honoured.

pub mod connector;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _};
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_TYPE, USER_AGENT};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use url::Url;

use connector::BoundConnector;
pub use connector::{BindOptions, IpFamily};

/// Go's `http.Client` follows at most 10 redirects by default.
const MAX_REDIRECTS: usize = 10;

pub type ReqBody = BoxBody<Bytes, io::Error>;

/// An empty request body.
pub fn empty_body() -> ReqBody {
    Empty::<Bytes>::new().map_err(|e| match e {}).boxed()
}

/// A request body holding the given bytes.
pub fn full_body(b: Bytes) -> ReqBody {
    Full::new(b).map_err(|e| match e {}).boxed()
}

/// TLS trust configuration.
pub struct TlsSettings<'a> {
    /// PEM bundle replacing the system trust store (`--ca-cert`).
    pub ca_cert: Option<&'a Path>,
    /// Accept any certificate (`--skip-cert-verify`).
    pub skip_verify: bool,
}

/// Certificate verifier that accepts everything, for `--skip-cert-verify`.
#[derive(Debug)]
struct NoVerifier(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn build_tls_config(tls: &TlsSettings<'_>) -> anyhow::Result<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    // ALPN is left untouched: hyper-rustls sets it from `enable_http1()`.
    if tls.skip_verify {
        return Ok(
            rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerifier(provider)))
                .with_no_client_auth(),
        );
    }

    let mut roots = rustls::RootCertStore::empty();
    match tls.ca_cert {
        // `--ca-cert` replaces the system trust store, as it does in the Go version.
        Some(path) => {
            let pem = std::fs::read(path)
                .with_context(|| format!("cannot read CA certificate bundle {}", path.display()))?;
            let mut reader = std::io::BufReader::new(pem.as_slice());
            for cert in rustls_pemfile::certs(&mut reader) {
                roots.add(cert?)?;
            }
            if roots.is_empty() {
                bail!("no certificates found in {}", path.display());
            }
        }
        None => {
            let native = rustls_native_certs::load_native_certs();
            for cert in native.certs {
                // Ignore individual unparsable roots, like Go's system pool does.
                let _ = roots.add(cert);
            }
            if roots.is_empty() {
                bail!("could not load any certificate from the system trust store");
            }
        }
    }

    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth())
}

/// The program's HTTP client.
#[derive(Clone)]
pub struct HttpClient {
    inner: Client<HttpsConnector<BoundConnector>, ReqBody>,
    timeout: Duration,
    user_agent: HeaderValue,
}

impl HttpClient {
    pub fn new(
        bind: BindOptions,
        tls: &TlsSettings<'_>,
        timeout: Duration,
        concurrent: usize,
        user_agent: &str,
    ) -> anyhow::Result<Self> {
        let tls_config = build_tls_config(tls)?;

        let https = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(tls_config)
            .https_or_http()
            .enable_http1()
            .wrap_connector(BoundConnector::new(bind));

        // Keep enough connections alive for every concurrent stream, matching the
        // Go version's MaxIdleConnsPerHost/MaxConnsPerHost tuning.
        let inner = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(concurrent + 2)
            .build(https);

        Ok(Self {
            inner,
            timeout,
            user_agent: HeaderValue::from_str(user_agent)?,
        })
    }

    /// The configured per-request timeout (`--timeout`).
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Issues a request, following redirects the way Go's `http.Client` does.
    ///
    /// `mk_body` is called once per attempt so that a redirected request can be
    /// replayed with a fresh body.
    pub async fn request<F>(
        &self,
        method: Method,
        url: &Url,
        headers: &[(HeaderName, HeaderValue)],
        mk_body: F,
    ) -> anyhow::Result<Response<Incoming>>
    where
        F: Fn() -> ReqBody,
    {
        let mut url = url.clone();
        let mut method = method;

        for _ in 0..=MAX_REDIRECTS {
            let uri: Uri = url
                .as_str()
                .parse()
                .with_context(|| format!("invalid URL: {url}"))?;

            let mut builder = Request::builder().method(method.clone()).uri(uri);
            builder = builder.header(USER_AGENT, self.user_agent.clone());
            for (name, value) in headers {
                builder = builder.header(name.clone(), value.clone());
            }

            let body = if method == Method::GET || method == Method::HEAD {
                empty_body()
            } else {
                mk_body()
            };

            let resp = self.inner.request(builder.body(body)?).await?;

            let status = resp.status();
            if !status.is_redirection() {
                return Ok(resp);
            }

            let Some(location) = resp.headers().get(http::header::LOCATION) else {
                return Ok(resp);
            };
            let location = location.to_str().context("invalid Location header")?;
            let next = url.join(location).context("invalid redirect target")?;

            // 301/302/303 turn the request into a GET; 307/308 replay it as-is.
            if matches!(
                status,
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
            ) {
                method = Method::GET;
            }
            url = next;
        }

        bail!("stopped after {MAX_REDIRECTS} redirects")
    }

    /// Performs a GET request and reads the whole response body.
    pub async fn get_bytes(&self, url: &Url) -> anyhow::Result<(StatusCode, Bytes)> {
        let fut = async {
            let resp = self.request(Method::GET, url, &[], empty_body).await?;
            let status = resp.status();
            let body = resp.into_body().collect().await?.to_bytes();
            Ok::<_, anyhow::Error>((status, body))
        };

        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| anyhow::anyhow!("request timed out after {:?}", self.timeout))?
    }

    /// Posts a body and reads the whole response, used for telemetry.
    pub async fn post_bytes(
        &self,
        url: &Url,
        content_type: &str,
        body: Bytes,
    ) -> anyhow::Result<(StatusCode, Bytes)> {
        let headers = vec![(CONTENT_TYPE, HeaderValue::from_str(content_type)?)];
        let fut = async {
            let resp = self
                .request(Method::POST, url, &headers, || full_body(body.clone()))
                .await?;
            let status = resp.status();
            let out = resp.into_body().collect().await?.to_bytes();
            Ok::<_, anyhow::Error>((status, out))
        };

        tokio::time::timeout(self.timeout, fut)
            .await
            .map_err(|_| anyhow::anyhow!("request timed out after {:?}", self.timeout))?
    }

    /// Sends a request without buffering the response body, for the transfer tests.
    pub async fn send_streaming<F>(
        &self,
        method: Method,
        url: &Url,
        mk_body: F,
    ) -> anyhow::Result<Response<Incoming>>
    where
        F: Fn() -> ReqBody,
    {
        // Speed tests must measure the wire, not a decompressed stream.
        let headers = vec![(ACCEPT_ENCODING, HeaderValue::from_static("identity"))];
        self.request(method, url, &headers, mk_body).await
    }
}
