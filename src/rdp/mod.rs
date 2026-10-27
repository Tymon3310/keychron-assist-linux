//! Cross-screen ("RDP" in the Keychron UI): discovery, pairing, heartbeat, input sharing
//! and file transfer, wire-compatible with Keychron Assist on Windows and macOS.

pub mod capture;
pub mod commands;
pub mod files;
pub mod inject;
pub mod keymap;
pub mod screen;
pub mod wire;

use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager};
use wire::*;

const HEARTBEAT: Duration = Duration::from_secs(1);
const PEER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug)]
pub struct Peer {
    pub ip: IpAddr,
    pub device_id: String,
    pub name: String,
    pub os_info: String,
    pub session_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Idle,
    Controlling,
    Controlled,
}

#[derive(Default)]
struct Settings {
    disconnect_key: Option<String>,
    lock_key: Option<String>,
}

pub struct St {
    pub scanning: bool,
    /// Side of our screen the peer sits on; `None` = "Off" (no crossing).
    pub dir: Option<RdpDir>,
    pub pairing: Option<IpAddr>,
    pub incoming: Option<(IpAddr, Instant, Value)>,
    pub peer: Option<Peer>,
    pub role: Role,
    pub last_rx: Instant,
    pub file_enabled: bool,
    pub peer_file_enabled: bool,
    shortcuts: Settings,
    /// Last time a JSON beacon was seen per IP (to suppress the legacy text duplicate).
    beacons: HashMap<IpAddr, Instant>,
    confirm_seen: Option<(IpAddr, Instant)>,
    last_pair: Option<(String, Instant)>,
}

pub struct Local {
    pub device_id: String,
    pub name: String,
    pub os_info: String,
    pub session_id: String,
}

static INSTANCE: OnceLock<Arc<Rdp>> = OnceLock::new();

/// True while our keyboard/mouse are being forwarded to the peer.
pub fn forwarding() -> bool {
    INSTANCE.get().is_some_and(|r| r.st.lock().unwrap().role == Role::Controlling)
}

pub fn instance() -> Option<Arc<Rdp>> {
    INSTANCE.get().cloned()
}

pub struct Rdp {
    pub app: AppHandle,
    pub root: PathBuf,
    sock: UdpSocket,
    disc: UdpSocket,
    pub local: Local,
    pub st: Mutex<St>,
    pub inj: inject::Injector,
    cap: OnceLock<Arc<capture::Capture>>,
    pub files: files::Files,
    mons: Mutex<(Instant, Vec<screen::Mon>)>,
    edge_check: Mutex<Instant>,
}

fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    Ok(s.into())
}

pub fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn local_info(root: &std::path::Path) -> Local {
    let id_path = root.join("device_id");
    let device_id = std::fs::read_to_string(&id_path).ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).unwrap_or_else(|| {
        let id = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::write(&id_path, &id);
        id
    });
    let name = std::fs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_owned()).unwrap_or_else(|_| "Linux".into());
    // Same style as "Windows 11 (26200)": distro name plus kernel version.
    let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let distro = release.lines().find_map(|l| l.strip_prefix("NAME=")).unwrap_or("Linux").trim_matches('"').to_owned();
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let kernel: String = kernel.trim().split(|c: char| c == '-').next().unwrap_or("").to_owned();
    let os_info = if distro.contains("Linux") { format!("{distro} ({kernel})") } else { format!("{distro} Linux ({kernel})") };
    let session_id = format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    Local { device_id, name, os_info, session_id }
}

