//! A TCP connector with full socket-level control.
//!
//! This exists instead of an off-the-shelf HTTP client connector because the CLI
//! needs to bind sockets to a source address, a network interface
//! (`SO_BINDTODEVICE` / `IP_BOUND_IF`) and a firewall mark (`SO_MARK`) — the last
//! of which no high-level Rust HTTP client exposes.

use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use http::Uri;
use hyper_util::rt::TokioIo;
use socket2::{SockRef, TcpKeepalive};
use tokio::net::{TcpSocket, TcpStream};

/// Matches the Go implementation's `net.Dialer{Timeout: 30s, KeepAlive: 30s}`.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const KEEPALIVE: Duration = Duration::from_secs(30);

/// Which IP address family connections are restricted to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IpFamily {
    #[default]
    Any,
    V4,
    V6,
}

impl IpFamily {
    pub fn accepts(&self, addr: &SocketAddr) -> bool {
        match self {
            IpFamily::Any => true,
            IpFamily::V4 => addr.is_ipv4(),
            IpFamily::V6 => addr.is_ipv6(),
        }
    }

    /// The equivalent of Go's `ip` / `ip4` / `ip6` network strings.
    pub fn network(&self) -> &'static str {
        match self {
            IpFamily::Any => "ip",
            IpFamily::V4 => "ip4",
            IpFamily::V6 => "ip6",
        }
    }
}

/// Socket binding options applied to every outgoing connection.
#[derive(Clone, Debug, Default)]
pub struct BindOptions {
    /// Local source address to bind to (`--source`).
    pub source: Option<IpAddr>,
    /// Network interface to bind to (`--interface`).
    pub interface: Option<String>,
    /// Firewall mark to set on the socket (`--fwmark`), 0 means unset.
    pub fwmark: u32,
    /// Restrict connections to one address family (`--ipv4` / `--ipv6`).
    pub family: IpFamily,
}

impl BindOptions {
    /// Fails early, with the same wording as the Go implementation, when the
    /// platform cannot honour the requested interface or fwmark binding.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.interface.is_none() && self.fwmark == 0 {
            return Ok(());
        }
        #[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
        {
            Ok(())
        }
        #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "fuchsia")))]
        {
            // IP_BOUND_IF gives us interface binding on Apple platforms; there is
            // no portable equivalent of SO_MARK anywhere but Linux.
            if self.fwmark > 0 {
                anyhow::bail!("cannot set a firewall mark on this platform");
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            {
                Ok(())
            }
            #[cfg(not(any(target_os = "macos", target_os = "ios")))]
            {
                anyhow::bail!("cannot bound to interface on this platform")
            }
        }
    }
}

/// Resolves `host:port`, keeping only addresses of the configured family.
pub async fn resolve(host: &str, port: u16, family: IpFamily) -> io::Result<Vec<SocketAddr>> {
    // Strip brackets from IPv6 literals such as `[::1]`.
    let host = host.trim_start_matches('[').trim_end_matches(']');

    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await?
        .filter(|a| family.accepts(a))
        .collect();

    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("no {} address found for {host}", family.network()),
        ));
    }
    Ok(addrs)
}

/// Applies the interface and firewall mark options to a socket.
fn apply_socket_options(
    socket: &TcpSocket,
    opts: &BindOptions,
    addr: &SocketAddr,
) -> io::Result<()> {
    let sock = SockRef::from(socket);

    sock.set_tcp_keepalive(&TcpKeepalive::new().with_time(KEEPALIVE))?;

    if let Some(iface) = &opts.interface {
        bind_to_interface(&sock, iface, addr)?;
    }

    if opts.fwmark > 0 {
        set_fwmark(&sock, opts.fwmark)?;
    }

    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
fn bind_to_interface(sock: &SockRef<'_>, iface: &str, _addr: &SocketAddr) -> io::Result<()> {
    // On Linux SO_BINDTODEVICE really binds the socket to the device, instead of
    // binding to an address that would still be subject to the default routes.
    sock.bind_device(Some(iface.as_bytes()))
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn bind_to_interface(sock: &SockRef<'_>, iface: &str, addr: &SocketAddr) -> io::Result<()> {
    let name = std::ffi::CString::new(iface)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interface name contains NUL"))?;
    // SAFETY: `name` is a valid NUL-terminated C string for the duration of the call.
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    let index = std::num::NonZeroU32::new(index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("no such interface: {iface}"),
        )
    })?;
    if addr.is_ipv4() {
        sock.bind_device_by_index_v4(Some(index))
    } else {
        sock.bind_device_by_index_v6(Some(index))
    }
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "fuchsia",
    target_os = "macos",
    target_os = "ios"
)))]
fn bind_to_interface(_sock: &SockRef<'_>, _iface: &str, _addr: &SocketAddr) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cannot bound to interface on this platform",
    ))
}

#[cfg(any(target_os = "linux", target_os = "android", target_os = "fuchsia"))]
fn set_fwmark(sock: &SockRef<'_>, fwmark: u32) -> io::Result<()> {
    sock.set_mark(fwmark)
}

#[cfg(not(any(target_os = "linux", target_os = "android", target_os = "fuchsia")))]
fn set_fwmark(_sock: &SockRef<'_>, _fwmark: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cannot set a firewall mark on this platform",
    ))
}

/// Opens a single TCP connection to `addr` honouring all bind options.
async fn connect_one(addr: SocketAddr, opts: &BindOptions) -> io::Result<TcpStream> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };

    socket.set_nodelay(true)?;
    apply_socket_options(&socket, opts, &addr)?;

    if let Some(src) = opts.source {
        // A source address can only be bound to a socket of the same family.
        if src.is_ipv4() == addr.is_ipv4() {
            socket.bind(SocketAddr::new(src, 0))?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source address family does not match destination",
            ));
        }
    }

    tokio::time::timeout(CONNECT_TIMEOUT, socket.connect(addr))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connection timed out"))?
}

/// A `tower` connector that produces socket-bound TCP streams.
#[derive(Clone, Debug)]
pub struct BoundConnector {
    opts: BindOptions,
}

impl BoundConnector {
    pub fn new(opts: BindOptions) -> Self {
        Self { opts }
    }
}

impl tower_service::Service<Uri> for BoundConnector {
    type Response = TokioIo<TcpStream>;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, dst: Uri) -> Self::Future {
        let opts = self.opts.clone();
        Box::pin(async move {
            let host = dst
                .host()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "URI has no host"))?;
            let port = dst.port_u16().unwrap_or(match dst.scheme_str() {
                Some("https") => 443,
                _ => 80,
            });

            let addrs = resolve(host, port, opts.family).await?;

            let mut last_err = None;
            for addr in addrs {
                match connect_one(addr, &opts).await {
                    Ok(stream) => return Ok(TokioIo::new(stream)),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(last_err.unwrap_or_else(|| {
                io::Error::new(io::ErrorKind::AddrNotAvailable, "no address to connect to")
            }))
        })
    }
}
