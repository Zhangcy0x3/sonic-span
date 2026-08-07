//! Rudimentary UDP transport implementing the [`NetworkTransport`] trait.
//!
//! [`NetworkTransport`]: span_core::traits::NetworkTransport

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use span_core::errors::NetworkError;
use span_core::traits::{NetworkStats, NetworkTransport};
use tokio::net::UdpSocket;

use crate::protocol::MAX_PAYLOAD_BYTES;

/// Per-connection counters exposed for diagnostics.
#[derive(Debug, Default)]
pub struct UdpStats {
    packets_sent: AtomicU64,
    packets_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
}

impl UdpStats {
    pub fn packets_sent(&self) -> u64 {
        self.packets_sent.load(Ordering::Relaxed)
    }

    pub fn packets_received(&self) -> u64 {
        self.packets_received.load(Ordering::Relaxed)
    }

    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }

    pub fn bytes_received(&self) -> u64 {
        self.bytes_received.load(Ordering::Relaxed)
    }
}

/// A minimal UDP socket wrapper.
///
/// Create it with [`UdpTransport::connect`] on the transmitting side (binds an
/// ephemeral port and remembers the peer) or [`UdpTransport::bind`] on the
/// receiving side.
pub struct UdpTransport {
    socket: UdpSocket,
    peer: Option<SocketAddr>,
    stats: UdpStats,
}

impl UdpTransport {
    /// Bind to `addr` and wait for datagrams (receiver side).
    pub async fn bind(addr: SocketAddr) -> Result<Self, NetworkError> {
        let socket = UdpSocket::bind(addr)
            .await
            .map_err(|e| NetworkError::ReceiveError(e.to_string()))?;
        Ok(Self {
            socket,
            peer: None,
            stats: UdpStats::default(),
        })
    }

    /// Bind an ephemeral port and connect to `peer` (transmitter side).
    pub async fn connect(peer: SocketAddr) -> Result<Self, NetworkError> {
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .map_err(|e| NetworkError::SendError(e.to_string()))?;
        socket
            .connect(peer)
            .await
            .map_err(|e| NetworkError::SendError(e.to_string()))?;
        Ok(Self {
            socket,
            peer: Some(peer),
            stats: UdpStats::default(),
        })
    }

    /// The local address this transport is bound to.
    pub fn local_addr(&self) -> Result<SocketAddr, NetworkError> {
        self.socket
            .local_addr()
            .map_err(|e| NetworkError::StatsError(e.to_string()))
    }

    /// Current transfer counters.
    pub fn stats(&self) -> &UdpStats {
        &self.stats
    }
}

#[async_trait]
impl NetworkTransport for UdpTransport {
    async fn send_packet(&mut self, payload: &[u8]) -> Result<(), NetworkError> {
        let n = match self.peer {
            Some(peer) => self
                .socket
                .send_to(payload, peer)
                .await
                .map_err(|e| NetworkError::SendError(e.to_string()))?,
            None => self
                .socket
                .send(payload)
                .await
                .map_err(|e| NetworkError::SendError(e.to_string()))?,
        };
        self.stats.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_sent.fetch_add(n as u64, Ordering::Relaxed);
        Ok(())
    }

    async fn receive_packet(&mut self) -> Result<Vec<u8>, NetworkError> {
        let mut buf = vec![0u8; MAX_PAYLOAD_BYTES + 64];
        let (n, _src) = self
            .socket
            .recv_from(&mut buf)
            .await
            .map_err(|e| NetworkError::ReceiveError(e.to_string()))?;
        buf.truncate(n);
        self.stats.packets_received.fetch_add(1, Ordering::Relaxed);
        self.stats.bytes_received.fetch_add(n as u64, Ordering::Relaxed);
        Ok(buf)
    }

    fn get_stats(&self) -> NetworkStats {
        // Latency and loss measurement arrive with the jitter buffer in Phase 3.
        NetworkStats {
            latency_ms: 0,
            packet_loss_rate: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn udp_round_trip() {
        let mut receiver = UdpTransport::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let rx_addr = receiver.local_addr().unwrap();

        let mut sender = UdpTransport::connect(rx_addr).await.unwrap();
        let payload = vec![1u8, 2, 3, 4, 5];
        sender.send_packet(&payload).await.unwrap();

        let received = receiver.receive_packet().await.unwrap();
        assert_eq!(received, payload);
        assert_eq!(sender.stats().packets_sent(), 1);
        assert_eq!(receiver.stats().packets_received(), 1);
        assert_eq!(sender.stats().bytes_sent(), payload.len() as u64);
        assert_eq!(receiver.stats().bytes_received(), payload.len() as u64);
    }

    #[tokio::test]
    async fn multiple_packets_preserve_order() {
        let mut receiver = UdpTransport::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let rx_addr = receiver.local_addr().unwrap();
        let mut sender = UdpTransport::connect(rx_addr).await.unwrap();

        for i in 0..100u16 {
            sender.send_packet(&i.to_le_bytes()).await.unwrap();
        }
        for i in 0..100u16 {
            let data = receiver.receive_packet().await.unwrap();
            assert_eq!(u16::from_le_bytes(data.try_into().unwrap()), i);
        }
    }
}
