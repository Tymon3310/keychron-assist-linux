//! Tauri commands for the cross-screen page (names and arguments match the shared UI).

use super::{files, wire::*, Peer, Rdp};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::IpAddr, path::PathBuf, sync::Arc};
use tauri::{Manager, State};

type R<'a> = State<'a, Arc<Rdp>>;

fn ip(addr: &str) -> Result<IpAddr, String> {
    let a = addr.trim();
    a.parse::<IpAddr>()
        .or_else(|_| a.parse::<std::net::SocketAddr>().map(|s| s.ip()))
        .map_err(|_| format!("invalid address: {addr}"))
}

fn dir_from(v: &Value) -> Option<RdpDir> {
    serde_json::from_value(v.clone()).ok()
}

#[tauri::command]
pub fn switch_rdp_scan(flag: bool, rdp: R) -> bool {
    rdp.st.lock().unwrap().scanning = flag;
    true
}

#[tauri::command]
pub fn get_local_device_info(rdp: R) -> Value {
    rdp.device_info()
}

#[tauri::command]
pub fn validate_port_status() -> Value {
    // Ports are bound at startup; the app doesn't start without them.
    let tcp = std::net::TcpStream::connect_timeout(&"127.0.0.1:40003".parse().unwrap(), std::time::Duration::from_millis(200)).is_ok();
    json!({"discover_port": true, "global_udp_channel": true, "global_ws_channel": tcp})
}

#[tauri::command]
pub fn rdp_connect(addr: String, trust_uuid: Option<String>, automatic: Option<bool>, dir: Option<Value>, rdp: R) -> Result<bool, String> {
    let ip = ip(&addr)?;
    let local_dir = dir.as_ref().and_then(dir_from);
    {
        let mut st = rdp.st.lock().unwrap();
        if st.peer.is_some() || st.incoming.as_ref().is_some_and(|(i, ..)| *i == ip) {
            return Ok(false);
        }
        st.pairing = Some(ip);
        st.confirm_seen = None;
        if dir.is_some() {
            st.dir = local_dir;
        }
    }
    // The peer is told where *we* are from its point of view.
    let info = rdp.pair_info(local_dir.unwrap_or(RdpDir::Right).opposite(), trust_uuid, automatic.unwrap_or(false));
    log::info!("[cross-screen] requesting pair with {ip}");
    for _ in 0..3 {
        rdp.send(ip, "RequestPair", serde_json::to_value(&info).unwrap());
    }
    Ok(true)
}

#[tauri::command]
pub fn rdp_cancel_pairing(addr: String, rdp: R) -> Result<bool, String> {
    let ip = ip(&addr)?;
    let pending = {
        let mut st = rdp.st.lock().unwrap();
        let p = st.pairing == Some(ip);
        if p {
            st.pairing = None;
        }
        p
    };
    if pending {
        rdp.send(ip, "PairCancel", json!(""));
        rdp.send(ip, "Disconnect", json!(""));
    }
    Ok(pending)
}

#[tauri::command]
pub fn rdp_complete_connection(addr: String, device_id: Option<String>, device_name: Option<String>, session_id: Option<String>, rdp: R) -> Result<(), String> {
    let ip = ip(&addr)?;
    rdp.connect_peer(Peer {
        ip,
        device_id: device_id.unwrap_or_default(),
        name: device_name.unwrap_or_default(),
        os_info: String::new(),
        session_id: session_id.unwrap_or_default(),
    });
    Ok(())
}

fn accept(rdp: &Rdp, ip: IpAddr, dir: Option<RdpDir>, trust_uuid: Option<String>, pair: &Value) {
    let s = |k: &str| pair.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let confirm = serde_json::to_value(rdp.confirm_info(trust_uuid)).unwrap();
    for _ in 0..3 {
        rdp.send(ip, "RequestConfirm", confirm.clone());
    }
    rdp.connect_peer(Peer { ip, device_id: s("device_id"), name: s("name"), os_info: s("os_info"), session_id: s("session_id") });
    rdp.st.lock().unwrap().dir = dir;
}

