use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use span_capture::device::{list_input_devices, list_output_devices};
use span_capture::sink::CpalSink;
use span_capture::source::{CpalLoopbackSource, DEFAULT_BUFFER_CAPACITY_FRAMES};
use span_core::traits::{AudioSink, AudioSource, NetworkTransport};
use span_transport::protocol::{PcmPacket, HEADER_SIZE, MAX_PAYLOAD_BYTES};
use span_transport::udp::UdpTransport;
use span_transport::websocket::WebSocketServer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
        /// Transport to stream over: `udp` for native receivers, `websocket`
        /// for browser clients
        #[arg(long, value_enum, default_value_t = Transport::Udp)]
        transport: Transport,
        /// Bind address for the WebSocket listener
        #[arg(long, default_value = "0.0.0.0:9000")]
        bind: String,
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
    /// Serve the browser client and stream captured audio to it over WebSockets
    Serve {
        /// Local address to serve HTTP + WebSockets on
        #[arg(long, default_value = "0.0.0.0:8080")]
        bind: String,
        /// Web root served to browsers; must contain `web/pkg` after a
        /// `wasm-pack build --target web --out-dir web/pkg`
        #[arg(long, default_value = "targets/wasm-client/web")]
        web_root: PathBuf,
        /// Capture device index from `list-devices`
        #[arg(long)]
        device: Option<usize>,
        /// Audio frames per WebSocket message (default 256)
        #[arg(long, default_value_t = 256)]
        packet_frames: usize,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Transport {
    Udp,
    WebSocket,
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
            transport,
            bind,
        } => run_transmit(&target, device, packet_frames, transport, &bind).await,
        Command::Receive { bind, device } => run_receive(&bind, device).await,
        Command::Serve {
            bind,
            web_root,
            device,
            packet_frames,
        } => run_serve(&bind, &web_root, device, packet_frames).await,
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

async fn run_transmit(
    target: &str,
    device: Option<usize>,
    packet_frames: usize,
    transport: Transport,
    bind: &str,
) -> Result<()> {
    match transport {
        Transport::Udp => {
            let peer = resolve_target(target).await?;
            let transport = UdpTransport::connect(peer).await?;
            println!("Streaming uncompressed PCM over UDP to {peer} (Ctrl+C to stop)");
            run_capture_loop(device, packet_frames, StreamSink::Udp(transport)).await
        }
        Transport::WebSocket => {
            let addr: SocketAddr = bind
                .parse()
                .context("invalid --bind address; expected IP:port (e.g. 0.0.0.0:9000)")?;
            let (server, local) = WebSocketServer::bind(addr).await?;
            println!(
                "WebSocket broadcast listening on ws://{local} — waiting for browser clients (Ctrl+C to stop)"
            );
            let srv = server.clone();
            tokio::spawn(async move {
                if let Err(e) = srv.accept_loop().await {
                    eprintln!("[desktop-node] websocket accept loop failed: {e}");
                }
            });
            run_capture_loop(device, packet_frames, StreamSink::WebSocket(server)).await
        }
    }
}

/// A destination for captured PCM packets: a connected UDP socket or a
/// WebSocket broadcast hub.
enum StreamSink {
    Udp(UdpTransport),
    WebSocket(WebSocketServer),
}

impl StreamSink {
    async fn send(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Udp(transport) => transport.send_packet(bytes).await.map_err(|e| anyhow!(e)),
            Self::WebSocket(server) => server.broadcast(bytes).await.map_err(|e| anyhow!(e)),
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Udp(_) => "UDP",
            Self::WebSocket(_) => "WebSocket",
        }
    }
}

