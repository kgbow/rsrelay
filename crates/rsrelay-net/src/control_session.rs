//! TCP Control & Telemetry plane (`ControlServer` and `ControlClient`).
//!
//! Exchanges length-prefixed JSON `ControlMessage` frames over TCP to provide:
//! - Automatic capability & stream parameter handshake (`Hello`, `StartStream`)
//! - 4-timestamp NTP-style Round-Trip Time (RTT) measurement (`Ping` / `Pong`)
//! - Live receiver health telemetry feedback (`ReceiverTelemetry`) back to the sender

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rsrelay_proto::{
    ChannelLayout, CodecType, ControlMessage, FrameDuration, NodeCapabilities, PROTOCOL_VERSION,
};

use crate::receiver::ReceiverMetrics;

/// Write a length-prefixed `ControlMessage` to a TCP stream.
pub fn write_control_message(stream: &mut TcpStream, msg: &ControlMessage) -> std::io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = (payload.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(&payload)?;
    stream.flush()?;
    Ok(())
}

/// Read a length-prefixed `ControlMessage` from a TCP stream.
pub fn read_control_message(stream: &mut TcpStream) -> std::io::Result<ControlMessage> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 64 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "control frame exceeds 64KB limit",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    serde_json::from_slice(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn configure_control_stream(stream: &TcpStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(250)));
}

/// Receiver-side TCP control server responding to `Ping` and streaming `ReceiverTelemetry`.
pub struct ControlServer {
    stop_flag: Arc<AtomicBool>,
    local_addr: SocketAddr,
    join_handle: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn spawn(
        bind_addr: SocketAddr,
        audio_udp_port: u16,
        rx_metrics: Arc<ReceiverMetrics>,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(bind_addr)?;
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_clone = Arc::clone(&stop_flag);

        let join_handle = thread::Builder::new()
            .name("rsrelay-ctrl-srv".to_string())
            .spawn(move || {
                let start_epoch = Instant::now();
                let host = hostname::get()
                    .ok()
                    .and_then(|h| h.into_string().ok())
                    .unwrap_or_else(|| "rsrelay-receiver".to_string());

                while !stop_clone.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _peer)) => {
                            configure_control_stream(&stream);

                            let hello = ControlMessage::Hello {
                                protocol_version: PROTOCOL_VERSION,
                                node_id: format!("rx-{host}"),
                                hostname: host.clone(),
                                os: std::env::consts::OS.to_string(),
                                audio_udp_port,
                                capabilities: NodeCapabilities::default(),
                            };
                            let _ = write_control_message(&mut stream, &hello);

                            let mut last_telemetry = Instant::now() - Duration::from_secs(1);
                            let mut last_client_activity = Instant::now();

                            while !stop_clone.load(Ordering::Relaxed) {
                                // Supersede stale connection if a new client connects
                                if let Ok((new_stream, _new_peer)) = listener.accept() {
                                    configure_control_stream(&new_stream);
                                    stream = new_stream;
                                    let _ = write_control_message(&mut stream, &hello);
                                    last_client_activity = Instant::now();
                                }

                                // Drop dead half-open connections after 3s of silence
                                if last_client_activity.elapsed() > Duration::from_secs(3) {
                                    break;
                                }

                                match read_control_message(&mut stream) {
                                    Ok(ControlMessage::Ping {
                                        seq,
                                        client_send_us,
                                    }) => {
                                        last_client_activity = Instant::now();
                                        let now_us = start_epoch.elapsed().as_micros() as u64;
                                        let pong = ControlMessage::Pong {
                                            seq,
                                            client_send_us,
                                            server_recv_us: now_us,
                                            server_send_us: start_epoch.elapsed().as_micros()
                                                as u64,
                                        };
                                        if write_control_message(&mut stream, &pong).is_err() {
                                            break;
                                        }
                                    }
                                    Ok(ControlMessage::StartStream { .. })
                                    | Ok(ControlMessage::Hello { .. }) => {
                                        last_client_activity = Instant::now();
                                    }
                                    Ok(ControlMessage::StopStream { .. }) => break,
                                    Ok(_) => {}
                                    Err(ref e)
                                        if e.kind() == std::io::ErrorKind::WouldBlock
                                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                                    Err(_) => break,
                                }

                                if last_telemetry.elapsed() >= Duration::from_millis(500) {
                                    last_telemetry = Instant::now();
                                    let telem = ControlMessage::ReceiverTelemetry {
                                        rtt_us: 0,
                                        jitter_us: rx_metrics.jitter_us.load(Ordering::Relaxed),
                                        packets_received: rx_metrics
                                            .packets_received
                                            .load(Ordering::Relaxed),
                                        packets_lost: rx_metrics
                                            .packets_lost
                                            .load(Ordering::Relaxed),
                                        plc_frames: rx_metrics.plc_frames.load(Ordering::Relaxed),
                                        fec_recovered_frames: rx_metrics
                                            .fec_recovered
                                            .load(Ordering::Relaxed),
                                        buffer_level_ms: f32::from_bits(
                                            rx_metrics.buffer_level_ms_bits.load(Ordering::Relaxed),
                                        ),
                                        resample_ratio: f64::from_bits(
                                            rx_metrics.resample_ratio_bits.load(Ordering::Relaxed),
                                        ),
                                    };
                                    if write_control_message(&mut stream, &telem).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50));
                        }
                        Err(_) => {
                            thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })?;

