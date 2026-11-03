# `rsrelay` Engineering Architecture & Technical Specification

## 1. Real-Time Thread Isolation Model

Real-time audio callbacks (WASAPI on Windows, CoreAudio / ScreenCaptureKit on macOS) execute under strict sub-millisecond deadlines. Any mutex contention, heap allocation (`malloc`/`free`), or blocking system call (`sendto`/`recvfrom`) inside the OS audio callback causes buffer underruns (clicks/pops).

`rsrelay` enforces a strict 3-stage wait-free boundary:

1. **OS Audio Hardware Callback Thread**:
   - Reads/writes interleaved `f32` frames directly to/from a pre-allocated Single-Producer Single-Consumer (`rtrb::RingBuffer<f32>`) using `write_chunk` / `read_chunk`.
   - Zero locks, zero allocations, zero syscalls.
2. **Real-Time DSP & Packetizer Worker Thread (`rsrelay-tx-worker` / `rsrelay-rx-worker`)**:
   - Elevated to OS real-time scheduling priority via `audio_thread_priority` (Windows MMCSS `"Pro Audio"`, macOS Mach `THREAD_TIME_CONSTRAINT_POLICY`).
   - Performs sample-rate normalization (`44.1kHz -> 48kHz`), Opus Low-Delay encoding/decoding, LBRR FEC & PLC loss concealment, and PI-controlled dynamic resampling (`rubato::FastFixedIn`).
3. **Control & Discovery Plane (`mdns-sd` / TCP)**:
   - Completely decoupled from the UDP audio hot path.

---

## 2. 12-Byte UDP Wire Protocol (`rsrelay-proto`)

All multi-byte integers are encoded in Network Byte Order (Big-Endian):

| Byte Offset | Field | Type | Description |
| :--- | :--- | :--- | :--- |
| `0..2` | `magic` | `u16` | Fixed `0x5253` (`"RS"`) |
| `2..4` | `seq` | `u16` | Wrapping packet sequence number (compared via RFC 1982 signed `i16` diff) |
| `4..8` | `timestamp` | `u32` | Monotonic sample counter at `48,000 Hz` |
| `8..10` | `format_flags` | `u16` | Bitfield: Codec (`0..2`), Channels (`2..4`), FrameDuration (`4..7`), `FLAG_SILENCE` (`7`), `FLAG_FEC` (`8`), `FLAG_RESET` (`9`), Version (`10..13`) |
| `10..12` | `payload_len` | `u16` | Payload length in bytes (`0..=1460`) |

---

## 3. Adaptive Clock-Drift Compensation (`rsrelay-dsp::drift`)

Because the sender soundcard crystal oscillator ($48000 + \delta_1\text{ Hz}$) and receiver soundcard crystal oscillator ($48000 + \delta_2\text{ Hz}$) inevitably differ by $\pm 20\text{ ppm} \sim \pm 150\text{ ppm}$, `rsrelay` implements a Proportional-Integral (PI) controller over an Exponential Moving Average (EMA) of the total receiver buffer occupancy $L_k$:

$$\bar{L}_k = \alpha L_k + (1 - \alpha)\bar{L}_{k-1}, \quad \alpha = 0.005$$

$$e_k = \frac{\bar{L}_k - L_{\text{target}}}{L_{\text{target}}}$$

$$r_k = \text{clamp}\left(1.0 - K_p e_k - \sum K_i e_j, \; 0.9980, \; 1.0020\right)$$

The ratio $r_k$ dynamically modulates `rubato::FastFixedIn::set_resample_ratio_relative(r_k, true)` with cubic polynomial interpolation, maintaining constant buffer latency indefinitely without audible pitch shift.