/// Accept from the pair-confirm window.
#[tauri::command]
pub fn rdp_confirm_dialog(pair: Value, trusted: Option<bool>, trust_uuid: Option<String>, rdp: R) -> Result<bool, String> {
    let ip = ip(pair.get("addr").and_then(Value::as_str).unwrap_or(""))?;
    let dir = pair.get("dir").and_then(dir_from);
    accept(&rdp, ip, dir, trust_uuid.clone(), &pair);
    rdp.pair_window(false);
    rdp.notify_rdp("LocalPairAccepted", json!({"pair": pair, "trusted": trusted.unwrap_or(false), "trust_uuid": trust_uuid}));
    Ok(true)
}

/// Accept from the main window (automatic connections).
#[tauri::command]
pub fn rdp_confirm(addr: String, dir: Option<Value>, trust_uuid: Option<String>, device_id: Option<String>, device_name: Option<String>, session_id: Option<String>, rdp: R) -> Result<bool, String> {
    let ip = ip(&addr)?;
    let pair = rdp.st.lock().unwrap().incoming.as_ref().filter(|(i, ..)| *i == ip).map(|(_, _, p)| p.clone());
    let mut pair = pair.unwrap_or_else(|| json!({}));
    for (k, v) in [("device_id", device_id), ("name", device_name), ("session_id", session_id)] {
        if let Some(v) = v {
            pair[k] = json!(v);
        }
    }
    accept(&rdp, ip, dir.as_ref().and_then(dir_from), trust_uuid, &pair);
    Ok(true)
}

fn reject(rdp: &Rdp, ip: IpAddr, trust_uuid: Option<String>) {
    {
        let mut st = rdp.st.lock().unwrap();
        if st.incoming.as_ref().is_some_and(|(i, ..)| *i == ip) {
            st.incoming = None;
        }
    }
    let info = serde_json::to_value(RejectInfo { addr: String::new(), trust_uuid, reason: None }).unwrap();
    for _ in 0..3 {
        rdp.send(ip, "RequestReject", info.clone());
    }
}

#[tauri::command]
pub fn rdp_reject(addr: String, trust_uuid: Option<String>, rdp: R) -> Result<bool, String> {
    reject(&rdp, ip(&addr)?, trust_uuid);
    Ok(true)
}

#[tauri::command]
pub fn rdp_reject_dialog(pair: Value, rdp: R) -> Result<bool, String> {
    let ip = ip(pair.get("addr").and_then(Value::as_str).unwrap_or(""))?;
    let trust = pair.get("trust_uuid").and_then(Value::as_str).map(Into::into);
    reject(&rdp, ip, trust);
    rdp.pair_window(false);
    rdp.notify_rdp("LocalPairRejected", Value::Null);
    Ok(true)
}

#[tauri::command]
pub fn rdp_disconnect(rdp: R) -> bool {
    if let Some(ip) = rdp.peer_ip() {
        rdp.send(ip, "Disconnect", json!(""));
    }
    rdp.teardown("local_disconnect");
    true
}

#[tauri::command]
pub fn rdp_reset_stale_pairing_state(rdp: R) -> bool {
    let mut st = rdp.st.lock().unwrap();
    st.pairing = None;
    st.incoming = None;
    drop(st);
    rdp.pair_window(false);
    true
}

/// When both sides want to auto-connect, only one of them may start (the one with the
/// smaller device id), otherwise both requests would collide.
#[tauri::command]
pub fn rdp_should_initiate_automatic(addr: String, rdp: R) -> bool {
    let Ok(ip) = ip(&addr) else { return false };
    let peer_id = connection_history(&rdp).into_iter().find(|r| r.addr == ip.to_string()).map(|r| r.device_id);
    peer_id.is_some_and(|p| rdp.local.device_id < p)
}

#[tauri::command]
pub fn rdp_trust_current_peer(trust_uuid: String, rdp: R) -> bool {
    match rdp.peer_ip() {
        Some(ip) => {
            rdp.send(ip, "TrustUpdate", serde_json::to_value(TrustInfo { addr: String::new(), trust_uuid }).unwrap());
            true
        }
        None => false,
    }
}

#[tauri::command]
pub fn sync_rdp_web_state(state: Value, app: tauri::AppHandle) {
    // Mirror of the page state (used by the tray); keep the last copy.
    if let Some(s) = app.try_state::<WebState>() {
        *s.0.lock().unwrap() = state;
    }
}

pub struct WebState(pub std::sync::Mutex<Value>);

