//! Maestro over Streamable HTTP (`maestro serve --http`), meant to run once per machine as
//! `maestrod`. Nothing here enforces that; node `deploy` (BRO-3285) installs the one service.
//!
//! Clients on MCP 2026-07-28 are served statelessly; older clients that open
//! with `initialize` get a legacy session. Either way every request reads a
//! store resolved for that request ([`MaestroServer::store`]), so the daemon
//! follows `origin/main` as it moves.
//!
//! Where it listens is a [`Bind`]: loopback unless an address is named, and a
//! named address must be loopback or on the tailnet. The unspecified address
//! (`0.0.0.0`, `::`) and every other address are refused.

use crate::MaestroServer;
use maestro_store::Store;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// The port the daemon listens on when none is named.
pub const DEFAULT_PORT: u16 = 7171;
/// The HTTP path of the MCP endpoint.
pub const PATH: &str = "/mcp";
/// How long a stop waits for requests in flight before it drops them. Reads finish in well
/// under this; a client that stalls (a body it never finishes, a stream it never closes)
/// does not hold the daemon past it.
pub const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// An address the daemon may listen on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind(SocketAddr);

/// Why a named address was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindRefused(pub SocketAddr);

impl fmt::Display for BindRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "refusing to listen on {}: only a loopback or tailnet address (100.64.0.0/10, fd7a:115c:a1e0::/48) may be named",
            self.0
        )
    }
}

impl std::error::Error for BindRefused {}

impl Bind {
    /// The default: `127.0.0.1` on [`DEFAULT_PORT`].
    pub fn loopback() -> Self {
        Self(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT))
    }

    /// An address named with `--bind`: loopback or tailnet, nothing else.
    pub fn named(addr: SocketAddr) -> Result<Self, BindRefused> {
        if addr.ip().is_loopback() || is_tailnet(addr.ip()) { Ok(Self(addr)) } else { Err(BindRefused(addr)) }
    }

    /// What `--bind` resolves to: loopback when absent, else the named address if it may be named.
    pub fn from_arg(arg: Option<SocketAddr>) -> Result<Self, BindRefused> {
        match arg {
            None => Ok(Self::loopback()),
            Some(addr) => Self::named(addr),
        }
    }

    pub fn addr(self) -> SocketAddr {
        self.0
    }
}

/// Tailscale's address ranges: CGNAT `100.64.0.0/10` and ULA `fd7a:115c:a1e0::/48`.
fn is_tailnet(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            a == 100 && (b & 0xc0) == 64
        }
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0] || mapped_tailnet(v6),
    }
}

fn mapped_tailnet(v6: Ipv6Addr) -> bool {
    v6.to_ipv4_mapped().is_some_and(|v4| is_tailnet(IpAddr::V4(v4)))
}

/// The tower service for one listener. `local` is the address actually bound:
/// requests must name it (or a loopback name) in `Host`, which stops DNS rebinding,
/// and a request carrying any browser `Origin` is refused.
pub fn service(
    server: MaestroServer,
    local: SocketAddr,
    cancel: CancellationToken,
) -> StreamableHttpService<MaestroServer, LocalSessionManager> {
    let hosts = ["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string(), local.ip().to_string()];
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(true)
        .with_json_response(true)
        .with_allowed_hosts(hosts)
        .enforce_origin_validation()
        .with_cancellation_token(cancel);
    StreamableHttpService::new(move || Ok(server.clone()), Arc::new(LocalSessionManager::default()), config)
}

/// Bind the listener. Split from [`serve`] so a caller can report the bound port.
pub async fn listen(bind: Bind) -> std::io::Result<TcpListener> {
    TcpListener::bind(bind.addr()).await
}

/// Serve MCP on `listener` at [`PATH`] until `cancel` fires, then for at most [`SHUTDOWN_GRACE`].
pub async fn serve(store: Store, listener: TcpListener, cancel: CancellationToken) -> std::io::Result<()> {
    let local = listener.local_addr()?;
    let mcp = service(MaestroServer::new(store), local, cancel.child_token());
    let router = axum::Router::new().nest_service(PATH, mcp);
    let graceful = axum::serve(listener, router).with_graceful_shutdown(cancel.clone().cancelled_owned());
    tokio::select! {
        done = graceful.into_future() => done,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(s: &str) -> Result<Bind, BindRefused> {
        Bind::named(s.parse().unwrap())
    }

    #[test]
    fn the_default_is_loopback() {
        assert!(Bind::loopback().addr().ip().is_loopback());
        assert_eq!(Bind::loopback().addr().port(), DEFAULT_PORT);
    }

    #[test]
    fn no_bind_argument_means_loopback() {
        assert_eq!(Bind::from_arg(None), Ok(Bind::loopback()));
        assert!(Bind::from_arg(Some("0.0.0.0:7171".parse().unwrap())).is_err());
    }

    #[test]
    fn loopback_and_tailnet_may_be_named() {
        for ok in [
            "127.0.0.1:7171",
            "127.0.0.2:0",
            "[::1]:7171",
            "100.64.0.1:7171",
            "100.127.255.254:1",
            "[fd7a:115c:a1e0::1]:7171",
        ] {
            assert!(named(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn everything_else_is_refused() {
        for bad in [
            "0.0.0.0:7171",
            "[::]:7171",
            "192.168.1.10:7171",
            "10.0.0.1:7171",
            "100.63.255.255:7171",
            "100.128.0.0:7171",
            "8.8.8.8:7171",
            "[fd7a:115c:a1e1::1]:7171",
            "[::ffff:0.0.0.0]:7171",
            "[::ffff:192.168.1.10]:7171",
        ] {
            assert!(named(bad).is_err(), "{bad}");
        }
    }
}
