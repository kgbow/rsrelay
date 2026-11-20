use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rsrelay_proto::{DEFAULT_DISCOVERY_UDP_PORT, MDNS_SERVICE_TYPE, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;

const DISCOVER_REQ_MAGIC: &[u8] = b"RSRELAY_DISCOVER_REQ_V1";
const DISCOVER_RESP_PREFIX: &[u8] = b"RSRELAY_DISCOVER_RESP_V1:";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BeaconAnnouncement {
    pub instance_name: String,
    pub hostname: String,
    pub lan_ip: Option<IpAddr>,
    pub udp_port: u16,
    pub os: String,
    pub version: String,
}

fn ipv4_priority_score(ip: Ipv4Addr) -> u32 {
    let octets = ip.octets();
    if ip.is_loopback() || ip.is_unspecified() || ip.is_link_local() || ip.is_broadcast() {
        return 0;
    }
    // Deprioritize Clash / TUN benchmarking range 198.18.0.0/15
    if octets[0] == 198 && (octets[1] == 18 || octets[1] == 19) {
        return 10;
    }
    if octets[0] == 192 && octets[1] == 168 {
        // VirtualBox default host-only adapter is 192.168.56.1
        if octets[2] == 56 && octets[3] == 1 {
            return 60;
        }
        return 100;
    }
    if octets[0] == 10 {
        return 90;
    }
    if octets[0] == 172 && (16..=31).contains(&octets[1]) {
        // WSL / Hyper-V often uses .1 host address in 172.16..31
        if octets[3] == 1 {
            return 50;
        }
        return 80;
    }
    40
}

/// Enumerate all candidate local LAN IPv4 addresses on this machine, ordered by LAN priority
/// (e.g. `192.168.x.x` physical LAN interfaces before virtual/TUN adapters).
pub fn detect_all_local_ipv4s() -> Vec<Ipv4Addr> {
    let mut candidates: Vec<Ipv4Addr> = Vec::new();

    // 1. Probe OS routing table across common private LAN subnets + public routes
    let probe_targets = [
        "192.168.3.1:80",
        "192.168.1.1:80",
        "192.168.0.1:80",
        "192.168.2.1:80",
        "192.168.31.1:80",
        "192.168.50.1:80",
        "192.168.100.1:80",
        "10.0.0.1:80",
        "172.16.0.1:80",
        "192.0.2.1:80",
        "8.8.8.8:80",
    ];
    for target in probe_targets {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(target).is_ok() {
                if let Ok(local_addr) = sock.local_addr() {
                    if let IpAddr::V4(v4) = local_addr.ip() {
                        if ipv4_priority_score(v4) > 0 && !candidates.contains(&v4) {
                            candidates.push(v4);
                        }
                    }
                }
            }
        }
    }

    // 2. Also query system hostname resolution for multi-homed interfaces
    if let Ok(raw_host) = hostname::get() {
        if let Ok(host_str) = raw_host.into_string() {
            for query in [format!("{host_str}:0"), format!("{host_str}.local:0")] {
                if let Ok(addrs) = query.to_socket_addrs() {
                    for addr in addrs {
                        if let IpAddr::V4(v4) = addr.ip() {
                            if ipv4_priority_score(v4) > 0 && !candidates.contains(&v4) {
                                candidates.push(v4);
                            }
                        }
                    }
                }
            }
        }
    }

    candidates.sort_by_key(|ip| std::cmp::Reverse(ipv4_priority_score(*ip)));
    candidates
}

/// Probe the OS routing table via a non-transmitting UDP socket to determine the primary
/// local LAN IPv4 address (e.g. `192.168.x.x` or `10.x.x.x`). Sends zero network packets.
pub fn detect_local_lan_ip() -> Option<IpAddr> {
    detect_all_local_ipv4s().into_iter().next().map(IpAddr::V4)
}

