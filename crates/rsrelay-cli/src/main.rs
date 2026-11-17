use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use rsrelay_core::{
    detect_local_lan_ip, list_audio_devices, select_default_virtual_mic_output_device,
    CaptureSource, ChannelLayout, DiscoveryService, DuplexRole, DuplexSession, DuplexSessionConfig,
    LatencyProfile, ReceiverSession, ReceiverSessionConfig, SenderSession, SenderSessionConfig,
    DEFAULT_AUDIO_UDP_PORT, DEFAULT_UPLINK_UDP_PORT,
};

#[derive(Parser)]
#[command(
    name = "rsrelay",
    version,
    about = "Ultra-low-latency cross-platform audio & microphone relay in pure Rust (Windows & macOS)"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, ValueEnum)]
enum SourceArg {
    /// System audio loopback (WASAPI Loopback on Windows, ScreenCaptureKit on macOS)
    Loopback,
    /// Microphone / Line-In input
    Mic,
    /// Built-in 440Hz/660Hz stereo sine test generator
    Tone,
}

impl From<SourceArg> for CaptureSource {
    fn from(val: SourceArg) -> Self {
        match val {
            SourceArg::Loopback => CaptureSource::SystemLoopback,
            SourceArg::Mic => CaptureSource::Microphone,
            SourceArg::Tone => CaptureSource::SyntheticTone,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum ProfileArg {
    /// 5ms Opus, 10ms Jitter Buffer (~15-20ms E2E)
    UltraLow,
    /// 5ms Opus + In-Band FEC, 20ms Jitter Buffer (~25-30ms E2E)
    Balanced,
    /// 10ms Opus + In-Band FEC, 40ms Jitter Buffer (~50ms E2E)
    Stable,
    /// 5ms Uncompressed 16-bit 48kHz Stereo PCM, 15ms Jitter Buffer
    Lossless,
}

impl From<ProfileArg> for LatencyProfile {
    fn from(val: ProfileArg) -> Self {
        match val {
            ProfileArg::UltraLow => LatencyProfile::UltraLow,
            ProfileArg::Balanced => LatencyProfile::Balanced,
            ProfileArg::Stable => LatencyProfile::Stable,
            ProfileArg::Lossless => LatencyProfile::LosslessPcm,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum DuplexRoleArg {
    /// Main PC: sends System Loopback audio (:49153) & receives remote Microphone into Virtual Mic (:49155)
    Host,
    /// Wireless Headset: receives System Audio (:49153) & sends local Microphone (:49155)
    Headset,
    /// Symmetric Dual-Desktop (Win + Mac): simultaneous bidirectional Speaker + Virtual Mic sharing
    Symmetric,
}

impl From<DuplexRoleArg> for DuplexRole {
    fn from(val: DuplexRoleArg) -> Self {
        match val {
            DuplexRoleArg::Host => DuplexRole::Host,
            DuplexRoleArg::Headset => DuplexRole::Headset,
            DuplexRoleArg::Symmetric => DuplexRole::Symmetric,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// List all local input, output, and virtual microphone audio devices
    Devices,

    /// Install the virtual microphone driver (`rsrelayVirtualMic.driver` on macOS, or `VB-CABLE` via winget on Windows)
    InstallDriver,

    /// Discover active rsrelay receivers on the LAN via mDNS (_rsrelay._udp.local.)
    Discover {
        /// Scan duration in milliseconds
        #[arg(long, default_value_t = 2000)]
        timeout_ms: u64,
    },

    /// Capture system or microphone audio and stream over UDP to a target receiver
    Send {
        /// Target receiver UDP address (e.g. 192.168.1.100:49153)
        #[arg(short, long, default_value = "127.0.0.1:49153")]
        target: SocketAddr,

        /// Audio capture source
        #[arg(short, long, value_enum, default_value_t = SourceArg::Loopback)]
        source: SourceArg,

        /// Latency and codec profile
        #[arg(short, long, value_enum, default_value_t = ProfileArg::Balanced)]
        profile: ProfileArg,

        /// Optional audio device name substring filter
        #[arg(short, long)]
        device: Option<String>,

        /// Opus target bitrate in bps (used for Opus profiles)
        #[arg(long, default_value_t = 128_000)]
        bitrate: u32,

        /// Optional microphone Noise Gate threshold in dBFS (e.g. -45.0)
        #[arg(long)]
        gate_db: Option<f32>,
    },

    /// Full-Duplex / Cross-Machine Speaker & Microphone sharing (`host`, `headset`, or `symmetric`)
    Duplex {
        /// Role preset (`host`, `headset`, or `symmetric` for dual-desktop Win+Mac sharing)
        #[arg(short, long, value_enum, default_value_t = DuplexRoleArg::Host)]
        role: DuplexRoleArg,

        /// Remote peer IP address (e.g. 192.168.1.105)
        #[arg(short, long, default_value = "127.0.0.1")]
        peer: IpAddr,

        /// Latency and codec profile
        #[arg(short = 'P', long, value_enum, default_value_t = ProfileArg::Balanced)]
        profile: ProfileArg,

        /// Microphone noise gate threshold in dBFS (<= -90 disables)
        #[arg(long, default_value_t = -45.0)]
        gate_db: f32,

        /// Optional TX capture device name substring filter
        #[arg(long)]
        tx_device: Option<String>,

        /// Optional RX playback / Virtual Mic device name substring filter
        #[arg(long)]
        rx_device: Option<String>,

        /// Media / Speaker bus UDP port (default :49153)
        #[arg(long, default_value_t = DEFAULT_AUDIO_UDP_PORT)]
        downlink_port: u16,

        /// Microphone / Virtual Mic bus UDP port (default :49155)
        #[arg(long, default_value_t = DEFAULT_UPLINK_UDP_PORT)]
        uplink_port: u16,

        /// Playback volume multiplier (0.0 to 2.0)
        #[arg(short, long, default_value_t = 1.0)]
        volume: f32,
    },

    /// Receive UDP audio stream, apply adaptive jitter buffer + PI clock-drift compensation, and play out
    Recv {
        /// Local UDP bind address
        #[arg(short, long, default_value_t = SocketAddr::from(([0, 0, 0, 0], DEFAULT_AUDIO_UDP_PORT)))]
        bind: SocketAddr,

        /// Target jitter + playback buffer in milliseconds
        #[arg(long, default_value_t = 20)]
        buffer_ms: u16,

        /// Optional output audio device name substring filter
        #[arg(short, long)]
        device: Option<String>,

        /// Playback volume multiplier (0.0 to 2.0)
        #[arg(short, long, default_value_t = 1.0)]
        volume: f32,

        /// Disable mDNS LAN advertisement
        #[arg(long)]
        no_mdns: bool,
    },

    /// Run a self-contained localhost sender -> receiver E2E pipeline verification test
    SelfTest {
        /// Test duration in seconds
        #[arg(short, long, default_value_t = 3)]
        duration_secs: u64,

        /// Latency profile to test
        #[arg(short, long, value_enum, default_value_t = ProfileArg::Balanced)]
        profile: ProfileArg,

        /// Run full-duplex bidirectional (Host <-> Headset) verification
        #[arg(long)]
        duplex: bool,
    },
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Devices => {
            let lan_ip = detect_local_lan_ip()
                .map(|ip| ip.to_string())
                .unwrap_or_else(|| "127.0.0.1 (Offline)".to_string());
            println!("Local LAN IPv4 : {lan_ip}");
            #[cfg(target_os = "macos")]
            println!(
                "macOS ScreenCaptureKit System Audio Permission: {}",
                if rsrelay_core::has_system_loopback_permission() {
                    "GRANTED (Driverless 48kHz Stereo Loopback ready)"
                } else {
                    "NOT GRANTED (Will prompt on first Loopback capture)"
                }
            );
            println!(
                "Preferred Virtual Mic Sink: {}",
                select_default_virtual_mic_output_device()
                    .unwrap_or_else(|| "NONE (Run `rsrelay install-driver` to install)".to_string())
            );
            let devices = list_audio_devices()?;
            println!("=== Local Audio Devices ({}) ===", devices.len());
            for dev in devices {
                let role = if dev.is_input { "INPUT " } else { "OUTPUT" };
                let default_tag = if dev.is_default_input || dev.is_default_output {
                    " [DEFAULT]"
                } else {
                    ""
                };
                let rate = dev
                    .default_sample_rate
                    .map(|r| format!("{r} Hz"))
                    .unwrap_or_else(|| "N/A".to_string());
                let ch = dev
                    .default_channels
                    .map(|c| format!("{c} ch"))
                    .unwrap_or_else(|| "N/A".to_string());
                println!("  [{role}] {:<42} ({rate}, {ch}){default_tag}", dev.name);
            }
        }

        Commands::InstallDriver => {
            #[cfg(target_os = "macos")]
            {
                println!("Building and installing `/Library/Audio/Plug-Ins/HAL/rsrelayVirtualMic.driver`...");
                match rsrelay_core::install_macos_virtual_mic_driver() {
                    Ok(path) => {
                        println!("Successfully installed macOS Virtual Mic HAL driver at: {path}");
                        println!("Select 'rsrelay Virtual Mic' as the microphone in Zoom, Chrome, WeChat, or Discord.");
                    }
                    Err(e) => anyhow::bail!("{e}"),
                }
            }
            #[cfg(target_os = "windows")]
            {
                println!("Launching signed VB-CABLE Virtual Audio Driver installation via winget...");
                match rsrelay_core::launch_windows_vbcable_install() {
                    Ok(pkg) => println!("Launched winget installer for {pkg}."),
                    Err(e) => anyhow::bail!("{e}"),
                }
            }
            #[cfg(not(any(target_os = "macos", target_os = "windows")))]
            {
                println!("On Linux, PipeWire/PulseAudio virtual sources can be created natively via `pactl load-module module-null-sink`.");
            }
        }

        Commands::Discover { timeout_ms } => {
            println!("Scanning LAN for _rsrelay._udp.local. ({timeout_ms} ms)...");
            let svc = DiscoveryService::new()?;
            let peers = svc.scan(Duration::from_millis(timeout_ms))?;
            if peers.is_empty() {
                println!("No active rsrelay receivers discovered on the local network.");
            } else {
                println!("Discovered {} peer(s):", peers.len());
                for p in peers {
                    println!(
                        "  - {} ({}, OS: {}) -> {:?}:{}",
                        p.instance_name, p.hostname, p.os, p.addresses, p.udp_port
                    );
                }
            }
        }

        Commands::Send {
            target,
            source,
            profile,
            device,
            bitrate,
            gate_db,
        } => {
            let lat_profile: LatencyProfile = profile.into();
            let cfg = SenderSessionConfig {
                target_addr: target,
                source: source.into(),
                device_name: device,
                codec: lat_profile.default_codec(),
                channels: ChannelLayout::Stereo,
                frame_duration: lat_profile.default_frame_duration(),
                opus_bitrate_bps: bitrate,
                enable_fec: lat_profile.default_fec(),
                noise_gate_db: gate_db,
            };

            let session = SenderSession::start(cfg.clone())?;
            let init_status = session.status();
            println!(
                "Started rsrelay Sender -> {} | Device: '{}' ({} Hz) | Codec: {:?} ({:?})",
                target,
                init_status.device_name,
                init_status.sample_rate,
                cfg.codec,
                cfg.frame_duration
            );
            println!("Press Ctrl+C to stop.");

            let running = setup_ctrlc_flag();
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
                let st = session.status();
                let vu = render_vu_bar(st.peak_level, 16);
                let rtt_str = if st.control.connected {
                    format!("{:.2}ms", (st.control.rtt_us as f64) / 1000.0)
                } else {
                    "N/A".to_string()
                };
                print!(
                    "\r[TX] VU: [{vu}] {:>5.1}% | Sent: {:>8} pkts ({:>7.1} KB) | RTT: {:>7} | Silence: {:>5}   ",
                    st.peak_level * 100.0,
                    st.net.packets_sent,
                    (st.net.bytes_sent as f64) / 1024.0,
                    rtt_str,
                    st.net.silence_heartbeats_sent
                );
                let _ = std::io::stdout().flush();
            }
            println!("\nSender stopped.");
        }

        Commands::Duplex {
            role,
            peer,
            profile,
            gate_db,
            tx_device,
            rx_device,
            downlink_port,
            uplink_port,
            volume,
        } => {
            let duplex_role: DuplexRole = role.into();
            let lat_profile: LatencyProfile = profile.into();

            let mut cfg = DuplexSessionConfig::new(duplex_role, peer);
            cfg.profile = lat_profile;
            cfg.downlink_udp_port = downlink_port;
            cfg.uplink_udp_port = uplink_port;
            cfg.tx_device_name = tx_device;
            cfg.rx_device_name = rx_device;
            cfg.mic_noise_gate_db = if gate_db > -90.0 { Some(gate_db) } else { None };
            cfg.initial_volume = volume;

            let session = DuplexSession::start(cfg)?;
            let st = session.status();
            println!(
                "Started Full-Duplex [{:?}] <-> Peer {} (Media Bus :{}, Mic Bus :{})",
                duplex_role, peer, downlink_port, uplink_port
            );
            if let Some(ref mtx) = st.media_tx {
                println!("  Media TX   : '{}' ({} Hz)", mtx.device_name, mtx.sample_rate);
            }
            if let Some(ref mrx) = st.media_rx {
                println!("  Media RX   : '{}' ({} Hz)", mrx.device_name, mrx.sample_rate);
            }
            if let Some(ref vtx) = st.mic_tx {
                println!("  Mic TX     : '{}' ({} Hz)", vtx.device_name, vtx.sample_rate);
            }
            if let Some(ref vrx) = st.mic_rx {
                println!(
                    "  VirtualMic : '{}' ({} Hz){}",
                    vrx.device_name,
                    vrx.sample_rate,
                    if st.virtual_mic_ready {
                        ""
                    } else {
                        " [ECHO-FREE GUARD: Run `rsrelay install-driver` to expose OS Virtual Mic]"
                    }
                );
            }
            println!("Press Ctrl+C to stop.");

            let running = setup_ctrlc_flag();
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
                let st = session.status();
                let tx_pkts = st.primary_tx().map(|t| t.net.packets_sent).unwrap_or(0);
                let tx_dtx = st.primary_tx().map(|t| t.net.gated_dtx_frames).unwrap_or(0);
                let tx_peak = st.primary_tx().map(|t| t.peak_level).unwrap_or(0.0);
                let rx_pkts = st.primary_rx().map(|r| r.net.packets_received).unwrap_or(0);
                let rx_lost = st.primary_rx().map(|r| r.net.packets_lost).unwrap_or(0);
                let rx_buf = st.primary_rx().map(|r| r.net.buffer_level_ms).unwrap_or(0.0);
                let rx_asrc = st.primary_rx().map(|r| r.net.resample_ratio).unwrap_or(1.0);
                let rx_peak = st.primary_rx().map(|r| r.peak_level).unwrap_or(0.0);

                let tx_vu = render_vu_bar(tx_peak, 10);
                let rx_vu = render_vu_bar(rx_peak, 10);
                let rtt_str = st
                    .best_rtt_us()
                    .map(|us| format!("{:.2}ms", (us as f64) / 1000.0))
                    .unwrap_or_else(|| "N/A".to_string());

                print!(
                    "\r[DUPLEX {:?}] TX[{tx_vu}] {:>6} pkts (DTX:{:>4}) | RX[{rx_vu}] {:>6} pkts (Lost:{:>2}, Buf:{:>4.1}ms, ASRC:{:.5}x) | RTT:{:>7}   ",
                    st.role, tx_pkts, tx_dtx, rx_pkts, rx_lost, rx_buf, rx_asrc, rtt_str
                );
                let _ = std::io::stdout().flush();
            }
            println!("\nFull-Duplex session stopped.");
        }

        Commands::Recv {
            bind,
            buffer_ms,
            device,
            volume,
            no_mdns,
        } => {
            let host_label = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "rsrelay-node".to_string());

            let cfg = ReceiverSessionConfig {
                bind_addr: bind,
                device_name: device,
                channels: ChannelLayout::Stereo,
                target_buffer_ms: buffer_ms,
                initial_volume: volume,
                advertise_mdns: !no_mdns,
                node_name: format!("rsrelay-{host_label}"),
            };

            let session = ReceiverSession::start(cfg)?;
            let init_status = session.status();
            let lan_hint = detect_local_lan_ip()
                .map(|ip| format!("{ip}:{}", bind.port()))
                .unwrap_or_else(|| bind.to_string());
            println!(
                "Started rsrelay Receiver on {} (LAN Endpoint: {}) | Output: '{}' ({} Hz) | Target Buffer: {} ms",
                bind, lan_hint, init_status.device_name, init_status.sample_rate, buffer_ms
            );
            println!("Press Ctrl+C to stop.");

            let running = setup_ctrlc_flag();
            while running.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(250));
                let st = session.status();
                let vu = render_vu_bar(st.peak_level, 16);
                print!(
                    "\r[RX] VU: [{vu}] | Pkts: {:>7} (Lost: {:>3}, FEC: {:>3}, PLC: {:>3}) | Buf: {:>4.1}ms | ASRC: {:.5}x   ",
                    st.net.packets_received,
                    st.net.packets_lost,
                    st.net.fec_recovered,
                    st.net.plc_frames,
                    st.net.buffer_level_ms,
                    st.net.resample_ratio
                );
                let _ = std::io::stdout().flush();
            }
            println!("\nReceiver stopped.");
        }

        Commands::SelfTest {
            duration_secs,
            profile,
            duplex,
        } => {
            let lat_profile: LatencyProfile = profile.into();

            if duplex {
                println!(
                    "Running Full-Duplex (Host <-> Headset) localhost E2E self-test for {duration_secs}s (Profile: {lat_profile:?})..."
                );
                let lo_ip: IpAddr = "127.0.0.1".parse().unwrap();

                let mut host_cfg = DuplexSessionConfig::new(DuplexRole::Host, lo_ip);
                host_cfg.downlink_udp_port = 49353;
                host_cfg.uplink_udp_port = 49355;
                host_cfg.profile = lat_profile;
                host_cfg.initial_volume = 0.0;
                host_cfg.advertise_mdns = false;
                host_cfg.synthetic_source_override = true;

                let mut headset_cfg = DuplexSessionConfig::new(DuplexRole::Headset, lo_ip);
                headset_cfg.downlink_udp_port = 49353;
                headset_cfg.uplink_udp_port = 49355;
                headset_cfg.profile = lat_profile;
                headset_cfg.initial_volume = 0.0;
                headset_cfg.advertise_mdns = false;
                headset_cfg.synthetic_source_override = true;

                let host_session = DuplexSession::start(host_cfg)?;
                let headset_session = DuplexSession::start(headset_cfg)?;

                for sec in 1..=duration_secs {
                    thread::sleep(Duration::from_secs(1));
                    let h_st = host_session.status();
                    let hs_st = headset_session.status();
                    let h_tx = h_st.media_tx.as_ref().unwrap();
                    let h_rx = h_st.mic_rx.as_ref().unwrap();
                    let hs_rx = hs_st.media_rx.as_ref().unwrap();
                    let hs_tx = hs_st.mic_tx.as_ref().unwrap();
                    println!(
                        "  [{sec}s/{duration_secs}s] Media Bus (Host->Headset): TX {} / RX {} pkts (Buf {:.1}ms) | Mic Bus (Headset->Host): TX {} / RX {} pkts (Buf {:.1}ms, RTT {:.2}ms)",
                        h_tx.net.packets_sent,
                        hs_rx.net.packets_received,
                        hs_rx.net.buffer_level_ms,
                        hs_tx.net.packets_sent,
                        h_rx.net.packets_received,
                        h_rx.net.buffer_level_ms,
                        (hs_tx.control.rtt_us as f64) / 1000.0
                    );
                }

                let final_h = host_session.status();
                let final_hs = headset_session.status();
                let dl_pkts = final_hs.media_rx.as_ref().unwrap().net.packets_received;
                let ul_pkts = final_h.mic_rx.as_ref().unwrap().net.packets_received;
                println!(
                    "Full-Duplex self-test complete: Media Bus delivered {dl_pkts} pkts, Mic Bus delivered {ul_pkts} pkts."
                );
                return Ok(());
            }

            let test_addr: SocketAddr = "127.0.0.1:49255".parse().unwrap();

            println!(
                "Running E2E localhost pipeline self-test for {duration_secs}s (Profile: {lat_profile:?})..."
            );

            let rx_cfg = ReceiverSessionConfig {
                bind_addr: test_addr,
                device_name: None,
                channels: ChannelLayout::Stereo,
                target_buffer_ms: lat_profile.default_target_buffer_ms(),
                initial_volume: 0.0, // Mute speaker during automated self-test
                advertise_mdns: false,
                node_name: "rsrelay-selftest".to_string(),
            };
            let rx = ReceiverSession::start(rx_cfg)?;

            let tx_cfg = SenderSessionConfig {
                target_addr: test_addr,
                source: CaptureSource::SyntheticTone,
                device_name: None,
                codec: lat_profile.default_codec(),
                channels: ChannelLayout::Stereo,
                frame_duration: lat_profile.default_frame_duration(),
                opus_bitrate_bps: 128_000,
                enable_fec: lat_profile.default_fec(),
                noise_gate_db: None,
            };
            let tx = SenderSession::start(tx_cfg)?;

            for sec in 1..=duration_secs {
                thread::sleep(Duration::from_secs(1));
                let tx_st = tx.status();
                let rx_st = rx.status();
                println!(
                    "  [{sec}s/{duration_secs}s] TX Sent: {} pkts ({:.1} KB) | RX Recv: {} pkts, Lost: {}, RTT: {:.2} ms, Buffer: {:.1} ms, ASRC Ratio: {:.5}x",
                    tx_st.net.packets_sent,
                    (tx_st.net.bytes_sent as f64) / 1024.0,
                    rx_st.net.packets_received,
                    rx_st.net.packets_lost,
                    (tx_st.control.rtt_us as f64) / 1000.0,
                    rx_st.net.buffer_level_ms,
                    rx_st.net.resample_ratio
                );
            }

            let final_rx = rx.status();
            println!(
                "Self-test complete: {} packets delivered, {} lost, final ASRC drift ratio {:.5}x.",
                final_rx.net.packets_received, final_rx.net.packets_lost, final_rx.net.resample_ratio
            );
        }
    }

    Ok(())
}

fn setup_ctrlc_flag() -> Arc<AtomicBool> {
    let running = Arc::new(AtomicBool::new(true));
    let flag = Arc::clone(&running);
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _ = tokio::signal::ctrl_c().await;
            flag.store(false, Ordering::Relaxed);
        });
    });
    running
}

fn render_vu_bar(level: f32, width: usize) -> String {
    let filled = ((level.clamp(0.0, 1.0) * (width as f32)).round() as usize).min(width);
    let mut s = String::with_capacity(width);
    for i in 0..width {
        if i < filled {
            s.push('#');
        } else {
            s.push('-');
        }
    }
    s
}
