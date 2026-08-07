//! WebSocket broadcast transport for browser clients.
//!
//! Browsers cannot open raw UDP sockets, so Phase 2 streams the same PCM
//! datagrams (see [`crate::protocol`]) over WebSockets. [`WebSocketServer`]
//! accepts one or more browser clients and broadcasts every packet to all of
//! them.

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use futures_util::{SinkExt, StreamExt};
use span_core::errors::NetworkError;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

/// Outbound message queue for one client connection.
type ClientSender = mpsc::Sender<Vec<u8>>;

/// Shared state between the accept loop, connection tasks, and the capture
/// loop that broadcasts packets.
#[derive(Default)]
struct Inner {
    clients: Mutex<HashMap<u64, ClientSender>>,
    next_id: AtomicU64,
}

/// A WebSocket broadcast hub.
///
/// Use [`WebSocketServer::bind`] to open a listener and [`WebSocketServer::accept_loop`]
/// to accept browser clients, or create a bare [`WebSocketServer::new`] and
/// register upgraded connections yourself (useful when serving the web client
/// from the same TCP listener).
#[derive(Clone)]
pub struct WebSocketServer {
    inner: Arc<Inner>,
    listener: Option<Arc<TcpListener>>,
}

impl Default for WebSocketServer {
    fn default() -> Self {
        Self::new()
    }
}

impl WebSocketServer {
    /// A server without a listener; register upgraded connections manually.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::default()),
            listener: None,
        }
    }

    /// Bind a TCP listener on `addr` and return the server plus its local
    /// address (useful for `0` ports). Call [`WebSocketServer::accept_loop`]
    /// to start accepting clients.
    pub async fn bind(
        addr: std::net::SocketAddr,
    ) -> Result<(Self, std::net::SocketAddr), NetworkError> {
        let listener = Arc::new(TcpListener::bind(addr).await.map_err(|e| {
            NetworkError::ReceiveError(format!("failed to bind WebSocket listener: {e}"))
        })?);
        let local = listener
            .local_addr()
            .map_err(|e| NetworkError::StatsError(e.to_string()))?;
        Ok((
            Self {
                inner: Arc::new(Inner::default()),
                listener: Some(listener),
            },
            local,
        ))
    }

    /// Accept WebSocket clients forever. Runs the WebSocket handshake on each
    /// incoming connection and registers it for broadcasts.
    pub async fn accept_loop(&self) -> Result<(), NetworkError> {
        let listener = self.listener.clone().ok_or_else(|| {
            NetworkError::StatsError("WebSocketServer has no listener; use bind()".into())
        })?;

        loop {
            let (stream, peer) = listener.accept().await.map_err(|e| {
                NetworkError::ReceiveError(format!("failed to accept connection: {e}"))
            })?;
            let server = self.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::accept_async(stream).await {
                    Ok(ws) => {
                        if let Err(e) = server.register(ws).await {
                            eprintln!("[span-transport] websocket client {peer}: {e}");
                        }
                    }
                    Err(e) => {
                        eprintln!("[span-transport] websocket handshake from {peer} failed: {e}")
                    }
                }
            });
        }
    }

    /// Upgrade a connection whose HTTP request head was already read by the
    /// caller (e.g. a combined HTTP + WebSocket server). The buffered request
    /// bytes are replayed so the handshake can be completed.
    pub async fn accept_with_prefix(
        stream: TcpStream,
        prefix: Vec<u8>,
    ) -> Result<WebSocketStream<PrefixedStream<TcpStream>>, NetworkError> {
        tokio_tungstenite::accept_async(PrefixedStream::new(prefix, stream))
            .await
            .map_err(|e| NetworkError::ReceiveError(format!("websocket handshake failed: {e}")))
    }

    /// Register an already-upgraded WebSocket connection. Spawns the reader
    /// and writer tasks for the connection and starts broadcasting to it.
    pub async fn register<S>(&self, ws: WebSocketStream<S>) -> Result<(), NetworkError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(128);
        self.inner.clients.lock().await.insert(id, tx);

        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            let writer = tokio::spawn(async move {
                while let Some(bytes) = rx.recv().await {
                    if sink.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
                let _ = sink.close().await;
            });

            // Drain inbound frames (usually empty for a pure receiver) so the
            // connection stays alive until the client disconnects.
            while stream.next().await.is_some() {}

            writer.abort();
            inner.clients.lock().await.remove(&id);
        });
        Ok(())
    }

    /// Broadcast one packet to every connected client. Dead connections are
    /// dropped from the client set.
    pub async fn broadcast(&self, payload: &[u8]) -> Result<(), NetworkError> {
        let mut clients = self.inner.clients.lock().await;
        let mut dead = Vec::new();
        for (id, sender) in clients.iter() {
            if sender.try_send(payload.to_vec()).is_err() {
                dead.push(*id);
            }
        }
        for id in dead {
            clients.remove(&id);
        }
        Ok(())
    }

    /// Number of currently connected clients.
    pub async fn client_count(&self) -> usize {
        self.inner.clients.lock().await.len()
    }
}

