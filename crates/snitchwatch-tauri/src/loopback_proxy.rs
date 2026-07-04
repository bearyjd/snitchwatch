//! Local TCP↔Unix-domain-socket loopback proxy for the Tauri webview.
//!
//! The bridge's WS+HTTP server binds a Unix domain socket under
//! `$XDG_RUNTIME_DIR/snitchwatch/` (see `snitchwatch_bridge::ws_server`) —
//! required so a future Flatpak-sandboxed GUI can reach it at all (see
//! `docs/superpowers/specs/2026-07-04-flatpak-feasibility-research.md`).
//! Today's Tauri shell still points its webview window at a plain
//! `http://127.0.0.1:3031/` URL (`tauri.conf.json`'s `app.windows[0].url`):
//! WebKitGTK's `WebSocket`/`fetch` implementations have no way to dial a
//! Unix domain socket directly, so this module bridges the two. It binds a
//! loopback TCP listener the webview can reach at the same address it
//! always has, and transparently proxies bytes to/from the bridge's Unix
//! socket for every accepted connection — the static-asset routes (`/`,
//! `/assets/*`) are pure byte-for-byte passthrough.
//!
//! The `/stream` WebSocket route is the one exception: the bridge requires
//! a handshake token as the first WS text frame (see
//! `snitchwatch_bridge::auth`) before it treats the connection as trusted.
//! Rather than changing the webview's JS to fetch and send that token
//! itself, this proxy reads the token once (already known by the in-process
//! bridge) and injects it as a raw, correctly-masked WS frame on the
//! bridge-facing side of any `/stream` connection — right after the WS
//! upgrade handshake completes, before relaying anything the client itself
//! sends. The webview's own WS client code is completely unaware this
//! happens.

use snitchwatch_bridge::auth::Token;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixStream};
use tracing::debug;

/// Bind a loopback TCP listener at `tcp_bind` and proxy every connection to
/// the Unix socket at `socket_path`, transparently injecting `token` as the
/// first WS frame for `/stream` connections. Runs until the listener errors;
/// callers abort the returned task handle to stop it.
pub async fn run(tcp_bind: SocketAddr, socket_path: PathBuf, token: Token) -> io::Result<()> {
    let listener = TcpListener::bind(tcp_bind).await?;
    loop {
        let (tcp_stream, _peer) = listener.accept().await?;
        let socket_path = socket_path.clone();
        let token = token.clone();
        tokio::spawn(async move {
            if let Err(e) = proxy_connection(tcp_stream, &socket_path, &token).await {
                debug!(error = %e, "loopback proxy connection ended");
            }
        });
    }
}

async fn proxy_connection(
    mut tcp_stream: TcpStream,
    socket_path: &Path,
    token: &Token,
) -> io::Result<()> {
    let mut uds_stream = UnixStream::connect(socket_path).await?;

    // Peek the request line to decide whether this is the `/stream` upgrade
    // (which needs the token injected) or a plain asset GET (byte-for-byte
    // passthrough, no further protocol awareness needed).
    let request_head = read_http_head(&mut tcp_stream).await?;
    let is_stream = is_stream_request(&request_head);
    uds_stream.write_all(&request_head).await?;

    if is_stream {
        // Relay the bridge's upgrade response headers verbatim (a 101
        // Switching Protocols response has no body) before injecting the
        // token as the connection's first WS frame.
        let response_head = read_http_head(&mut uds_stream).await?;
        tcp_stream.write_all(&response_head).await?;
        uds_stream
            .write_all(&encode_ws_text_frame(token.as_str()))
            .await?;
    }

    tokio::io::copy_bidirectional(&mut tcp_stream, &mut uds_stream).await?;
    Ok(())
}

