//! Which daemon connection a gRPC request came from (#391).
//!
//! tonic fills a request's extensions from the accepted connection's
//! `Connected::connect_info()`. TCP gives `TcpConnectInfo`; a Unix socket
//! carries a [`UnixPeerInfo`] once the accept path attaches one. The id is
//! assigned by the server at accept and never comes from the peer.

use crate::daemon_commands::{ConnKey, PeerKey};
use tonic::Request;

/// Connection info for one accepted Unix-socket daemon connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnixPeerInfo {
    /// Process-wide monotonic connection id; the only field a decision reads.
    pub id: u64,
    /// Peer credentials at accept, for log fields only.
    pub uid: u32,
    pub pid: Option<i32>,
}

/// The [`ConnKey`] of the connection `req` arrived on. `TcpConnectInfo` wins
/// over [`UnixPeerInfo`] (both cannot be present in production); no
/// connection info gives `None`.
pub fn conn_key<T>(req: &Request<T>) -> ConnKey {
    if let Some(addr) = req.remote_addr() {
        return Some(PeerKey::Tcp(addr));
    }
    req.extensions()
        .get::<UnixPeerInfo>()
        .map(|info| PeerKey::Unix(info.id))
}
