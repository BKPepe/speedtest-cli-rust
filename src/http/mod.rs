//! HTTP client built directly on hyper so that socket binding options
//! (`--source`, `--interface`, `--fwmark`) can be honoured.

pub mod connector;
pub mod tls;

use std::io;
use std::time::Duration;

use anyhow::{bail, Context as _};
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_TYPE, USER_AGENT};
use http::{Method, Request, Response, StatusCode, Uri};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use url::Url;

pub use connector::{BindOptions, IpFamily};
pub use tls::TlsSettings;

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

/// The program's HTTP client.
#[derive(Clone)]
pub struct HttpClient {
    inner: Client<tls::Connector, ReqBody>,
    timeout: Duration,
    user_agent: HeaderValue,
}

impl HttpClient {
    pub fn new(
        bind: BindOptions,
        tls_settings: &TlsSettings<'_>,
        timeout: Duration,
        concurrent: usize,
        user_agent: &str,
    ) -> anyhow::Result<Self> {
        let https = tls::build(bind, tls_settings)?;

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