impl Rdp {
    pub fn start(app: AppHandle, root: PathBuf) -> std::io::Result<Arc<Self>> {
        let sock = bind(SESSION_PORT)?;
        let disc = bind(DISCOVERY_PORT)?;
        if let Err(e) = disc.join_multicast_v4(&MULTICAST_GROUP, &Ipv4Addr::UNSPECIFIED) {
            log::warn!("join multicast: {e}");
        }
        let receive_dir = files::load_receive_dir(&root);
        let rdp = Arc::new(Self {
            app,
            local: local_info(&root),
            root,
            sock,
            disc,
            st: Mutex::new(St {
                scanning: false,
                dir: Some(RdpDir::Right),
                pairing: None,
                incoming: None,
                peer: None,
                role: Role::Idle,
                last_rx: Instant::now(),
                file_enabled: true,
                peer_file_enabled: true,
                shortcuts: Settings::default(),
                beacons: HashMap::new(),
                confirm_seen: None,
                last_pair: None,
            }),
            inj: {
                screen::init();
                inject::Injector::new()
            },
            cap: OnceLock::new(),
            files: files::Files::new(receive_dir),
            mons: Mutex::new((Instant::now() - Duration::from_secs(60), Vec::new())),
            edge_check: Mutex::new(Instant::now()),
        });
        log::info!("[cross-screen] local device {} \"{}\" {} session {}", rdp.local.device_id, rdp.local.name, rdp.local.os_info, rdp.local.session_id);
        let _ = INSTANCE.set(rdp.clone());
        let _ = rdp.cap.set(capture::Capture::start(Arc::new(CapHost(rdp.clone()))));
        for (name, f) in [("rdp-session", Self::session_loop as fn(Arc<Self>)), ("rdp-discovery", Self::discovery_loop), ("rdp-timer", Self::timer_loop)] {
            let r = rdp.clone();
            std::thread::Builder::new().name(name.into()).spawn(move || f(r))?;
        }
        Ok(rdp)
    }

    fn cap(&self) -> &capture::Capture {
        self.cap.get().expect("capture started")
    }

    // ---------- sending ----------

    pub fn send(&self, ip: IpAddr, kind: &str, data: Value) {
        let p = packet(kind, data);
        if let Err(e) = self.sock.send_to(&p, SocketAddr::new(ip, SESSION_PORT)) {
            log::warn!("send {kind} to {ip}: {e}");
        }
    }

    pub fn peer_ip(&self) -> Option<IpAddr> {
        self.st.lock().unwrap().peer.as_ref().map(|p| p.ip)
    }

    pub fn peer(&self) -> Option<Peer> {
        self.st.lock().unwrap().peer.clone()
    }

    pub fn send_toc(&self, t: &Toc) {
        if let Some(ip) = self.peer_ip() {
            self.send(ip, "RequestToC", serde_json::to_value(t).unwrap());
        }
    }

    // ---------- UI events ----------

    pub fn notify(&self, evt_type: &str, data: Value) {
        let _ = self.app.emit("notify", json!({"evt_type": evt_type, "evt_data": data}));
    }

    pub fn notify_rdp(&self, kind: &str, data: Value) {
        self.notify("RDP", json!({"evt_type": kind, "evt_data": data}));
    }

    pub(crate) fn pair_window(&self, show: bool) {
        if let Some(w) = self.app.get_webview_window("pair-confirm") {
            if show {
                let _ = w.set_always_on_top(true);
                let _ = w.set_visible_on_all_workspaces(true);
                let _ = w.show().and_then(|_| w.set_focus());
                let w = w.clone();
                std::thread::spawn(move || {
                    screen::float_dialog("Connection Request", 460, 300);
                    let _ = w.center();
                });
            } else {
                let _ = w.hide();
            }
        }
    }

    // ---------- local description ----------