#[tauri::command]
pub fn rdp_update_screen_position(dir: Value, rdp: R) -> bool {
    let d = dir_from(&dir);
    rdp.st.lock().unwrap().dir = d;
    if d.is_none() {
        rdp.release_mouse();
    }
    if let Some(ip) = rdp.peer_ip() {
        let mirrored = d.map(|d| json!(d.opposite())).unwrap_or(json!("Off"));
        rdp.send(ip, "ScreenPosition", mirrored);
    }
    true
}

#[tauri::command]
pub fn rdp_release_mouse(rdp: R) -> bool {
    rdp.release_mouse();
    true
}

#[tauri::command]
pub fn set_rdp_disconnect_shortcut(key: Option<String>, rdp: R) -> bool {
    rdp.set_shortcut(false, key.filter(|k| !k.is_empty()));
    true
}

#[tauri::command]
pub fn set_system_lock_shortcut(key: Option<String>, rdp: R) -> bool {
    rdp.set_shortcut(true, key.filter(|k| !k.is_empty()));
    true
}

// ---------------- files ----------------

#[tauri::command]
pub fn switch_file_transfer(v: bool, rdp: R) {
    rdp.st.lock().unwrap().file_enabled = v;
    if let Some(ip) = rdp.peer_ip() {
        rdp.send(ip, "FileTransferSwitch", json!(v));
    }
}

