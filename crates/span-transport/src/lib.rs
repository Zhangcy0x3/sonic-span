//! Network delivery for SonicSpan.
//!
//! [`udp::UdpTransport`] is a rudimentary, low-latency UDP transport that
//! implements the [`NetworkTransport`] trait. [`protocol`] defines the wire
//! format for uncompressed PCM datagrams.
//!
//! [`NetworkTransport`]: span_core::traits::NetworkTransport

pub mod protocol;
pub mod udp;