    pub fn local_ip_for(&self, peer: Option<IpAddr>) -> Option<IpAddr> {
        // The source address the kernel would use towards the peer (or the default route).
        let s = UdpSocket::bind("0.0.0.0:0").ok()?;
        s.connect(SocketAddr::new(peer.unwrap_or(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))), 9)).ok()?;
        Some(s.local_addr().ok()?.ip())
    }

    pub fn device_info(&self) -> Value {
        json!({"device_id": self.local.device_id, "name": self.local.name, "os_info": self.local.os_info, "screen_resolution": screen::resolution()})
    }

    fn beacon(&self) -> PeerClient {
        let ip = self.local_ip_for(None).map(|i| i.to_string()).unwrap_or_default();
        let (iface, mac) = iface_for(&ip);
        PeerClient {
            device_id: Some(self.local.device_id.clone()),
            addr: ip,
            name: self.local.name.clone(),
            os_info: self.local.os_info.clone(),
            network_type: Some(if iface.starts_with('w') { "wifi" } else { "ethernet" }.into()),
            interface_name: Some(iface),
            mac_address: Some(mac),
            session_id: Some(self.local.session_id.clone()),
            screen_resolution: Some(screen::resolution()),
            remember: false,
        }
    }

    pub fn pair_info(&self, dir_for_peer: RdpDir, trust_uuid: Option<String>, automatic: bool) -> RequestPairInfo {
        RequestPairInfo {
            monitors: screen::monitor_list(),
            os_info: self.local.os_info.clone(),
            screen_resolution: Some(screen::resolution()),
            addr: String::new(),
            name: self.local.name.clone(),
            device_id: Some(self.local.device_id.clone()),
            session_id: Some(self.local.session_id.clone()),
            dir: dir_for_peer,
            trust_uuid,
            automatic,
        }
    }

    pub fn confirm_info(&self, trust_uuid: Option<String>) -> ConfirmInfo {
        ConfirmInfo {
            addr: String::new(),
            device_id: Some(self.local.device_id.clone()),
            name: self.local.name.clone(),
            os_info: self.local.os_info.clone(),
            session_id: Some(self.local.session_id.clone()),
            screen_resolution: Some(screen::resolution()),
            trust_uuid,
        }
    }

    // ---------- session state ----------

    pub fn connect_peer(&self, peer: Peer) {
        log::info!("[cross-screen] connected to {} ({}) at {}", peer.name, peer.os_info, peer.ip);
        let mut st = self.st.lock().unwrap();
        st.pairing = None;
        st.incoming = None;
        st.peer = Some(peer);
        st.role = Role::Idle;
        st.last_rx = Instant::now();
        st.peer_file_enabled = true;
    }

    /// Drop the session. `reason`: local_disconnect | peer_disconnect | network_timeout | network_changed.
    pub fn teardown(&self, reason: &str) {
        let had = {
            let mut st = self.st.lock().unwrap();
            st.role = Role::Idle;
            st.pairing = None;
            st.peer.take()
        };
        self.cap().stop();
        self.inj.release_all();
        if let Some(p) = had {
            log::info!("[cross-screen] disconnected from {} ({reason})", p.name);
            files::terminate_for_disconnect(self);
            self.notify_rdp("PeerDisconnected", json!(reason));
        }
    }

    /// Put the cursor at (x, y): directly where the desktop allows it, else (KWin) by
    /// relative moves through the virtual pointer.
    fn warp(&self, x: f64, y: f64) {
        if screen::can_warp() {
            screen::warp(x, y);
        } else if !screen::warp_relative(x, y, |dx, dy| self.inj.apply(&Toc::MouseMove(dx, dy))) {
            log::debug!("[screen] relative warp to ({x}, {y}) did not settle");
        }
    }

    fn monitors(&self) -> Vec<screen::Mon> {
        let mut m = self.mons.lock().unwrap();
        if m.0.elapsed() > Duration::from_secs(2) {
            *m = (Instant::now(), screen::monitors());
        }
        m.1.clone()
    }

    // ---------- control hand-over ----------

    fn become_controlled(&self, ratio: (f32, f32)) {
        let dir = {
            let mut st = self.st.lock().unwrap();
            if st.role == Role::Controlling {
                drop(st);
                self.cap().stop();
                st = self.st.lock().unwrap();
            }
            st.role = Role::Controlled;
            st.dir
        };
        let Some(dir) = dir else { return };
        if let Some((x, y)) = screen::entry_point(&self.monitors(), dir, ratio, 3.0) {
            self.warp(x, y);
        }
        log::info!("[cross-screen] peer crossed over, now controlled: entry_ratio=({}, {})", ratio.0, ratio.1);
    }

    /// Controlled side: our cursor hit the edge facing the peer; hand control back.
    fn give_back(&self, exit: (f32, f32)) {
        {
            let mut st = self.st.lock().unwrap();
            if st.role != Role::Controlled {
                return;
            }
            st.role = Role::Idle;
        }
        self.inj.release_all();
        self.send_toc(&Toc::Release(exit.0, exit.1));
        log::info!("[cross-screen] gave control back at ({}, {})", exit.0, exit.1);
    }

    /// Controlling side: the peer handed control back at its exit point `ratio`.
    fn control_returned(&self, ratio: (f32, f32)) {
        let dir = {
            let mut st = self.st.lock().unwrap();
            if st.role != Role::Controlling {
                return;
            }
            st.role = Role::Idle;
            st.dir
        };
        self.cap().stop();
        if let Some(dir) = dir {
            if let Some((x, y)) = screen::entry_point(&self.monitors(), dir, ratio, 3.0) {
                self.warp(x, y);
            }
        }
        log::info!("[cross-screen] control returned: ratio=({}, {})", ratio.0, ratio.1);
    }

    /// `rdp_release_mouse`: stop any cross-screen control right now.
    pub fn release_mouse(&self) {
        let (role, dir) = {
            let st = self.st.lock().unwrap();
            (st.role, st.dir)
        };
        match role {
            Role::Controlling => {
                self.cap().stop();
                self.st.lock().unwrap().role = Role::Idle;
                if let Some(dir) = dir {
                    if let Some((x, y)) = screen::entry_point(&self.monitors(), dir, (0.5, 0.5), 40.0) {
                        self.warp(x, y);
                    }
                }
            }
            Role::Controlled => {
                let exit = match dir.unwrap_or(RdpDir::Right) {
                    RdpDir::Left => (0.0, 0.5),
                    RdpDir::Right => (1.0, 0.5),
                    RdpDir::Top => (0.5, 0.0),
                    RdpDir::Bottom => (0.5, 1.0),
                };
                self.give_back(exit);
            }
            Role::Idle => {}
        }
    }

    fn after_injected_move(&self, dx: i64, dy: i64) {
        let dir = {
            let st = self.st.lock().unwrap();
            if st.role != Role::Controlled {
                return;
            }
            st.dir
        };
        let Some(dir) = dir else { return };
        let pushing = match dir {
            RdpDir::Left => dx < 0,
            RdpDir::Right => dx > 0,
            RdpDir::Top => dy < 0,
            RdpDir::Bottom => dy > 0,
        };
        if !pushing {
            return;
        }
        {
            let mut t = self.edge_check.lock().unwrap();
            if t.elapsed() < Duration::from_millis(4) {
                return;
            }
            *t = Instant::now();
        }
        if let Some((x, y)) = screen::cursor() {
            if let Some(exit) = screen::edge_exit(&self.monitors(), x, y, dir) {
                self.give_back(exit);
            }
        }
    }

    // ---------- threads ----------

    fn timer_loop(self: Arc<Self>) {
        let mut last_beacon = Instant::now() - Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(250));
            let (scanning, peer, last_rx) = {
                let st = self.st.lock().unwrap();
                (st.scanning, st.peer.as_ref().map(|p| p.ip), st.last_rx)
            };
            if peer.is_some() {
                if last_rx.elapsed() > PEER_TIMEOUT {
                    self.teardown("network_timeout");
                    continue;
                }
            }
            if last_beacon.elapsed() >= HEARTBEAT {
                last_beacon = Instant::now();
                if let Some(ip) = peer {
                    self.send(ip, "Heartbeat", json!(""));
                }
                if scanning {
                    self.send_beacons();
                }
            }
        }
    }

    fn send_beacons(&self) {
        let b = self.beacon();
        let json = format!("DISCOVER_JSON:{}", serde_json::to_string(&b).unwrap());
        let legacy = format!("DISCOVER:{}:{}", b.name, b.os_info);
        let mut bcast = broadcast_addrs();
        if bcast.is_empty() {
            bcast.push(Ipv4Addr::BROADCAST);
        }
        let mut targets: Vec<SocketAddr> = vec![SocketAddr::from((MULTICAST_GROUP, DISCOVERY_PORT))];
        targets.extend(bcast.into_iter().map(|a| SocketAddr::from((a, DISCOVERY_PORT))));
        for t in targets {
            let _ = self.sock.send_to(json.as_bytes(), t);
            let _ = self.sock.send_to(legacy.as_bytes(), t);
        }
    }

    fn discovery_loop(self: Arc<Self>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let Ok((n, src)) = self.disc.recv_from(&mut buf) else { continue };
            let data = &buf[..n];
            let ip = src.ip();
            let peer = if let Some(j) = data.strip_prefix(b"DISCOVER_JSON:") {
                let Ok(mut p) = serde_json::from_slice::<PeerClient>(j) else { continue };
                self.st.lock().unwrap().beacons.insert(ip, Instant::now());
                if p.addr.is_empty() {
                    p.addr = ip.to_string();
                }
                p
            } else if let Some(t) = data.strip_prefix(b"DISCOVER:") {
                if self.st.lock().unwrap().beacons.get(&ip).is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
                    continue;
                }
                let t = String::from_utf8_lossy(t);
                let (name, os) = t.split_once(':').unwrap_or((&t, ""));
                PeerClient { addr: ip.to_string(), name: name.into(), os_info: os.into(), ..Default::default() }
            } else {
                continue;
            };
            if peer.device_id.as_deref() == Some(&self.local.device_id) || peer.session_id.as_deref() == Some(&self.local.session_id) {
                continue;
            }
            if self.st.lock().unwrap().scanning {
                self.notify_rdp("Discover", serde_json::to_value(&peer).unwrap());
            }
        }
    }

    fn session_loop(self: Arc<Self>) {
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, src) = match self.sock.recv_from(&mut buf) {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("session recv: {e}");
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            let Some((kind, data)) = parse(&buf[..n]) else { continue };
            self.handle(src.ip(), &kind, data);
        }
    }

    fn handle(&self, ip: IpAddr, kind: &str, mut data: Value) {
        let from_peer = {
            let mut st = self.st.lock().unwrap();
            let is = st.peer.as_ref().is_some_and(|p| p.ip == ip);
            if is {
                st.last_rx = Instant::now();
            }
            is
        };
        let set_addr = |d: &mut Value| {
            if let Some(o) = d.as_object_mut() {
                o.insert("addr".into(), json!(ip.to_string()));
            }
        };
        match kind {
            "Heartbeat" => {}
            "RequestToC" if from_peer => self.handle_toc(data),
            "RequestToC" => {}
            "FileTransferSwitch" => {
                if from_peer {
                    let on = data.as_bool().unwrap_or(true);
                    self.st.lock().unwrap().peer_file_enabled = on;
                    self.notify_rdp("FileTransferSwitch", json!(on));
                }
            }
            k if k.starts_with("File") => files::handle(self, ip, from_peer, k, data),
            "RequestPair" => {
                set_addr(&mut data);
                let session = data.get("session_id").and_then(Value::as_str).unwrap_or("").to_owned();
                {
                    let mut st = self.st.lock().unwrap();
                    // RequestPair is sent several times; handle each (peer, session, trust) once.
                    let key = format!("{ip}|{session}|{}", data.get("trust_uuid").and_then(Value::as_str).unwrap_or(""));
                    if st.last_pair.as_ref().is_some_and(|(k, t)| *k == key && t.elapsed() < Duration::from_secs(10)) {
                        return;
                    }
                    st.last_pair = Some((key, Instant::now()));
                    st.incoming = Some((ip, Instant::now(), data.clone()));
                }
                log::info!("[cross-screen] pair request from {ip}: {}", data.get("name").and_then(Value::as_str).unwrap_or("?"));
                let automatic = data.get("automatic").and_then(Value::as_bool).unwrap_or(false);
                // A request carrying the trust token we stored for this device is accepted by
                // the main window without asking; only unknown requests get the prompt.
                let trusted = data.get("trust_uuid").and_then(Value::as_str).is_some_and(|t| {
                    let id = data.get("device_id").and_then(Value::as_str).unwrap_or("");
                    commands::connection_history(self).iter().any(|r| r.device_id == id && r.connection_type == "passive" && r.trusted && r.trust_uuid.as_deref() == Some(t))
                });
                let connected = self.st.lock().unwrap().peer.is_some();
                self.notify_rdp("RequestPair", data);
                if !automatic && !trusted && !connected {
                    self.pair_window(true);
                }
            }
            "RequestConfirm" => {
                {
                    let mut st = self.st.lock().unwrap();
                    if st.pairing != Some(ip) {
                        return;
                    }
                    if st.confirm_seen.is_some_and(|(i, t)| i == ip && t.elapsed() < Duration::from_secs(3)) {
                        return;
                    }
                    st.confirm_seen = Some((ip, Instant::now()));
                }
                set_addr(&mut data);
                log::info!("[cross-screen] pair confirmed by {ip}");
                self.notify_rdp("RequestConfirm", data);
            }
            "RequestReject" => {
                {
                    let mut st = self.st.lock().unwrap();
                    if st.pairing != Some(ip) {
                        return;
                    }
                    st.pairing = None;
                }
                set_addr(&mut data);
                log::info!("[cross-screen] pair rejected by {ip}");
                self.notify_rdp("RequestReject", data);
            }
            "PairCancel" => {
                let was = {
                    let mut st = self.st.lock().unwrap();
                    let was = st.incoming.as_ref().is_some_and(|(i, ..)| *i == ip);
                    if was {
                        st.incoming = None;
                    }
                    was
                };
                if was {
                    self.pair_window(false);
                    self.notify_rdp("LocalPairRejected", Value::Null);
                }
            }
            "Disconnect" => {
                if from_peer {
                    self.teardown("peer_disconnect");
                } else {
                    let mut st = self.st.lock().unwrap();
                    if st.pairing == Some(ip) {
                        st.pairing = None;
                    }
                }
            }
            "TrustUpdate" => {
                set_addr(&mut data);
                self.notify_rdp("TrustUpdate", data);
            }
            "ScreenPosition" if from_peer => {
                let dir: Option<RdpDir> = serde_json::from_value(data.clone()).ok();
                self.st.lock().unwrap().dir = dir;
                if dir.is_none() {
                    self.release_mouse();
                }
                self.notify_rdp("ScreenPosition", data);
            }
            "LockScreen" if from_peer => lock_screen(),
            _ => log::debug!("ignored {kind} from {ip}"),
        }
    }

    fn handle_toc(&self, data: Value) {
        let t: Toc = match serde_json::from_value(data) {
            Ok(t) => t,
            Err(e) => {
                log::debug!("bad RequestToC: {e}");
                return;
            }
        };
        match t {
            Toc::Init(x, y) => self.become_controlled((x, y)),
            Toc::Release(x, y) => self.control_returned((x, y)),
            other => {
                if self.st.lock().unwrap().role != Role::Controlled {
                    return;
                }
                self.inj.apply(&other);
                if let Toc::MouseMove(dx, dy) = other {
                    self.after_injected_move(dx, dy);
                }
            }
        }
    }

    pub fn set_shortcut(&self, lock: bool, key: Option<String>) {
        let mut st = self.st.lock().unwrap();
        if lock {
            st.shortcuts.lock_key = key;
        } else {
            st.shortcuts.disconnect_key = key;
        }
    }

    fn on_shortcut(&self, s: capture::Shortcut) {
        match s {
            capture::Shortcut::Disconnect => {
                log::info!("[cross-screen] disconnect shortcut");
                if let Some(ip) = self.peer_ip() {
                    self.send(ip, "Disconnect", json!(""));
                }
                self.teardown("local_disconnect");
            }
            capture::Shortcut::Lock => {
                let controlling = self.st.lock().unwrap().role == Role::Controlling;
                match self.peer_ip() {
                    Some(ip) if controlling => self.send(ip, "LockScreen", json!("")),
                    _ => lock_screen(),
                }
            }
        }
    }
}

