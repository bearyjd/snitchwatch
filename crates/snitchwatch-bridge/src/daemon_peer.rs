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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tonic::transport::server::TcpConnectInfo;

    fn tcp(port: u16) -> TcpConnectInfo {
        TcpConnectInfo {
            local_addr: None,
            remote_addr: Some(SocketAddr::from(([127, 0, 0, 1], port))),
        }
    }

    fn unix(id: u64) -> UnixPeerInfo {
        UnixPeerInfo {
            id,
            uid: 0,
            pid: None,
        }
    }

    #[test]
    fn tcp_info_gives_the_peer_address() {
        let mut req = Request::new(());
        req.extensions_mut().insert(tcp(4000));
        assert_eq!(
            conn_key(&req),
            Some(PeerKey::Tcp(SocketAddr::from(([127, 0, 0, 1], 4000))))
        );
    }

    #[test]
    fn unix_info_gives_the_connection_id() {
        let mut req = Request::new(());
        req.extensions_mut().insert(unix(7));
        assert_eq!(conn_key(&req), Some(PeerKey::Unix(7)));
    }

    #[test]
    fn no_connection_info_gives_none() {
        assert_eq!(conn_key(&Request::new(())), None);
    }

    #[test]
    fn tcp_info_without_an_address_gives_none() {
        let mut req = Request::new(());
        req.extensions_mut().insert(TcpConnectInfo {
            local_addr: None,
            remote_addr: None,
        });
        assert_eq!(conn_key(&req), None);
    }

    #[test]
    fn tcp_wins_when_both_are_present() {
        let mut req = Request::new(());
        req.extensions_mut().insert(unix(1));
        req.extensions_mut().insert(tcp(4001));
        assert!(matches!(conn_key(&req), Some(PeerKey::Tcp(_))));
    }

    #[test]
    fn peer_keys_of_different_connections_differ() {
        assert_ne!(Some(PeerKey::Unix(1)), Some(PeerKey::Unix(2)));
        assert_ne!(Some(PeerKey::Unix(1)), None);
        assert_ne!(
            PeerKey::Tcp(SocketAddr::from(([127, 0, 0, 1], 1))),
            PeerKey::Tcp(SocketAddr::from(([127, 0, 0, 1], 2)))
        );
        assert_ne!(
            PeerKey::Unix(1),
            PeerKey::Tcp(SocketAddr::from(([127, 0, 0, 1], 1)))
        );
    }
}
