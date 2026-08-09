//! HTTP client built directly on hyper so that socket binding options
//! (`--source`, `--interface`, `--fwmark`) can be honoured.

pub mod connector;
pub mod tls;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _};
use bytes::{Bytes, BytesMut};
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

/// Cap on a buffered response body. The server list URL is user-supplied and
/// the entries in it name further hosts, so response sizes are attacker-chosen;
/// without a cap the client will buffer whatever it is fed until it runs out of
/// memory. Transfer test bodies are streamed and never buffered, so this only
/// bounds control-plane responses.
pub const MAX_BUFFERED_RESPONSE: usize = 8 * 1024 * 1024;
/// Telemetry replies are a short `id <n>` string.
pub const MAX_TELEMETRY_RESPONSE: usize = 64 * 1024;

/// HTTP/2 flow-control windows, matching what Go's transport uses
/// (`transportDefaultStreamFlow` / `transportDefaultConnFlow`).
///
/// The protocol default is 64 KiB, which caps a stream at window/RTT: about
/// 105 Mbps at 5 ms and 26 Mbps at 20 ms. Leaving it there would silently
/// understate any link faster than that.
const H2_STREAM_WINDOW: u32 = 4 * 1024 * 1024;
const H2_CONNECTION_WINDOW: u32 = 1024 * 1024 * 1024;

pub type ReqBody = BoxBody<Bytes, io::Error>;

/// Whether two URLs share a scheme, host and effective port.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// An empty request body.
pub fn empty_body() -> ReqBody {
    Empty::<Bytes>::new().map_err(|e| match e {}).boxed()
}

/// A request body holding the given bytes.
pub fn full_body(b: Bytes) -> ReqBody {
    Full::new(b).map_err(|e| match e {}).boxed()
}

/// Reads a response body, refusing to buffer more than `limit` bytes.
async fn collect_limited(body: Incoming, limit: usize) -> anyhow::Result<Bytes> {
    let mut body = body;
    let mut out = BytesMut::new();

    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            if out.len() + data.len() > limit {
                bail!("response body exceeds the {limit} byte limit");
            }
            out.extend_from_slice(data);
        }
    }

    Ok(out.freeze())
}

/// The program's HTTP client.
#[derive(Clone)]
pub struct HttpClient {
    inner: Client<tls::Connector, ReqBody>,
    /// Offers every suite, used once the forced one turns out to be refused.
    fallback: Option<Client<tls::Connector, ReqBody>>,
    /// Latches once the fallback has been needed, so it is tried only once.
    relaxed: Arc<AtomicBool>,
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
        let https = tls::build(bind.clone(), tls_settings)?;

        // When a single suite is being forced, keep a client that offers the
        // full set. A server that cannot do ChaCha20 would otherwise fail the
        // handshake outright, and a measurement tool should not refuse to
        // measure because it wanted a faster cipher.
        let fallback = if tls_settings.chacha_only {
            let relaxed = TlsSettings {
                chacha_only: false,
                ..*tls_settings
            };
            Some(tls::build(bind, &relaxed)?)
        } else {
            None
        };

        // Keep enough connections alive for every concurrent stream, matching the
        // Go version's MaxIdleConnsPerHost/MaxConnsPerHost tuning.
        let mut builder = Client::builder(TokioExecutor::new());
        builder.pool_max_idle_per_host(concurrent + 2);

        if tls_settings.http2 {
            builder
                .http2_initial_stream_window_size(H2_STREAM_WINDOW)
                .http2_initial_connection_window_size(H2_CONNECTION_WINDOW);
        }

        let inner = builder.build(https);
        let fallback = fallback.map(|f| {
            let mut b = Client::builder(TokioExecutor::new());
            b.pool_max_idle_per_host(concurrent + 2);
            if tls_settings.http2 {
                b.http2_initial_stream_window_size(H2_STREAM_WINDOW)
                    .http2_initial_connection_window_size(H2_CONNECTION_WINDOW);
            }
            b.build(f)
        });

        Ok(Self {
            inner,
            fallback,
            relaxed: Arc::new(AtomicBool::new(false)),
            timeout,
            user_agent: HeaderValue::from_str(user_agent)?,
        })
    }

    /// The client to issue the next request with.
    fn client(&self) -> &Client<tls::Connector, ReqBody> {
        match (&self.fallback, self.relaxed.load(Ordering::Relaxed)) {
            (Some(f), true) => f,
            _ => &self.inner,
        }
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

            let req = builder.body(body)?;
            let resp = match self.client().request(req).await {
                Ok(resp) => resp,
                Err(e) => {
                    // A server that will not do the forced suite refuses the
                    // handshake. Drop to the full set and try once more; the
                    // latch keeps this to a single switch per client.
                    if self.fallback.is_some() && !self.relaxed.swap(true, Ordering::Relaxed) {
                        let retry = Request::builder()
                            .method(method.clone())
                            .uri(url.as_str().parse::<Uri>()?);
                        let retry = headers.iter().fold(
                            retry.header(USER_AGENT, self.user_agent.clone()),
                            |b, (n, v)| b.header(n.clone(), v.clone()),
                        );
                        let body = if method == Method::GET || method == Method::HEAD {
                            empty_body()
                        } else {
                            mk_body()
                        };
                        self.client().request(retry.body(body)?).await?
                    } else {
                        return Err(e.into());
                    }
                }
            };

            let status = resp.status();
            if !status.is_redirection() {
                return Ok(resp);
            }

            let Some(location) = resp.headers().get(http::header::LOCATION) else {
                return Ok(resp);
            };
            let location = location.to_str().context("invalid Location header")?;
            let next = url.join(location).context("invalid redirect target")?;

            // Never let a redirect drop TLS. Downgrading would expose a request
            // that was deliberately made over https.
            if url.scheme() == "https" && next.scheme() != "https" {
                bail!(
                    "refusing to follow a redirect from https to {}",
                    next.scheme()
                );
            }

            // 301/302/303 turn the request into a GET; 307/308 replay it as-is.
            let becomes_get = matches!(
                status,
                StatusCode::MOVED_PERMANENTLY | StatusCode::FOUND | StatusCode::SEE_OTHER
            );

            // A replayed body must not be handed to a different origin: the
            // telemetry POST carries the measurement, the client's IP and the
            // ISP details, and 307/308 would resend all of it verbatim.
            //
            // Only requests that actually carry a body are affected. A 307/308
            // on a GET has nothing to replay, and refusing it broke the common
            // case of a scheme-less server list redirecting http to https.
            let carries_body = !matches!(method, Method::GET | Method::HEAD);
            if !becomes_get && carries_body && !same_origin(&url, &next) {
                bail!(
                    "refusing to replay a {method} body across origins ({} -> {})",
                    url.host_str().unwrap_or_default(),
                    next.host_str().unwrap_or_default()
                );
            }

            if becomes_get {
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
            let body = collect_limited(resp.into_body(), MAX_BUFFERED_RESPONSE).await?;
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
            let out = collect_limited(resp.into_body(), MAX_TELEMETRY_RESPONSE).await?;
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