async fn run_capture_loop(
    device: Option<usize>,
    packet_frames: usize,
    mut sink: StreamSink,
) -> Result<()> {
    let payload_bytes = packet_frames * 4;
    if payload_bytes > MAX_PAYLOAD_BYTES {
        bail!(
            "--packet-frames {packet_frames} would produce a {payload_bytes}-byte payload; \
             keep it at or below {}",
            MAX_PAYLOAD_BYTES / 4
        );
    }

    let mut source =
        CpalLoopbackSource::new(device, DEFAULT_BUFFER_CAPACITY_FRAMES).map_err(|e| anyhow!(e))?;
    source.start().context("failed to start audio capture")?;
    let cfg = source.config();

    println!(
        "Capturing {} Hz / {} ch from '{}'",
        cfg.sample_rate,
        cfg.channels,
        source.device_name()
    );
    println!(
        "Streaming uncompressed PCM over {} ({packet_frames} frames per packet) — Ctrl+C to stop",
        sink.label()
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
        sink.send(&bytes).await?;
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
                old.stop()
                    .context("failed to stop previous output stream")?;
            }
            let mut new_sink = CpalSink::new(device, packet.sample_rate, packet.channels)
                .map_err(|e| anyhow!(e))?;
            new_sink.start().context("failed to start audio output")?;
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

/// Serve the static browser client and stream captured audio to it over
/// WebSockets, so a tablet/phone can play desktop audio with no native app.
async fn run_serve(
    bind: &str,
    web_root: &Path,
    device: Option<usize>,
    packet_frames: usize,
) -> Result<()> {
    let root = std::fs::canonicalize(web_root)
        .with_context(|| format!("web root '{}' does not exist", web_root.display()))?;
    if !root.is_dir() {
        bail!("web root '{}' is not a directory", root.display());
    }

    let addr: SocketAddr = bind
        .parse()
        .context("invalid --bind address; expected IP:port (e.g. 0.0.0.0:8080)")?;
    let listener = TcpListener::bind(addr)
        .await
        .context("failed to bind serve address")?;
    let local = listener
        .local_addr()
        .context("failed to read local address")?;

    let ws_server = WebSocketServer::new();
    let srv = ws_server.clone();
    let serve_root = root.clone();
    tokio::spawn(async move {
        if let Err(e) = accept_connections(listener, serve_root, srv).await {
            eprintln!("[desktop-node] http/websocket accept loop failed: {e:#}");
        }
    });

    println!("Serving web client from {}", root.display());
    println!("Open http://{local} in a browser on this machine or your tablet/phone");
    run_capture_loop(device, packet_frames, StreamSink::WebSocket(ws_server)).await
}

const MAX_REQUEST_BYTES: usize = 16 * 1024;
const HEAD_DELIMITER: &[u8] = b"\r\n\r\n";

/// Accept HTTP and WebSocket connections and dispatch each to its own task.
async fn accept_connections(
    listener: TcpListener,
    root: PathBuf,
    ws_server: WebSocketServer,
) -> Result<()> {
    loop {
        let (stream, _peer) = listener.accept().await?;
        let root = root.clone();
        let srv = ws_server.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, &root, &srv).await {
                eprintln!("[desktop-node] connection error: {e:#}");
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    root: &Path,
    ws_server: &WebSocketServer,
) -> Result<()> {
    let mut head = Vec::with_capacity(1024);
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > MAX_REQUEST_BYTES {
            return Ok(());
        }
        if head
            .windows(HEAD_DELIMITER.len())
            .any(|w| w == HEAD_DELIMITER)
        {
            break;
        }
    }

    let head_end = head
        .windows(HEAD_DELIMITER.len())
        .position(|w| w == HEAD_DELIMITER)
        .unwrap()
        + HEAD_DELIMITER.len();
    let request = String::from_utf8_lossy(&head[..head_end]).to_string();

    if is_websocket_upgrade(&request) {
        let ws = WebSocketServer::accept_with_prefix(stream, head[..head_end].to_vec()).await?;
        ws_server.register(ws).await?;
        return Ok(());
    }

    serve_static(&mut stream, &request, root).await
}

fn is_websocket_upgrade(request: &str) -> bool {
    let lower = request.to_ascii_lowercase();
    lower.contains("upgrade: websocket") && lower.contains("sec-websocket-key:")
}

/// Serve one static file for a GET/HEAD request, resolving paths safely
/// against the web root.
async fn serve_static(stream: &mut TcpStream, request: &str, root: &Path) -> Result<()> {
    let request_line = request.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let head_only = method == "HEAD";
    if method != "GET" && !head_only {
        return write_response(
            stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            false,
        )
        .await;
    }

    let path = target.split('?').next().unwrap_or("/");
    let file = resolve_web_path(root, path);
    let Some(file) = file else {
        return write_response(
            stream,
            "404 Not Found",
            "text/plain",
            b"404 Not Found",
            head_only,
        )
        .await;
    };
    let bytes = match tokio::fs::read(&file).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return write_response(
                stream,
                "404 Not Found",
                "text/plain",
                b"404 Not Found",
                head_only,
            )
            .await
        }
    };

    write_response(stream, "200 OK", content_type_for(&file), &bytes, head_only).await
}

/// Map a URL path to a file inside `root`, rejecting path traversal.
fn resolve_web_path(root: &Path, raw_path: &str) -> Option<PathBuf> {
    let mut relative = PathBuf::new();
    for component in raw_path.split('/') {
        match component {
            "" | "." => continue,
            ".." => return None,
            comp => {
                let decoded = percent_decode(comp);
                if decoded.contains(['/', '\\']) || decoded == ".." || decoded == "." {
                    return None;
                }
                relative.push(decoded);
            }
        }
    }
    let mut candidate = root.join(relative);
    if candidate.is_dir() {
        candidate = candidate.join("index.html");
    }
    candidate.is_file().then_some(candidate)
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn content_type_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

async fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<()> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if !head_only {
        response.extend_from_slice(body);
    }
    stream.write_all(&response).await?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_web_root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("sonicspan-test-{}", std::process::id()));
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("web/index.html"), "hi").unwrap();
        root
    }

    #[test]
    fn percent_decode_decodes_escapes() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("100%25"), "100%");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[test]
    fn web_path_rejects_traversal() {
        let root = temp_web_root();
        assert!(resolve_web_path(&root, "/../etc/passwd").is_none());
        assert!(resolve_web_path(&root, "/a/../../b").is_none());
        assert!(resolve_web_path(&root, "/..%2f..").is_none());
        assert!(resolve_web_path(&root, "/%2e%2e/x").is_none());
        assert_eq!(
            resolve_web_path(&root, "/web/index.html"),
            Some(root.join("web/index.html"))
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn detects_websocket_upgrade_requests() {
        assert!(is_websocket_upgrade(
            "GET /ws HTTP/1.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: abc\r\n\r\n"
        ));
        assert!(!is_websocket_upgrade(
            "GET /index.html HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
    }

    #[test]
    fn maps_content_types() {
        assert!(content_type_for(Path::new("/x/index.html")).starts_with("text/html"));
        assert_eq!(
            content_type_for(Path::new("/x/client_bg.wasm")),
            "application/wasm"
        );
        assert_eq!(
            content_type_for(Path::new("/x/app.js")),
            "text/javascript; charset=utf-8"
        );
    }
}
