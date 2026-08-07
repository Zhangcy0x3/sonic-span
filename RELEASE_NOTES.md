# SonicSpan v1.0.0

Cross-platform, low-latency audio streaming from a desktop PC to any device
with a browser — or to another desktop. Phase 4 (mDNS discovery, system-tray
GUI, bidirectional audio) is intentionally out of scope; this is the final
feature-complete release.

## What's included

- **desktop-node** — Linux x86_64 prebuilt binary with `transmit`, `receive`,
  `list-devices`, and `serve` commands
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

## Quick start

```bash
# Linux: stream to any browser
./desktop-node serve
# open http://<pc-ip>:8080 from any device

# Desktop to desktop, compressed
./desktop-node transmit --codec opus --target <ip>
./desktop-node receive
```

Windows and macOS binaries are not prebuilt — build from source per the
instructions in `README.md`.
