# SonicSpan v1.1.0

Cross-platform, low-latency audio streaming from a desktop PC to any device
with a browser — or to another desktop. Phase 4 (mDNS discovery, system-tray
GUI, bidirectional audio) is intentionally out of scope; this is the final
feature-complete release.

## What's included

- **desktop-node** — Linux x86_64 and Windows x86_64 prebuilt binaries with
  `transmit`, `receive`, `list-devices`, and `serve` commands
- **Android app** — APK that receives desktop audio or transmits the phone's
  microphone to a desktop `receive` node
- **web client** — ready-to-serve browser receiver (served automatically by
  `desktop-node serve`, or by any static file server)

## Highlights

- **Phase 1** — system audio loopback capture (cpal), lock-free ring buffer,
  UDP transport, desktop CLI
- **Phase 2** — wasm-bindgen browser receiver with AudioWorklet playback,
  WebSocket broadcast transport, one-command `serve`
- **Phase 3** — bundled Opus codec (no system library needed), dynamic jitter
  buffer (reordering, loss detection, adaptive latency), clock-drift
  compensation via dynamic resampling, protocol v2 with codec byte
- **Android client** — native receiver (AudioTrack playback) and transmitter
  (microphone capture through AudioRecord, Opus-compressed UDP)

## Quick start

```bash
# Linux: stream to any browser
./desktop-node serve
# open http://<pc-ip>:8080 from any device

# Desktop to desktop, compressed
./desktop-node transmit --codec opus --target <ip>
./desktop-node receive
```

## Packages

- `sonicspan-desktop-node-v1.1.0-x86_64-unknown-linux-gnu.tar.gz`
- `sonicspan-desktop-node-v1.1.0-windows-x86_64.zip`
- `sonicspan-android-v1.1.0.apk` (unsigned — enable "install unknown apps")
- `sonicspan-web-client-v1.1.0.zip`

macOS binaries are not prebuilt — build from source per the instructions in
`README.md`.