#[derive(Debug, Error)]
pub enum DiscoveryError {
    #[error("mDNS daemon error: {0}")]
    Mdns(#[from] mdns_sd::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredPeer {
    pub instance_name: String,
    pub hostname: String,
    pub addresses: Vec<IpAddr>,
    pub udp_port: u16,
    pub os: String,
    pub version: String,
}

/// Lightweight cloneable handle for running LAN discovery scans on a background thread
/// while reusing an existing `mdns-sd` [`ServiceDaemon`] instead of spawning a new daemon
/// thread and multicast socket set on every scan.
#[derive(Clone)]
pub struct DiscoveryScanner {
    daemon: Option<ServiceDaemon>,
    beacon_port: u16,
}

impl DiscoveryScanner {
    /// Create a UDP-beacon-only scanner without spawning an `mdns-sd` background daemon.
    pub fn udp_only() -> Self {
        Self {
            daemon: None,
            beacon_port: DEFAULT_DISCOVERY_UDP_PORT,
        }
    }

    /// Browse the LAN for active `rsrelay` nodes during `scan_duration` using both
    /// UDP unicast/broadcast discovery probes and (when available) the shared mDNS daemon.
    pub fn scan(&self, scan_duration: Duration) -> Result<Vec<DiscoveredPeer>, DiscoveryError> {
        scan_lan_peers(self.daemon.as_ref(), self.beacon_port, scan_duration)
    }
}

/// Manages mDNS service advertisement + UDP LAN discovery beacon (`_rsrelay._udp.local.` & UDP `:49151`).
pub struct DiscoveryService {
    daemon: Option<ServiceDaemon>,
    fullname: Option<String>,
    beacon_stop: Arc<AtomicBool>,
    advertised_port: Arc<AtomicU16>,
    beacon_thread: Option<JoinHandle<()>>,
    beacon_port: u16,
}

impl DiscoveryService {
    pub fn new() -> Result<Self, DiscoveryError> {
        Self::with_beacon_port(DEFAULT_DISCOVERY_UDP_PORT)
    }

    pub fn with_beacon_port(beacon_port: u16) -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new().ok();
        Ok(Self {
            daemon,
            fullname: None,
            beacon_stop: Arc::new(AtomicBool::new(false)),
            advertised_port: Arc::new(AtomicU16::new(0)),
            beacon_thread: None,
            beacon_port,
        })
    }

    /// Dynamically update the advertised receiver UDP port without restarting the daemon.
    pub fn set_advertised_port(&self, udp_port: u16) {
        self.advertised_port.store(udp_port, Ordering::Relaxed);
    }

    /// Advertise this `rsrelay` node on the local network via both mDNS (`_rsrelay._udp.local.`)
    /// and the UDP LAN discovery beacon (`DEFAULT_DISCOVERY_UDP_PORT`).
    pub fn advertise(&mut self, instance_name: &str, udp_port: u16) -> Result<(), DiscoveryError> {
        self.advertised_port.store(udp_port, Ordering::Relaxed);

        let raw_host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "rsrelay-host".to_string());
        let clean_host = raw_host
            .trim_end_matches(".local.")
            .trim_end_matches(".local")
            .to_string();
        let host_fqdn = format!("{clean_host}.local.");

        // Ensure unique instance name per host so two desktops on the same LAN never collide
        let unique_instance = if instance_name
            .to_ascii_lowercase()
            .contains(&clean_host.to_ascii_lowercase())
        {
            instance_name.to_string()
        } else {
            format!("{instance_name}-{clean_host}")
        };

        let local_ips = detect_all_local_ipv4s();
        let primary_lan_ip = local_ips.first().copied().map(IpAddr::V4);