/// Wraps a stream with a prefix of already-read bytes so a WebSocket handshake
/// can be replayed from a buffered HTTP request head.
pub struct PrefixedStream<S> {
    prefix: io::Cursor<Vec<u8>>,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix: io::Cursor::new(prefix),
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for PrefixedStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let me = &mut *self;
        let prefix = me.prefix.get_ref();
        let pos = me.prefix.position() as usize;
        if pos < prefix.len() {
            let n = (prefix.len() - pos).min(buf.remaining());
            buf.put_slice(&prefix[pos..pos + n]);
            me.prefix.set_position((pos + n) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut me.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for PrefixedStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    async fn start_server() -> (WebSocketServer, std::net::SocketAddr) {
        let (server, addr) = WebSocketServer::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let srv = server.clone();
        tokio::spawn(async move {
            let _ = srv.accept_loop().await;
        });
        (server, addr)
    }

    async fn wait_for_clients(server: &WebSocketServer, expected: usize) {
        for _ in 0..200 {
            if server.client_count().await == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {expected} client(s)");
    }

    #[tokio::test]
    async fn broadcasts_to_a_connected_client() {
        let (server, addr) = start_server().await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        wait_for_clients(&server, 1).await;

        let payload = b"hello-sonic".to_vec();
        server.broadcast(&payload).await.unwrap();

        let message = ws.next().await.unwrap().unwrap();
        assert_eq!(message, Message::Binary(payload));
    }

    #[tokio::test]
    async fn broadcasts_to_multiple_clients() {
        let (server, addr) = start_server().await;
        let mut clients = Vec::new();
        for _ in 0..3 {
            let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
                .await
                .unwrap();
            clients.push(ws);
        }
        wait_for_clients(&server, 3).await;

        let payload = vec![1u8, 2, 3, 4];
        server.broadcast(&payload).await.unwrap();

        for ws in &mut clients {
            let message = ws.next().await.unwrap().unwrap();
            assert_eq!(message, Message::Binary(payload.clone()));
        }
    }

    #[tokio::test]
    async fn removes_disconnected_clients() {
        let (server, addr) = start_server().await;
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        wait_for_clients(&server, 1).await;

        drop(ws);
        for _ in 0..200 {
            if server.client_count().await == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("disconnected client was not removed");
    }

    #[tokio::test]
    async fn accept_with_prefix_completes_handshake_and_receives_broadcasts() {
        let server = WebSocketServer::new();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let srv = server.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            // Simulate a combined HTTP + WebSocket server: read the request
            // head, then hand the already-buffered bytes back for the upgrade.
            let mut head = Vec::new();
            let mut buf = [0u8; 512];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before sending request head");
                head.extend_from_slice(&buf[..n]);
            }
            let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let ws = WebSocketServer::accept_with_prefix(stream, head[..end].to_vec())
                .await
                .unwrap();
            srv.register(ws).await.unwrap();
        });

        // Raw WebSocket client: perform the handshake by hand.
        let mut client = TcpStream::connect(addr).await.unwrap();
        client
            .write_all(
                b"GET /ws HTTP/1.1\r\n\
                  Host: localhost\r\n\
                  Upgrade: websocket\r\n\
                  Connection: Upgrade\r\n\
                  Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
                  Sec-WebSocket-Version: 13\r\n\r\n",
            )
            .await
            .unwrap();

        let mut response = Vec::new();
        let mut buf = [0u8; 512];
        while !response.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = client.read(&mut buf).await.unwrap();
            assert!(n > 0, "server closed before sending handshake response");
            response.extend_from_slice(&buf[..n]);
        }
        assert!(
            String::from_utf8_lossy(&response).starts_with("HTTP/1.1 101"),
            "unexpected handshake response: {}",
            String::from_utf8_lossy(&response)
        );

        for _ in 0..200 {
            if server.client_count().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(server.client_count().await, 1);

        let payload = b"frame-payload".to_vec();
        server.broadcast(&payload).await.unwrap();

        // Parse one unmasked binary frame (FIN + opcode 2, then length).
        let mut header = [0u8; 2];
        client.read_exact(&mut header).await.unwrap();
        assert_eq!(header[0], 0x82);
        assert_eq!((header[1] & 0x7F) as usize, payload.len());
        let mut received = vec![0u8; payload.len()];
        client.read_exact(&mut received).await.unwrap();
        assert_eq!(received, payload);
    }
}
