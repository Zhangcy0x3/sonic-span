//! Network delivery for SonicSpan.
//!
//! [`udp::UdpTransport`] is a rudimentary, low-latency UDP transport that
//! implements the [`NetworkTransport`] trait. [`websocket::WebSocketServer`]
//! broadcasts the same PCM packets to browser clients that cannot use raw
//! UDP. [`protocol`] defines the wire format for uncompressed PCM datagrams.
//!
//! [`NetworkTransport`]: span_core::traits::NetworkTransport

pub mod protocol;
#[cfg(feature = "udp")]
pub mod udp;
#[cfg(feature = "websocket")]
pub mod websocket;