        if let Some(ref daemon) = self.daemon {
            let mut properties = HashMap::new();
            properties.insert("version".to_string(), PROTOCOL_VERSION.to_string());
            properties.insert("os".to_string(), std::env::consts::OS.to_string());
            properties.insert("udp_port".to_string(), udp_port.to_string());
            properties.insert("hostname".to_string(), clean_host.clone());
            if let Some(ip) = primary_lan_ip {
                properties.insert("lan_ip".to_string(), ip.to_string());
            }

            let ip_csv = local_ips
                .iter()
                .map(|ip| ip.to_string())
                .collect::<Vec<_>>()
                .join(",");

            if let Ok(service_info) = ServiceInfo::new(
                MDNS_SERVICE_TYPE,
                &unique_instance,
                &host_fqdn,
                ip_csv.as_str(),
                udp_port,
                properties,
            ) {
                let info = service_info.enable_addr_auto();
                self.fullname = Some(info.get_fullname().to_string());
                let _ = daemon.register(info);
            }
        }

        // Start UDP broadcast + unicast discovery beacon responder if not already running
        if self.beacon_thread.is_none() {
            self.beacon_stop.store(false, Ordering::Relaxed);
            let stop_flag = Arc::clone(&self.beacon_stop);
            let port_atomic = Arc::clone(&self.advertised_port);
            let beacon_port = self.beacon_port;
            let inst_clone = unique_instance;
            let host_clone = clean_host;

            if let Ok(handle) = thread::Builder::new()
                .name("rsrelay-lan-beacon".to_string())
                .spawn(move || {
                    run_udp_discovery_beacon(
                        beacon_port,
                        stop_flag,
                        port_atomic,
                        inst_clone,
                        host_clone,
                    );
                })
            {
                self.beacon_thread = Some(handle);
            }
        }

        Ok(())
    }

    /// Obtain a lightweight cloneable [`DiscoveryScanner`] sharing this service's mDNS daemon.
    pub fn scanner(&self) -> DiscoveryScanner {
        DiscoveryScanner {
            daemon: self.daemon.clone(),
            beacon_port: self.beacon_port,
        }
    }

    /// Whether this service is currently advertising an active mDNS record or UDP beacon.
    pub fn is_advertising(&self) -> bool {
        self.beacon_thread.is_some() || self.fullname.is_some()
    }

    /// Stop mDNS service advertisement and UDP beacon responder while keeping the underlying
    /// `ServiceDaemon` alive for future reuse without re-spawning threads or sockets.
    pub fn unadvertise(&mut self) {
        self.advertised_port.store(0, Ordering::Relaxed);
        self.beacon_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.beacon_thread.take() {
            let _ = handle.join();
        }
        if let Some(ref daemon) = self.daemon {
            if let Some(fullname) = self.fullname.take() {
                unregister_mdns_service(daemon, &fullname);
            }
        }
    }

    /// Browse the LAN for active `rsrelay` nodes during `scan_duration` using both
    /// UDP unicast/broadcast discovery probes and mDNS (`_rsrelay._udp.local.`).
    pub fn scan(&self, scan_duration: Duration) -> Result<Vec<DiscoveredPeer>, DiscoveryError> {
        scan_lan_peers(self.daemon.as_ref(), self.beacon_port, scan_duration)
    }
}

