# SonicSpan

> Low-latency, cross-platform audio streaming from desktop to any device with a browser.

**SonicSpan** captures system audio on a PC, encodes it, and streams it over the local network to desktop and browser clients with minimal latency. The project aims to build a robust, zero-copy audio pipeline before expanding into feature-rich applications.

---

## 🏗 Architecture

```
┌─────────────────────────────────────────────┐
│           Audio Source (System)              │
└──────────────────┬──────────────────────────┘
                   │
        ┌──────────▼──────────┐
        │   span-capture      │  cpal loopback capture
        └──────────┬──────────┘
                   │
        ┌──────────▼──────────┐
        │    span-core        │  Encode / Buffer / Traits
        └──────────┬──────────┘
                   │
        ┌──────────▼──────────┐
        │  span-transport     │  UDP / WebRTC
        └──────────┬──────────┘
                   │
        ┌──────────┴──────────┐
        │                     │
    ┌───▼────┐          ┌────▼────┐
    │Desktop │          │ Browser  │
    │ Node   │          │ Client   │
    └────────┘          └──────────┘
```

---

## 📁 Project Structure

```
sonic-span/
├── Cargo.toml                  # Workspace root manifest
├── README.md
├── docs/                       # Architecture diagrams & API references
├── crates/
│   ├── span-core/              # [I/O Agnostic] Ring buffers, state machines, traits
│   │   └── src/
│   │       ├── buffer.rs       # Lock-free ring buffer & jitter buffer
│   │       ├── codec.rs        # Opus encoding/decoding abstractions
│   │       └── traits.rs       # AudioSource, AudioSink, NetworkTransport
│   ├── span-capture/           # Native audio loopback capture
│   │   └── src/lib.rs          # cpal integration → AudioSource
│   └── span-transport/         # Network delivery (Tokio-based)
│       └── src/
│           ├── udp.rs          # Low-latency UDP transport
│           └── webrtc.rs       # WebRTC data channels
├── targets/
│   ├── desktop-node/           # PC transmitter/receiver CLI
│   │   └── src/main.rs
│   └── wasm-client/            # Tablet/browser receiver
│       ├── src/lib.rs          # WASM bindings → AudioSink
│       ├── pkg/                # Compiled WASM output
│       └── web/
│           ├── index.html
│           └── app.js
└── .github/workflows/          # CI/CD (Rust + WASM builds)
```

---

## 🗺️ Roadmap

### Phase 1: Foundation & "Hello World" of Audio (MVP)
*Establish core architecture and transmit uncompressed PCM audio over a local network.*

- [√] Initialize Cargo workspace and define `AudioSource`, `AudioSink`, and `NetworkTransport` traits
- [ ] Implement a lock-free ring buffer in `span-core` for thread-safe cross-boundary data handoffs
- [ ] Implement local system audio loopback capture using `cpal` (Windows/macOS/Linux)
- [ ] Build a rudimentary UDP transport layer using asynchronous I/O (`tokio`)
- [ ] Create a basic CLI transmitter (PC) and CLI receiver (PC) to verify end-to-end transmission

### Phase 2: WebAssembly Integration & Cross-Device Playback
*Allow any device with a modern browser to act as a receiver without installing native apps.*

- [ ] Develop `wasm-client` target utilizing `wasm-bindgen`
- [ ] Bridge WASM output to the JavaScript Web Audio API (`AudioWorklet`)
- [ ] Implement WebSockets or WebRTC Data Channels in `span-transport` for browser compatibility
- [ ] Successfully stream audio from the desktop node to a tablet browser

### Phase 3: High-Fidelity & Low-Latency Optimization
*Make the stream usable for real-time media consumption (videos/gaming).*

- [ ] Integrate Opus codec into `span-core` for high-quality, low-bandwidth transmission
- [ ] Implement a dynamic Jitter Buffer to handle network packet loss and out-of-order delivery
- [ ] Develop a Clock Drift Compensation algorithm (dynamic resampling) to synchronize PC and tablet audio clocks

### Phase 4: Ecosystem, Discovery, and Polish
*Transform the library into a user-friendly application.*

- [ ] Implement mDNS (Multicast DNS) for automatic local network device discovery
- [ ] Build a lightweight system-tray GUI for the desktop node
- [ ] Support bi-directional audio routing (tablet microphone → PC input) with Acoustic Echo Cancellation (AEC)

---

## 🚀 Getting Started

### Prerequisites

- **Rust** 1.75+
- **wasm-pack** (for WASM builds)
- **cpal** system dependencies:
  - Linux: `libasound2-dev`
  - macOS: (pre-installed)
  - Windows: (pre-installed)

### Build

```bash
# Build all crates
cargo build --release

# Build WASM client
cd targets/wasm-client
wasm-pack build --target web
```

### Run

```bash
# List available audio devices
cargo run --bin desktop-node -- list-devices

# Start transmitter
cargo run --bin desktop-node -- transmit --target 192.168.1.100 --device 0

# Start receiver
cargo run --bin desktop-node -- receive --bind 0.0.0.0:9000
```

---

## 🧩 Crate Overview

| Crate | Description | Key Dependencies |
|-------|-------------|------------------|
| `span-core` | Traits, ring buffer, Opus codec abstractions | `tokio`, `opus` |
| `span-capture` | System audio loopback capture | `cpal` |
| `span-transport` | UDP + WebRTC network layer | `tokio`, `quinn` |
| `desktop-node` | CLI transmitter/receiver | `clap`, `anyhow` |
| `wasm-client` | Browser-based receiver | `wasm-bindgen`, `web-sys` |

---

## 📄 License

MIT