/// Read bytes from `stream` up to and including the terminating `\r\n\r\n`
/// that ends an HTTP request/response head.
async fn read_http_head<R: AsyncRead + Unpin>(stream: &mut R) -> io::Result<Vec<u8>> {
    const MAX_HEAD_LEN: usize = 64 * 1024;
    let mut buf = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before HTTP head completed",
            ));
        }
        buf.push(byte[0]);
        if buf.len() >= 4 && buf[buf.len() - 4..] == *b"\r\n\r\n" {
            break;
        }
        if buf.len() > MAX_HEAD_LEN {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP head exceeded maximum size",
            ));
        }
    }
    Ok(buf)
}

/// Whether an HTTP request head's request line targets `/stream`.
fn is_stream_request(head: &[u8]) -> bool {
    let first_line_end = head
        .windows(2)
        .position(|w| w == b"\r\n")
        .unwrap_or(head.len());
    let first_line = String::from_utf8_lossy(&head[..first_line_end]);
    first_line.split_whitespace().nth(1) == Some("/stream")
}

/// Encode `payload` as a single, masked WS text frame (RFC 6455 §5.2) —
/// client-to-server frames must be masked.
fn encode_ws_text_frame(payload: &str) -> Vec<u8> {
    let payload_bytes = payload.as_bytes();
    let len = payload_bytes.len();
    let mut frame = Vec::with_capacity(2 + 8 + 4 + len);
    frame.push(0x81); // FIN + text opcode
    if len < 126 {
        frame.push(0x80 | (len as u8));
    } else if len <= 0xFFFF {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }
    let mask: [u8; 4] = rand::random();
    frame.extend_from_slice(&mask);
    for (i, b) in payload_bytes.iter().enumerate() {
        frame.push(b ^ mask[i % 4]);
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_stream_request_matches_stream_path() {
        let head = b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(is_stream_request(head));
    }

    #[test]
    fn is_stream_request_rejects_other_paths() {
        let head = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(!is_stream_request(head));
        let head = b"GET /assets/js/app.js HTTP/1.1\r\nHost: x\r\n\r\n";
        assert!(!is_stream_request(head));
    }

    #[tokio::test]
    async fn read_http_head_reads_up_to_terminator() {
        let raw = b"GET /stream HTTP/1.1\r\nHost: x\r\n\r\nEXTRA".to_vec();
        let mut cursor = std::io::Cursor::new(raw.clone());
        let head = read_http_head(&mut cursor).await.unwrap();
        assert_eq!(head, raw[..raw.len() - "EXTRA".len()]);
    }

    #[test]
    fn encode_ws_text_frame_is_masked_and_decodes_back() {
        let frame = encode_ws_text_frame("hello-token");
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1] & 0x80, 0x80, "MASK bit must be set");
        let len = (frame[1] & 0x7F) as usize;
        assert_eq!(len, "hello-token".len());
        let mask = &frame[2..6];
        let payload = &frame[6..];
        let decoded: Vec<u8> = payload
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ mask[i % 4])
            .collect();
        assert_eq!(decoded, b"hello-token");
    }

    #[tokio::test]
    async fn proxy_forwards_asset_request_and_response_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("bridge.sock");

        // A minimal "bridge" that just echoes back a canned HTTP response
        // for any request, so we can assert the proxy forwards it intact.
        let uds_listener = tokio::net::UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = uds_listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK")
                .await
                .unwrap();
        });

        let tcp_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = tcp_listener.local_addr().unwrap();
        let token = Token::generate();
        let socket_path_for_proxy = socket_path.clone();
        let proxy = tokio::spawn(async move {
            let (tcp_stream, _) = tcp_listener.accept().await.unwrap();
            proxy_connection(tcp_stream, &socket_path_for_proxy, &token)
                .await
                .ok();
        });

        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        // Half-close the write side: this is a one-shot request with no more
        // data coming, and lets `copy_bidirectional` observe EOF on both
        // legs so the proxy task can finish (matching a real client that
        // eventually stops writing on that request).
        tokio::io::AsyncWriteExt::shutdown(&mut client)
            .await
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert!(response.ends_with(b"OK"));

        server.await.unwrap();
        proxy.await.unwrap();
    }
}