struct CapHost(Arc<Rdp>);

impl capture::Host for CapHost {
    fn idle_dir(&self) -> Option<RdpDir> {
        let st = self.0.st.lock().unwrap();
        (st.peer.is_some() && st.role == Role::Idle).then_some(st.dir).flatten()
    }
    fn begin_control(&self, exit: (f32, f32)) -> bool {
        {
            let mut st = self.0.st.lock().unwrap();
            if st.peer.is_none() || st.role != Role::Idle {
                return false;
            }
            st.role = Role::Controlling;
        }
        self.0.send_toc(&Toc::Init(exit.0, exit.1));
        log::info!("[cross-screen] hit the edge, now controlling peer: exit=({}, {})", exit.0, exit.1);
        true
    }
    fn send_toc(&self, t: Toc) {
        self.0.send_toc(&t);
    }
    fn shortcut(&self, s: capture::Shortcut) {
        self.0.on_shortcut(s);
    }
    fn shortcut_keys(&self) -> (Option<String>, Option<String>) {
        let st = self.0.st.lock().unwrap();
        (st.shortcuts.disconnect_key.clone(), st.shortcuts.lock_key.clone())
    }
}

pub fn lock_screen() {
    for (cmd, args) in [("loginctl", &["lock-session"][..]), ("hyprlock", &[][..]), ("xdg-screensaver", &["lock"][..])] {
        if std::process::Command::new(cmd).args(args).spawn().is_ok() {
            return;
        }
    }
}

