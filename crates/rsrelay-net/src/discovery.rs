use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rsrelay_proto::{MDNS_SERVICE_TYPE, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

/// Manages mDNS service advertisement and LAN peer discovery.
pub struct DiscoveryService {
    daemon: ServiceDaemon,
    fullname: Option<String>,
}

impl DiscoveryService {
    pub fn new() -> Result<Self, DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        Ok(Self {
            daemon,
            fullname: None,
        })
    }

    /// Advertise this `rsrelay` node on the local network via mDNS (`_rsrelay._udp.local.`).
    pub fn advertise(&mut self, instance_name: &str, udp_port: u16) -> Result<(), DiscoveryError> {
        let raw_host = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "rsrelay-host".to_string());
        let host_fqdn = if raw_host.ends_with(".local.") {
            raw_host
        } else if raw_host.ends_with(".local") {
            format!("{raw_host}.")
        } else {
            format!("{raw_host}.local.")
        };

        let mut properties = HashMap::new();
        properties.insert("version".to_string(), PROTOCOL_VERSION.to_string());
        properties.insert("os".to_string(), std::env::consts::OS.to_string());
        properties.insert("udp_port".to_string(), udp_port.to_string());

        let service_info = ServiceInfo::new(
            MDNS_SERVICE_TYPE,
            instance_name,
            &host_fqdn,
            "",
            udp_port,
            properties,
        )?
        .enable_addr_auto();

        self.fullname = Some(service_info.get_fullname().to_string());
        self.daemon.register(service_info)?;
        Ok(())
    }

    /// Browse the LAN for active `rsrelay` nodes during `scan_duration`.
    pub fn scan(&self, scan_duration: Duration) -> Result<Vec<DiscoveredPeer>, DiscoveryError> {
        let receiver = self.daemon.browse(MDNS_SERVICE_TYPE)?;
        let deadline = Instant::now() + scan_duration;
        let mut peers: HashMap<String, DiscoveredPeer> = HashMap::new();

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(ServiceEvent::ServiceResolved(info)) => {
                    let fullname = info.get_fullname().to_string();
                    let addresses: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                    let os = info
                        .get_property_val_str("os")
                        .unwrap_or("unknown")
                        .to_string();
                    let version = info
                        .get_property_val_str("version")
                        .unwrap_or("1")
                        .to_string();

                    peers.insert(
                        fullname.clone(),
                        DiscoveredPeer {
                            instance_name: fullname,
                            hostname: info.get_hostname().to_string(),
                            addresses,
                            udp_port: info.get_port(),
                            os,
                            version,
                        },
                    );
                }
                Ok(_) => {}
                Err(_) => {}
            }
        }

        let _ = self.daemon.stop_browse(MDNS_SERVICE_TYPE);
        Ok(peers.into_values().collect())
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        if let Some(fullname) = self.fullname.take() {
            let _ = self.daemon.unregister(&fullname);
        }
        let _ = self.daemon.shutdown();
    }
}