        Ok(Self {
            stop_flag,
            local_addr,
            join_handle: Some(join_handle),
        })
    }

    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Debug, Default)]
pub struct ControlClientTelemetry {
    pub connected: AtomicBool,
    pub rtt_us: AtomicU64,
    pub remote_jitter_us: AtomicU64,
    pub remote_packets_lost: AtomicU64,
    pub remote_buffer_ms_bits: AtomicU32,
    pub remote_resample_ratio_bits: AtomicU64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ControlTelemetrySnapshot {
    pub connected: bool,
    pub rtt_us: u64,
    pub remote_jitter_us: u64,
    pub remote_packets_lost: u64,
    pub remote_buffer_ms: f32,
    pub remote_resample_ratio: f64,
}

/// Sender-side TCP control client measuring live RTT and receiving `ReceiverTelemetry`.
pub struct ControlClient {
    stop_flag: Arc<AtomicBool>,
    telemetry: Arc<ControlClientTelemetry>,
    join_handle: Option<JoinHandle<()>>,
}

impl ControlClient {
    pub fn spawn(target_control_addr: SocketAddr) -> std::io::Result<Self> {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let telemetry = Arc::new(ControlClientTelemetry::default());
        telemetry
            .remote_resample_ratio_bits
            .store(1.0f64.to_bits(), Ordering::Relaxed);

        let stop_clone = Arc::clone(&stop_flag);
        let telem_clone = Arc::clone(&telemetry);

        let join_handle = thread::Builder::new()
            .name("rsrelay-ctrl-cli".to_string())
            .spawn(move || {
                let start_epoch = Instant::now();
                let mut ping_seq = 0u32;

                while !stop_clone.load(Ordering::Relaxed) {
                    if let Ok(mut stream) =
                        TcpStream::connect_timeout(&target_control_addr, Duration::from_millis(250))
                    {
                        configure_control_stream(&stream);
                        telem_clone.connected.store(true, Ordering::Relaxed);

                        // Announce active stream parameters
                        let start_msg = ControlMessage::StartStream {
                            codec: CodecType::Opus,
                            channels: ChannelLayout::Stereo,
                            frame_duration: FrameDuration::Ms5,
                            opus_bitrate_bps: 128_000,
                            receiver_udp_port: target_control_addr.port().wrapping_add(1),
                            target_buffer_ms: 20,
                        };
                        let _ = write_control_message(&mut stream, &start_msg);

                        let mut last_ping = Instant::now() - Duration::from_secs(1);

                        while !stop_clone.load(Ordering::Relaxed) {
                            if last_ping.elapsed() >= Duration::from_millis(500) {
                                last_ping = Instant::now();
                                ping_seq = ping_seq.wrapping_add(1);
                                let ping = ControlMessage::Ping {
                                    seq: ping_seq,
                                    client_send_us: start_epoch.elapsed().as_micros() as u64,
                                };
                                if write_control_message(&mut stream, &ping).is_err() {
                                    break;
                                }
                            }

                            match read_control_message(&mut stream) {
                                Ok(ControlMessage::Pong {
                                    client_send_us,
                                    server_recv_us,
                                    server_send_us,
                                    ..
                                }) => {
                                    let now_us = start_epoch.elapsed().as_micros() as u64;
                                    let total_us = now_us.saturating_sub(client_send_us);
                                    let srv_proc_us = server_send_us.saturating_sub(server_recv_us);
                                    let rtt = total_us.saturating_sub(srv_proc_us);
                                    telem_clone.rtt_us.store(rtt, Ordering::Relaxed);
                                }
                                Ok(ControlMessage::ReceiverTelemetry {
                                    jitter_us,
                                    packets_lost,
                                    buffer_level_ms,
                                    resample_ratio,
                                    ..
                                }) => {
                                    telem_clone
                                        .remote_jitter_us
                                        .store(jitter_us, Ordering::Relaxed);
                                    telem_clone
                                        .remote_packets_lost
                                        .store(packets_lost, Ordering::Relaxed);
                                    telem_clone
                                        .remote_buffer_ms_bits
                                        .store(buffer_level_ms.to_bits(), Ordering::Relaxed);
                                    telem_clone
                                        .remote_resample_ratio_bits
                                        .store(resample_ratio.to_bits(), Ordering::Relaxed);
                                }
                                Ok(_) => {}
                                Err(ref e)
                                    if e.kind() == std::io::ErrorKind::WouldBlock
                                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                                Err(_) => break,
                            }
                        }

                        if stop_clone.load(Ordering::Relaxed) {
                            let stop_msg = ControlMessage::StopStream {
                                reason: "sender session stopped".to_string(),
                            };
                            let _ = write_control_message(&mut stream, &stop_msg);
                        }
                        telem_clone.connected.store(false, Ordering::Relaxed);
                    }

                    for _ in 0..6 {
                        if stop_clone.load(Ordering::Relaxed) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            })?;

        Ok(Self {
            stop_flag,
            telemetry,
            join_handle: Some(join_handle),
        })
    }

    pub fn snapshot(&self) -> ControlTelemetrySnapshot {
        ControlTelemetrySnapshot {
            connected: self.telemetry.connected.load(Ordering::Relaxed),
            rtt_us: self.telemetry.rtt_us.load(Ordering::Relaxed),
            remote_jitter_us: self.telemetry.remote_jitter_us.load(Ordering::Relaxed),
            remote_packets_lost: self.telemetry.remote_packets_lost.load(Ordering::Relaxed),
            remote_buffer_ms: f32::from_bits(
                self.telemetry.remote_buffer_ms_bits.load(Ordering::Relaxed),
            ),
            remote_resample_ratio: f64::from_bits(
                self.telemetry
                    .remote_resample_ratio_bits
                    .load(Ordering::Relaxed),
            ),
        }
    }
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_server_client_rtt_and_telemetry() {
        let rx_metrics = Arc::new(ReceiverMetrics::default());
        rx_metrics.packets_lost.store(3, Ordering::Relaxed);
        rx_metrics
            .buffer_level_ms_bits
            .store(19.5f32.to_bits(), Ordering::Relaxed);
        rx_metrics
            .resample_ratio_bits
            .store(1.00012f64.to_bits(), Ordering::Relaxed);

        let srv = ControlServer::spawn("127.0.0.1:0".parse().unwrap(), 49153, rx_metrics)
            .expect("spawn control server");
        let srv_addr = srv.local_addr();

        let cli = ControlClient::spawn(srv_addr).expect("spawn control client");

        // Wait up to 800ms for handshake, Ping/Pong RTT, and ReceiverTelemetry
        for _ in 0..16 {
            thread::sleep(Duration::from_millis(50));
            let snap = cli.snapshot();
            if snap.connected && snap.remote_buffer_ms > 0.0 {
                break;
            }
        }

        let snap = cli.snapshot();
        assert!(snap.connected, "control client should connect");
        assert_eq!(snap.remote_packets_lost, 3);
        assert!((snap.remote_buffer_ms - 19.5).abs() < 0.01);
        assert!((snap.remote_resample_ratio - 1.00012).abs() < 1e-6);
    }
}