/// IPv4 broadcast addresses of all up, non-loopback interfaces.
fn broadcast_addrs() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    unsafe {
        let mut ifa: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifa) != 0 {
            return out;
        }
        let mut p = ifa;
        while !p.is_null() {
            let i = &*p;
            let flags = i.ifa_flags as i32;
            if !i.ifa_addr.is_null() && (*i.ifa_addr).sa_family as i32 == libc::AF_INET && flags & libc::IFF_UP != 0 && flags & libc::IFF_LOOPBACK == 0 && flags & libc::IFF_BROADCAST != 0 && !i.ifa_ifu.is_null() {
                let name = std::ffi::CStr::from_ptr(i.ifa_name).to_string_lossy().to_ascii_lowercase();
                // Same idea as the native client: skip virtual/VPN adapters.
                let virt = ["docker", "veth", "br-", "virbr", "vmnet", "vbox", "tailscale", "zt", "tun", "tap", "wg", "bluetooth", "lxc", "podman"];
                if !virt.iter().any(|v| name.starts_with(v)) {
                    let b = &*(i.ifa_ifu as *const libc::sockaddr_in);
                    out.push(Ipv4Addr::from(u32::from_be(b.sin_addr.s_addr)));
                }
            }
            p = i.ifa_next;
        }
        libc::freeifaddrs(ifa);
    }
    out.sort();
    out.dedup();
    out
}

/// Interface name and MAC address owning `ip`.
fn iface_for(ip: &str) -> (String, String) {
    let Ok(ip) = ip.parse::<Ipv4Addr>() else { return (String::new(), String::new()) };
    let mut name = String::new();
    unsafe {
        let mut ifa: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifa) == 0 {
            let mut p = ifa;
            while !p.is_null() {
                let i = &*p;
                if !i.ifa_addr.is_null() && (*i.ifa_addr).sa_family as i32 == libc::AF_INET {
                    let a = &*(i.ifa_addr as *const libc::sockaddr_in);
                    if Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr)) == ip {
                        name = std::ffi::CStr::from_ptr(i.ifa_name).to_string_lossy().into_owned();
                    }
                }
                p = i.ifa_next;
            }
            libc::freeifaddrs(ifa);
        }
    }
    let mac = std::fs::read_to_string(format!("/sys/class/net/{name}/address")).map(|s| s.trim().to_owned()).unwrap_or_default();
    (name, mac)
}

