use std::time::Duration;

/// Watch the local route (address + default gateway). If it changes while a cross-screen
/// session exists, the link can't be trusted any more: drop the session like the native
/// client does (`PeerDisconnected` "network_changed") and tell the UI (`NetworkChanged`).
pub async fn run() {
    let mut last = route_fingerprint();
    loop {
        tokio::time::sleep(Duration::from_secs(3)).await;
        let current = route_fingerprint();
        if current == last {
            continue;
        }
        log::info!("[cross-screen] network changed: {last} -> {current}");
        last = current;
        let Some(rdp) = crate::rdp::instance() else { continue };
        if rdp.peer_ip().is_none() {
            continue;
        }
        rdp.teardown("network_changed");
        rdp.notify("NetworkChanged", serde_json::Value::Null);
    }
}

/// Default-route source address plus default gateway.
pub fn route_fingerprint() -> String {
    let ip = local_ip_address::local_ip().map(|ip| ip.to_string()).unwrap_or_default();
    format!("{ip}|{}", default_gateway().unwrap_or_default())
}

fn default_gateway() -> Option<String> {
    let text = std::fs::read_to_string("/proc/net/route").ok()?;
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.get(1) == Some(&"00000000") {
            let raw = u32::from_str_radix(f.get(2)?, 16).ok()?;
            return Some(std::net::Ipv4Addr::from(u32::from_be(raw)).to_string());
        }
    }
    None
}
