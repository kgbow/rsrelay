# `rsrelay` — Cross-Platform Ultra-Low-Latency Audio Relay in Rust

`rsrelay` is a high-performance, ultra-low-memory replacement for AudioRelay built in pure Rust for **Windows 10/11** and **macOS 13+**.

Unlike JVM-based alternatives that consume 300MB–600MB RAM and suffer from GC-induced audio glitches, `rsrelay` uses a **strictly wait-free real-time audio pipeline (`rtrb` SPSC ring buffers)**, **Opus Low-Delay (`2.5ms–20ms`) with In-Band FEC & PLC**, and a **PI-controlled dynamic asynchronous resampler (`rubato`)** to compensate for hardware crystal clock drift over multi-hour sessions.

---

## Key Features

- **Tiny Memory Footprint**: `< 8 MB` RSS in CLI / background tray mode; zero heap allocations on real-time audio threads.
- **Ultra-Low End-to-End Latency**: `15ms–30ms` over 5GHz Wi-Fi or Ethernet (`5ms` frames + DSCP `EF` Wi-Fi WMM Voice QoS prioritization).
- **Dual-Platform System Audio Loopback**:
  - **Windows**: Native WASAPI Loopback (`AUDCLNT_STREAMFLAGS_LOOPBACK`) + automatic silent stream keepalive & `FLAG_SILENCE` zero-payload heartbeats.
  - **macOS**: Native `ScreenCaptureKit` planar-to-interleaved helper + CoreAudio virtual loopback (`BlackHole` / `Loopback`) auto-detection.
- **Anti-Jitter & Clock Drift Compensation**:
  - Sequence-indexed ring `JitterBuffer` with RFC 1982 wrap-around handling, Opus LBRR FEC lookahead, and PLC fallback.
  - Proportional-Integral (PI) watermark controller driving `rubato::FastFixedIn` within `[0.9980x, 1.0020x]` so buffer watermarks never drift or overflow.
- **Zero-Configuration LAN Discovery**: Built-in mDNS-SD (`_rsrelay._udp.local.`) advertisement and peer scanning.

---

## Workspace Structure

```text
rsrelay/
├── crates/
│   ├── rsrelay-proto/   # 12-byte RTP-lite UDP wire header & TCP/mDNS control protocol
│   ├── rsrelay-dsp/     # Opus/PCM codecs, Adaptive JitterBuffer, PI ClockDriftController, rubato ASRC
│   ├── rsrelay-audio/   # Lock-free cpal capture/playback, Windows WASAPI Loopback, macOS SCK helpers
│   ├── rsrelay-net/     # Real-time UDP TX/RX workers (DSCP EF QoS) & mDNS discovery service
│   ├── rsrelay-core/    # High-level SenderSession & ReceiverSession engine API
│   ├── rsrelay-cli/     # Full-featured CLI (`devices`, `discover`, `send`, `recv`, `self-test`)
│   └── rsrelay-gui/     # Desktop GUI & System Tray reactive ViewModel
└── docs/
    └── ARCHITECTURE.md  # Detailed engineering architecture & mathematical specification
```

---

## Quick Start

### 1. Launch Native Desktop GUI & System Tray
```bash
cargo run --release -p rsrelay-gui
```

### 2. List Local Audio Devices (CLI)
```bash
cargo run --release -p rsrelay-cli -- devices
```

### 2. Run Localhost End-to-End Pipeline Self-Test
```bash
cargo run --release -p rsrelay-cli -- self-test --duration-secs 5 --profile balanced
```

### 3. Start Receiver (Speaker Output + mDNS Broadcast)
```bash
cargo run --release -p rsrelay-cli -- recv --bind 0.0.0.0:49153 --buffer-ms 20
```

### 4. Discover Receivers on LAN
```bash
cargo run --release -p rsrelay-cli -- discover
```

### 5. Start Sender (System Audio Loopback -> Receiver IP)
```bash
# Stream system loopback using Balanced profile (5ms Opus + FEC)
cargo run --release -p rsrelay-cli -- send --target 192.168.1.100:49153 --source loopback --profile balanced

# Stream uncompressed lossless 16-bit 48kHz Stereo PCM over 5GHz Wi-Fi / Ethernet
cargo run --release -p rsrelay-cli -- send --target 192.168.1.100:49153 --source loopback --profile lossless
```
