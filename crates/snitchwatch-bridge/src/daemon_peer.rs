//! Which daemon connection a gRPC request came from (#391).
//!
//! tonic fills a request's extensions from the accepted connection's
//! `Connected::connect_info()`. TCP gives `TcpConnectInfo`; the per-user
//! Unix socket carries a [`UnixPeerInfo`], which `OwnerUnixIncoming` attaches
//! at accept; the system socket does not, so its peers have no info. The id is
//! assigned by the server at accept and never comes from the peer.

use crate::daemon_commands::{ConnKey, PeerKey};
use std::sync::atomic::{AtomicU64, Ordering};
use tonic::Request;

/// Connection info for one accepted Unix-socket daemon connection. Fields
/// are private: the only constructor takes the id from a process-wide counter,
/// so no caller can choose one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnixPeerInfo {
    id: u64,
    uid: u32,
    pid: Option<i32>,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl UnixPeerInfo {
    /// Info for a connection accepted just now, with the next id (1, 2, ...).
    /// Call it only after the peer passed the uid check, so a rejected client
    /// consumes no id. `uid` and `pid` are for logs only.
    pub fn accept(uid: u32, pid: Option<i32>) -> Self {
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            uid,
            pid,
        }
    }

    /// A fixed id, for tests that stand in for the accept path.
    #[cfg(test)]
    pub(crate) fn for_tests(id: u64) -> Self {
        Self {
            id,
            uid: 0,
            pid: None,
        }
    }

    /// The connection id: the only field a decision reads.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Peer uid at accept. Log fields only; never a key, never trust.
    pub fn uid(&self) -> u32 {
        self.uid
    }

    /// Peer pid at accept, when known. Log fields only.
    pub fn pid(&self) -> Option<i32> {
        self.pid
    }
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
        .map(|info| PeerKey::Unix(info.id()))
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
        UnixPeerInfo::for_tests(id)
    }

    #[test]
    fn accepted_ids_are_unique_and_increasing_across_threads() {
        let mut ids: Vec<u64> = (0..8)
            .map(|_| {
                std::thread::spawn(|| {
                    (0..100)
                        .map(|_| UnixPeerInfo::accept(0, None).id())
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), before, "ids must be unique");
        let a = UnixPeerInfo::accept(0, None).id();
        assert!(UnixPeerInfo::accept(0, None).id() > a);
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
