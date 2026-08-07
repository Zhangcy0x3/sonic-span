use std::net::SocketAddr;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use span_capture::device::{list_input_devices, list_output_devices};
use span_capture::sink::CpalSink;
use span_capture::source::{CpalLoopbackSource, DEFAULT_BUFFER_CAPACITY_FRAMES};
use span_core::traits::{AudioSink, AudioSource, NetworkTransport};
use span_transport::protocol::{PcmPacket, HEADER_SIZE, MAX_PAYLOAD_BYTES};
use span_transport::udp::UdpTransport;
use tokio::time::sleep;

#[derive(Parser)]
#[command(
    name = "desktop-node",
    version,
    about = "SonicSpan desktop node: capture system audio and stream it over UDP",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List available audio capture and playback devices
    ListDevices,
    /// Capture system audio and stream it as UDP PCM packets
    Transmit {
        /// Destination address: IP or hostname, optionally with port (default 9000)
        #[arg(long, default_value = "127.0.0.1")]
        target: String,
        /// Capture device index from `list-devices`
        #[arg(long)]
        device: Option<usize>,
        /// Audio frames per UDP datagram (default 256)
        #[arg(long, default_value_t = 256)]
        packet_frames: usize,
    },
    /// Receive UDP PCM packets and play them through an output device
    Receive {
        /// Local address to bind the UDP socket to
        #[arg(long, default_value = "0.0.0.0:9000")]
        bind: String,
        /// Output device index from `list-devices`
        #[arg(long)]
        device: Option<usize>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::ListDevices => list_devices(),
        Command::Transmit {
            target,
            device,
            packet_frames,
        } => run_transmit(&target, device, packet_frames).await,
        Command::Receive { bind, device } => run_receive(&bind, device).await,
    }
}

fn list_devices() -> Result<()> {
    println!("Audio capture (input) devices:");
    let inputs = list_input_devices()?;
    if inputs.is_empty() {
        println!(
            "  (none found — on Windows use a loopback-capable device such as \"Stereo Mix\"; \
             on macOS install BlackHole; on Linux use a PulseAudio \"Monitor of ...\" device)"
        );
    }
    for d in &inputs {
        println!(
            "  [{}] {}{} — {} Hz, {} ch, {}",
            d.index,
            d.name,
            if d.is_default { " (default)" } else { "" },
            d.sample_rate,
            d.channels,
            d.sample_format
        );
    }

    println!("\nAudio playback (output) devices:");
    let outputs = list_output_devices()?;
    if outputs.is_empty() {
        println!("  (none found)");
    }
    for d in &outputs {
        println!(
            "  [{}] {}{} — {} Hz, {} ch, {}",
            d.index,
            d.name,
            if d.is_default { " (default)" } else { "" },
            d.sample_rate,
            d.channels,
            d.sample_format
        );
    }
    Ok(())
}

async fn run_transmit(target: &str, device: Option<usize>, packet_frames: usize) -> Result<()> {
    let payload_bytes = packet_frames * 4;
    if payload_bytes > MAX_PAYLOAD_BYTES {
        bail!(
            "--packet-frames {packet_frames} would produce a {payload_bytes}-byte payload; \
             keep it at or below {}",
            MAX_PAYLOAD_BYTES / 4
        );
    }
    let peer = resolve_target(target).await?;

    let mut source = CpalLoopbackSource::new(device, DEFAULT_BUFFER_CAPACITY_FRAMES)
        .map_err(|e| anyhow!(e))?;
    source.start().context("failed to start audio capture")?;
    let cfg = source.config();
    let mut transport = UdpTransport::connect(peer).await?;

    println!(
        "Capturing {} Hz / {} ch from '{}'",
        cfg.sample_rate,
        cfg.channels,
        source.device_name()
    );
    println!(
        "Streaming uncompressed PCM over UDP to {peer} ({packet_frames} frames per packet) — Ctrl+C to stop"
    );

    let mut chunk = vec![0.0f32; packet_frames];
    let mut sequence = 0u32;
    let mut sent_packets = 0u64;
    let mut sent_bytes = 0usize;
    let start = std::time::Instant::now();

    loop {
        let n = source.read_frames(&mut chunk).map_err(|e| anyhow!(e))?;
        if n == 0 {
            sleep(Duration::from_millis(2)).await;
            continue;
        }

        let packet = PcmPacket {
            sequence,
            sample_rate: cfg.sample_rate,
            channels: cfg.channels,
            samples: chunk[..n].to_vec(),
        };
        sequence = sequence.wrapping_add(1);

        let mut bytes = Vec::with_capacity(HEADER_SIZE + n * 4);
        packet.encode(&mut bytes);
        transport.send_packet(&bytes).await?;
        sent_packets += 1;
        sent_bytes += bytes.len();

        if sent_packets.is_multiple_of(500) {
            let kbps = sent_bytes as f64 * 8.0 / 1000.0 / start.elapsed().as_secs_f64();
            println!("[{sent_packets} packets] {kbps:.0} kbps, {sent_bytes} bytes sent");
        }
    }
}

async fn run_receive(bind: &str, device: Option<usize>) -> Result<()> {
    let addr: SocketAddr = bind
        .parse()
        .context("invalid --bind address; expected IP:port (e.g. 0.0.0.0:9000)")?;
    let mut transport = UdpTransport::bind(addr).await?;
    println!(
        "Listening for UDP PCM on {} — Ctrl+C to stop",
        transport.local_addr()?
    );

    let mut sink: Option<CpalSink> = None;
    let mut sink_config: Option<(u32, u16)> = None;
    let mut received_packets = 0u64;

    loop {
        let data = transport.receive_packet().await?;
        let packet = PcmPacket::decode(&data).map_err(|e| anyhow!(e))?;
        received_packets += 1;

        // (Re)create the output stream if the sender's configuration changes.
        let cfg = (packet.sample_rate, packet.channels);
        if sink.is_none() || sink_config != Some(cfg) {
            if let Some(mut old) = sink.take() {
                old.stop().context("failed to stop previous output stream")?;
            }
            let mut new_sink = CpalSink::new(device, packet.sample_rate, packet.channels)
                .map_err(|e| anyhow!(e))?;
            new_sink
                .start()
                .context("failed to start audio output")?;
            println!(
                "Receiving {} Hz / {} ch on '{}'",
                new_sink.config().sample_rate,
                new_sink.config().channels,
                new_sink.device_name()
            );
            sink = Some(new_sink);
            sink_config = Some(cfg);
        }

        if let Some(sink) = sink.as_mut() {
            sink.write_frames(&packet.samples).map_err(|e| anyhow!(e))?;
        }

        if received_packets.is_multiple_of(1000) {
            println!(
                "[{received_packets} packets] rx {} bytes",
                transport.stats().bytes_received()
            );
        }
    }
}

/// Resolve a `host` or `host:port` target to a socket address (default port 9000).
async fn resolve_target(target: &str) -> Result<SocketAddr> {
    if let Ok(addr) = target.parse::<SocketAddr>() {
        return Ok(addr);
    }
    let query: String = if target.contains(':') {
        target.to_string()
    } else {
        format!("{target}:9000")
    };
    let mut candidates = tokio::net::lookup_host(query.as_str())
        .await
        .with_context(|| format!("failed to resolve target '{target}'"))?;

    candidates
        .next()
        .ok_or_else(|| anyhow!("could not resolve target '{target}'"))
}