fn scan_lan_peers(
    daemon: Option<&ServiceDaemon>,
    beacon_port: u16,
    scan_duration: Duration,
) -> Result<Vec<DiscoveredPeer>, DiscoveryError> {
    let mdns_rx = daemon.and_then(|d| d.browse(MDNS_SERVICE_TYPE).ok());

    let local_ips = detect_all_local_ipv4s();
    let udp_probe_sock = bind_ephemeral_discovery_socket().ok();

    if let Some(ref sock) = udp_probe_sock {
        send_lan_discovery_probes(sock, &local_ips, beacon_port);
    }

    let start = Instant::now();
    let deadline = start + scan_duration;
    let mut second_probe_sent = false;
    let mut peers: HashMap<String, DiscoveredPeer> = HashMap::new();
    let mut recv_buf = [0u8; 2048];

    while Instant::now() < deadline {
        if !second_probe_sent
            && start.elapsed() >= (scan_duration / 2).max(Duration::from_millis(120))
        {
            second_probe_sent = true;
            if let Some(ref sock) = udp_probe_sock {
                send_lan_discovery_probes(sock, &local_ips, beacon_port);
            }
        }

        // 1. Drain UDP discovery beacon responses
        if let Some(ref sock) = udp_probe_sock {
            loop {
                match sock.recv_from(&mut recv_buf) {
                    Ok((len, src_addr)) => {
                        if let Some(ann) = parse_beacon_response(&recv_buf[..len]) {
                            let mut addrs = Vec::new();
                            let src_ip = src_addr.ip();
                            if !src_ip.is_loopback() && !src_ip.is_unspecified() {
                                addrs.push(src_ip);
                            }
                            if let Some(lan_ip) = ann.lan_ip {
                                if !addrs.contains(&lan_ip) {
                                    addrs.push(lan_ip);
                                }
                            }
                            if addrs.is_empty() && !src_ip.is_unspecified() {
                                addrs.push(src_ip);
                            }
                            sort_peer_addresses(&mut addrs, &local_ips);

                            let clean_host = ann
                                .hostname
                                .trim_end_matches(".local.")
                                .trim_end_matches(".local")
                                .to_string();
                            let key = addrs
                                .first()
                                .map(|ip| format!("{ip}:{}", ann.udp_port))
                                .unwrap_or_else(|| ann.instance_name.clone());

                            peers.insert(
                                key,
                                DiscoveredPeer {
                                    instance_name: ann.instance_name,
                                    hostname: clean_host,
                                    addresses: addrs,
                                    udp_port: ann.udp_port,
                                    os: ann.os,
                                    version: ann.version,
                                },
                            );
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
        }

        // 2. Poll mDNS browse channel
        if let Some(ref rx) = mdns_rx {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(remaining.min(Duration::from_millis(25))) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    let fullname = info.get_fullname().to_string();
                    let mut addresses: Vec<IpAddr> =
                        info.get_addresses().iter().copied().collect();
                    if let Some(lan_ip_str) = info.get_property_val_str("lan_ip") {
                        if let Ok(ip) = lan_ip_str.parse::<IpAddr>() {
                            if !addresses.contains(&ip) {
                                addresses.push(ip);
                            }
                        }
                    }
                    sort_peer_addresses(&mut addresses, &local_ips);

                    let os = info
                        .get_property_val_str("os")
                        .unwrap_or("unknown")
                        .to_string();
                    let version = info
                        .get_property_val_str("version")
                        .unwrap_or("1")
                        .to_string();
                    let clean_host = info
                        .get_property_val_str("hostname")
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| {
                            info.get_hostname()
                                .trim_end_matches(".local.")
                                .trim_end_matches(".local")
                                .to_string()
                        });
                    let udp_port = info.get_port();
                    let key = addresses
                        .first()
                        .map(|ip| format!("{ip}:{udp_port}"))
                        .unwrap_or_else(|| fullname.clone());

                    peers.entry(key).or_insert(DiscoveredPeer {
                        instance_name: fullname,
                        hostname: clean_host,
                        addresses,
                        udp_port,
                        os,
                        version,
                    });
                }
                Ok(_) => {}
                Err(_) => {}
            }
        } else {
            thread::sleep(Duration::from_millis(15));
        }
    }

    if let Some(d) = daemon {
        for _ in 0..3 {
            match d.stop_browse(MDNS_SERVICE_TYPE) {
                Err(mdns_sd::Error::Again) => thread::sleep(Duration::from_millis(5)),
                _ => break,
            }
        }
    }

    let mut list: Vec<DiscoveredPeer> = peers.into_values().collect();
    list.sort_by(|a, b| a.hostname.cmp(&b.hostname));
    Ok(list)
}

fn unregister_mdns_service(daemon: &ServiceDaemon, fullname: &str) {
    for _ in 0..4 {
        match daemon.unregister(fullname) {
            Ok(_) => break,
            Err(mdns_sd::Error::Again) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
}

fn shutdown_mdns_daemon(daemon: &ServiceDaemon) {
    for _ in 0..6 {
        match daemon.shutdown() {
            Ok(status_rx) => {
                let _ = status_rx.recv_timeout(Duration::from_millis(150));
                break;
            }
            Err(mdns_sd::Error::Again) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        self.unadvertise();
        if let Some(ref daemon) = self.daemon {
            shutdown_mdns_daemon(daemon);
        }
    }
}

/// Sort a peer's IP addresses so IPv4 addresses on the same `/24` subnet as this machine
/// come first, followed by other private IPv4 addresses, and IPv6 addresses last.
fn sort_peer_addresses(addrs: &mut [IpAddr], local_ipv4s: &[Ipv4Addr]) {
    addrs.sort_by_key(|ip| {
        let score = match ip {
            IpAddr::V4(v4) => {
                let oct = v4.octets();
                let same_subnet = local_ipv4s.iter().any(|loc| {
                    let lo = loc.octets();
                    lo[0] == oct[0] && lo[1] == oct[1] && lo[2] == oct[2]
                });
                let base = ipv4_priority_score(*v4);
                if same_subnet {
                    base + 200
                } else if v4.is_loopback() {
                    5
                } else {
                    base
                }
            }
            IpAddr::V6(v6) => {
                if v6.is_loopback() {
                    1
                } else {
                    2
                }
            }
        };
        std::cmp::Reverse(score)
    });
}

fn bind_ephemeral_discovery_socket() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_broadcast(true);
    let _ = socket.set_reuse_address(true);
    let bind_addr: SocketAddr = ([0, 0, 0, 0], 0).into();
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    Ok(socket.into())
}

fn bind_beacon_listener_socket(port: u16) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_reuse_address(true);
    let _ = socket.set_broadcast(true);
    let bind_addr: SocketAddr = ([0, 0, 0, 0], port).into();
    socket.bind(&bind_addr.into())?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    Ok(socket.into())
}

fn encode_beacon_response(ann: &BeaconAnnouncement) -> Option<Vec<u8>> {
    let json = serde_json::to_vec(ann).ok()?;
    let mut pkt = Vec::with_capacity(DISCOVER_RESP_PREFIX.len() + json.len());
    pkt.extend_from_slice(DISCOVER_RESP_PREFIX);
    pkt.extend_from_slice(&json);
    Some(pkt)
}

fn parse_beacon_response(pkt: &[u8]) -> Option<BeaconAnnouncement> {
    let json_slice = pkt.strip_prefix(DISCOVER_RESP_PREFIX)?;
    serde_json::from_slice(json_slice).ok()
}

fn send_lan_discovery_probes(sock: &UdpSocket, local_ips: &[Ipv4Addr], beacon_port: u16) {
    // 1. Localhost & limited broadcast
    let _ = sock.send_to(
        DISCOVER_REQ_MAGIC,
        SocketAddr::from(([127, 0, 0, 1], beacon_port)),
    );
    let _ = sock.send_to(
        DISCOVER_REQ_MAGIC,
        SocketAddr::from(([255, 255, 255, 255], beacon_port)),
    );

    // 2. For up to 3 local private IPv4 subnets, send both directed broadcast (.255)
    //    and a fast unicast /24 sweep (.1..=254) so routers that block multicast/broadcast
    //    still deliver discovery probes over normal unicast UDP.
    for ip in local_ips.iter().take(3) {
        let [a, b, c, self_d] = ip.octets();
        let _ = sock.send_to(
            DISCOVER_REQ_MAGIC,
            SocketAddr::from(([a, b, c, 255], beacon_port)),
        );
        for host_d in 1..=254u8 {
            if host_d == self_d {
                continue;
            }
            let _ = sock.send_to(
                DISCOVER_REQ_MAGIC,
                SocketAddr::from(([a, b, c, host_d], beacon_port)),
            );
        }
    }
}

fn run_udp_discovery_beacon(
    beacon_port: u16,
    stop_flag: Arc<AtomicBool>,
    advertised_port: Arc<AtomicU16>,
    instance_name: String,
    hostname: String,
) {
    let Ok(sock) = bind_beacon_listener_socket(beacon_port) else {
        return;
    };

    let mut buf = [0u8; 1024];
    let mut last_broadcast = Instant::now() - Duration::from_secs(10);

    while !stop_flag.load(Ordering::Relaxed) {
        let udp_port = advertised_port.load(Ordering::Relaxed);
        if udp_port > 0 && last_broadcast.elapsed() >= Duration::from_millis(2500) {
            last_broadcast = Instant::now();
            let local_ips = detect_all_local_ipv4s();
            let ann = BeaconAnnouncement {
                instance_name: instance_name.clone(),
                hostname: hostname.clone(),
                lan_ip: local_ips.first().copied().map(IpAddr::V4),
                udp_port,
                os: std::env::consts::OS.to_string(),
                version: PROTOCOL_VERSION.to_string(),
            };
            if let Some(pkt) = encode_beacon_response(&ann) {
                let _ = sock.send_to(&pkt, SocketAddr::from(([255, 255, 255, 255], beacon_port)));
                for ip in local_ips.iter().take(3) {
                    let [a, b, c, _] = ip.octets();
                    let _ = sock.send_to(&pkt, SocketAddr::from(([a, b, c, 255], beacon_port)));
                }
            }
        }

        match sock.recv_from(&mut buf) {
            Ok((len, src_addr)) => {
                if buf[..len].starts_with(DISCOVER_REQ_MAGIC) {
                    let current_port = advertised_port.load(Ordering::Relaxed);
                    if current_port > 0 {
                        let ann = BeaconAnnouncement {
                            instance_name: instance_name.clone(),
                            hostname: hostname.clone(),
                            lan_ip: detect_local_lan_ip(),
                            udp_port: current_port,
                            os: std::env::consts::OS.to_string(),
                            version: PROTOCOL_VERSION.to_string(),
                        };
                        if let Some(pkt) = encode_beacon_response(&ann) {
                            let _ = sock.send_to(&pkt, src_addr);
                        }
                    }
                }
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_udp_beacon_advertise_and_scan_roundtrip() {
        let test_beacon_port = 59151;
        let mut advertiser =
            DiscoveryService::with_beacon_port(test_beacon_port).expect("create advertiser");
        advertiser
            .advertise("rsrelay-test-node", 49153)
            .expect("advertise node");

        let scanner = advertiser.scanner();
        let peers = scanner
            .scan(Duration::from_millis(350))
            .expect("scan peers via shared scanner");

        assert!(
            !peers.is_empty(),
            "UDP discovery beacon should discover the local advertised node"
        );
        let found = peers.iter().find(|p| p.udp_port == 49153).expect("find port 49153");
        assert!(!found.addresses.is_empty());
        assert!(!found.hostname.is_empty());

        advertiser.unadvertise();
        assert!(!advertiser.is_advertising());
    }

    #[test]
    fn test_sort_peer_addresses_prefers_matching_subnet_ipv4() {
        let local_ips = vec![Ipv4Addr::new(192, 168, 3, 151)];
        let mut addrs: Vec<IpAddr> = vec![
            "fe80::1".parse().unwrap(),
            "172.24.16.1".parse().unwrap(),
            "192.168.3.110".parse().unwrap(),
        ];
        sort_peer_addresses(&mut addrs, &local_ips);
        assert_eq!(addrs[0], "192.168.3.110".parse::<IpAddr>().unwrap());
    }
}