#[tauri::command]
pub async fn select_transfer_files(max_files: Option<usize>) -> Result<Value, String> {
    let max = max_files.unwrap_or(10);
    let picked = tauri::async_runtime::spawn_blocking(|| rfd::FileDialog::new().pick_files().unwrap_or_default()).await.map_err(|e| e.to_string())?;
    let truncated = picked.len() > max;
    let files: Vec<Value> = picked
        .into_iter()
        .filter(|p| p.is_file())
        .take(max)
        .map(|p| json!({"name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "path": p.to_string_lossy()}))
        .collect();
    Ok(json!({"files": files, "truncated": truncated}))
}

#[tauri::command]
pub fn send_file_by_path(id: String, path: String, name: String, history_record_id: Option<String>, rdp: R) -> Result<(), String> {
    if rdp.peer_ip().is_none() {
        return Err("not_connected".into());
    }
    files::send_file(rdp.inner().clone(), id, path, name, history_record_id);
    Ok(())
}

#[tauri::command]
pub fn cancel_file_transfer(id: String, rdp: R) -> bool {
    files::cancel(&rdp, &id)
}

#[tauri::command]
pub fn transfer_file_exists(path: String) -> bool {
    PathBuf::from(path).is_file()
}

fn open(p: &std::path::Path) -> Result<bool, String> {
    std::process::Command::new("xdg-open").arg(p).spawn().map(|_| true).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn open_rdp_file_folder(rdp: R) -> Result<bool, String> {
    let d = rdp.files.receive_dir.lock().unwrap().clone();
    let _ = std::fs::create_dir_all(&d);
    open(&d)
}

#[tauri::command]
pub fn set_rdp_file_folder(path: String, rdp: R) -> Result<bool, String> {
    let p = PathBuf::from(&path);
    std::fs::create_dir_all(&p).map_err(|e| e.to_string())?;
    std::fs::write(rdp.root.join("rdp_file_folder"), &path).map_err(|e| e.to_string())?;
    *rdp.files.receive_dir.lock().unwrap() = p;
    Ok(true)
}

#[tauri::command]
pub fn reveal_file_transfer_file(path: String) -> Result<bool, String> {
    let p = PathBuf::from(&path);
    // Prefer a file manager that can select the file; fall back to opening its folder.
    let uri = format!("file://{}", p.display());
    let ok = std::process::Command::new("dbus-send")
        .args(["--session", "--dest=org.freedesktop.FileManager1", "--type=method_call", "/org/freedesktop/FileManager1", "org.freedesktop.FileManager1.ShowItems", &format!("array:string:{uri}"), "string:"])
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        return Ok(true);
    }
    open(p.parent().ok_or("no parent")?)
}

#[tauri::command]
pub fn get_file_transfer_history(device_id: Option<String>, rdp: R) -> Vec<files::HistoryRecord> {
    files::read_history(&rdp).into_iter().filter(|r| device_id.as_deref().is_none_or(|d| d.is_empty() || r.device_id == d)).collect()
}

#[tauri::command]
pub fn record_file_transfer_history(record: files::HistoryRecord, rdp: R) -> bool {
    let mut l = files::read_history(&rdp);
    l.retain(|r| r.id != record.id);
    l.insert(0, record);
    files::write_history(&rdp, &l);
    true
}

#[tauri::command]
pub fn remove_file_transfer_history_record(device_id: Option<String>, record_id: String, rdp: R) -> Vec<files::HistoryRecord> {
    let mut l = files::read_history(&rdp);
    l.retain(|r| r.id != record_id);
    files::write_history(&rdp, &l);
    l.into_iter().filter(|r| device_id.as_deref().is_none_or(|d| r.device_id == d)).collect()
}

#[tauri::command]
pub fn clear_file_transfer_history(device_id: Option<String>, rdp: R) -> Vec<files::HistoryRecord> {
    let mut l = files::read_history(&rdp);
    match device_id.as_deref() {
        Some(d) if !d.is_empty() => l.retain(|r| r.device_id != d),
        _ => l.clear(),
    }
    files::write_history(&rdp, &l);
    Vec::new()
}

// ---------------- connection history ----------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryDevice {
    pub device_id: String,
    pub name: String,
    pub addr: String,
    pub os_info: String,
    pub screen_resolution: ScreenResolution,
    #[serde(default)]
    pub last_time: u64,
    #[serde(default)]
    pub trusted: bool,
    #[serde(default)]
    pub trust_uuid: Option<String>,
    #[serde(default)]
    pub remember: bool,
    #[serde(default)]
    pub connection_type: String,
}

fn history_path(rdp: &Rdp) -> PathBuf {
    rdp.root.join("rdp_connection_history.json")
}

pub fn connection_history(rdp: &Rdp) -> Vec<HistoryDevice> {
    std::fs::read(history_path(rdp)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save_connection_history(rdp: &Rdp, l: &[HistoryDevice]) {
    if let Ok(b) = serde_json::to_vec_pretty(l) {
        let _ = std::fs::write(history_path(rdp), b);
    }
}

#[tauri::command]
pub fn get_rdp_connection_history(rdp: R) -> Vec<HistoryDevice> {
    connection_history(&rdp)
}

#[tauri::command]
pub fn record_rdp_connection_history(mut device: HistoryDevice, rdp: R) -> Vec<HistoryDevice> {
    device.last_time = super::unix_ms();
    if device.connection_type.is_empty() {
        device.connection_type = "active".into();
    }
    let mut l = connection_history(&rdp);
    if let Some(p) = rdp.st.lock().unwrap().peer.as_mut() {
        if p.device_id == device.device_id {
            p.os_info = device.os_info.clone();
            p.name = device.name.clone();
        }
    }
    l.retain(|r| !(r.device_id == device.device_id && r.connection_type == device.connection_type));
    l.insert(0, device);
    save_connection_history(&rdp, &l);
    l
}

#[tauri::command]
pub fn clear_rdp_connection_history(rdp: R) -> Vec<HistoryDevice> {
    save_connection_history(&rdp, &[]);
    Vec::new()
}

#[tauri::command]
pub fn remove_rdp_trusted_device(device_id: String, rdp: R) -> Vec<HistoryDevice> {
    let mut l = connection_history(&rdp);
    for r in l.iter_mut().filter(|r| r.device_id == device_id) {
        r.trusted = false;
        r.trust_uuid = None;
    }
    save_connection_history(&rdp, &l);
    l
}

#[tauri::command]
pub fn read_rdp_automatic_connection_device(rdp: R) -> Option<String> {
    std::fs::read(rdp.root.join("rdp_settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.get("automatic_connection_device_id").and_then(Value::as_str).map(Into::into))
}

#[tauri::command]
pub fn write_rdp_automatic_connection_device(device_id: Option<String>, rdp: R) -> Result<bool, String> {
    let v = json!({"automatic_connection_device_id": device_id.filter(|d| !d.is_empty())});
    std::fs::write(rdp.root.join("rdp_settings.json"), serde_json::to_vec_pretty(&v).unwrap()).map_err(|e| e.to_string())?;
    Ok(true)
}

