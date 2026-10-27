//! File transfer over the session channel (see PROTOCOL.md "File transfer").

use super::{unix_ms, wire::*, Rdp};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub id: String,
    pub name: String,
    pub path: String,
    pub directory_path: String,
    pub direction: String,
    pub status: String,
    pub size_bytes: u64,
    pub transferred_bytes: u64,
    pub average_speed_bytes_per_second: u64,
    pub started_at: u64,
    #[serde(default)]
    pub completed_at: Option<u64>,
    pub elapsed_ms: u64,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub device_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

enum Reply {
    Ack(u64, u64, Vec<u64>),
    Missing(Vec<u64>),
    Result(bool, Option<String>),
    Cancel,
}

struct Incoming {
    peer: IpAddr,
    name: String,
    file: File,
    tmp: PathBuf,
    size: u64,
    chunks: u64,
    got: BTreeSet<u64>,
    started: Instant,
    started_at: u64,
    last_progress: Instant,
}

pub struct Files {
    pub receive_dir: Mutex<PathBuf>,
    incoming: Mutex<HashMap<String, Incoming>>,
    /// Results of finished receives, re-sent if the sender repeats `FileEnd`.
    done: Mutex<HashMap<String, (bool, Option<String>, Instant)>>,
    outgoing: Mutex<HashMap<String, mpsc::Sender<Reply>>>,
    queue: Mutex<()>,
}

pub fn load_receive_dir(root: &Path) -> PathBuf {
    std::fs::read_to_string(root.join("rdp_file_folder"))
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| p.is_dir())
        .or_else(dirs::download_dir)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Downloads"))
}

impl Files {
    pub fn new(receive_dir: PathBuf) -> Self {
        Self {
            receive_dir: Mutex::new(receive_dir),
            incoming: Mutex::new(HashMap::new()),
            done: Mutex::new(HashMap::new()),
            outgoing: Mutex::new(HashMap::new()),
            queue: Mutex::new(()),
        }
    }
}

fn speed(bytes: u64, el: Duration) -> u64 {
    let s = el.as_secs_f64();
    if s > 0.0 { (bytes as f64 / s) as u64 } else { 0 }
}

#[allow(clippy::too_many_arguments)]
fn event(rdp: &Rdp, id: &str, name: &str, path: &str, dir: &str, status: &str, size: u64, done: u64, started: Instant, error: Option<&str>) {
    let el = started.elapsed();
    let mut v = json!({
        "average_speed_bytes_per_second": speed(done, el), "direction": dir, "elapsed_ms": el.as_millis() as u64,
        "error": error, "id": id, "name": name, "path": path, "size_bytes": size, "status": status, "transferred_bytes": done,
    });
    if status == "progress" {
        v["instantaneous_speed_bytes_per_second"] = json!(speed(done, el));
    }
    rdp.notify("FileTransfer", v);
}

pub fn history_path(rdp: &Rdp) -> PathBuf {
    rdp.root.join("file_transfer_history.json")
}

pub fn read_history(rdp: &Rdp) -> Vec<HistoryRecord> {
    std::fs::read(history_path(rdp)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn write_history(rdp: &Rdp, list: &[HistoryRecord]) {
    if let Ok(b) = serde_json::to_vec_pretty(list) {
        let tmp = history_path(rdp).with_extension("tmp");
        if std::fs::write(&tmp, b).is_ok() {
            let _ = std::fs::rename(tmp, history_path(rdp));
        }
    }
}

fn record(rdp: &Rdp, r: HistoryRecord) {
    let mut list = read_history(rdp);
    list.retain(|x| x.id != r.id);
    list.insert(0, r);
    list.truncate(500);
    write_history(rdp, &list);
}

#[allow(clippy::too_many_arguments)]
fn history(rdp: &Rdp, id: &str, name: &str, path: &str, dir: &str, status: &str, size: u64, done: u64, started: Instant, started_at: u64, error: Option<&str>) {
    let (device_id, device_name) = rdp.peer().map(|p| (p.device_id, p.name)).unwrap_or_default();
    let directory_path = if path.is_empty() { String::new() } else { Path::new(path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default() };
    record(rdp, HistoryRecord {
        id: id.into(),
        name: name.into(),
        path: path.into(),
        directory_path,
        direction: dir.into(),
        status: status.into(),
        size_bytes: size,
        transferred_bytes: done,
        average_speed_bytes_per_second: speed(done, started.elapsed()),
        started_at,
        completed_at: Some(unix_ms()),
        elapsed_ms: started.elapsed().as_millis() as u64,
        device_name,
        device_id,
        error: error.map(Into::into),
    });
}

/// "name.ext" -> "name (1).ext" until free (same scheme as the native client).
fn free_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (name.to_owned(), String::new()),
    };
    (1..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|p| !p.exists()).unwrap()
}

fn safe_name(n: &str) -> String {
    let base = n.rsplit(['/', '\\']).next().unwrap_or(n).trim();
    let base: String = base.chars().filter(|c| !c.is_control()).collect();
    if base.is_empty() || base == "." || base == ".." { "received-file".into() } else { base }
}

// ---------------- receiving ----------------

pub fn handle(rdp: &Rdp, ip: IpAddr, from_peer: bool, kind: &str, d: Value) {
    let a = d.as_array().cloned().unwrap_or_default();
    let s = |i: usize| a.get(i).and_then(Value::as_str).unwrap_or("").to_owned();
    let u = |i: usize| a.get(i).and_then(Value::as_u64).unwrap_or(0);
    let list = |i: usize| a.get(i).and_then(Value::as_array).map(|l| l.iter().filter_map(Value::as_u64).collect::<Vec<_>>()).unwrap_or_default();
    let reply = |id: &str, r: Reply| {
        if let Some(tx) = rdp.files.outgoing.lock().unwrap().get(id) {
            let _ = tx.send(r);
        }
    };
    match kind {
        // replies to our sends
        "FileBatchAck" => reply(&s(0), Reply::Ack(u(1), u(2), list(3))),
        "FileMissing" => reply(&s(0), Reply::Missing(list(1))),
        "FileResult" => reply(&s(0), Reply::Result(a.get(1).and_then(Value::as_bool).unwrap_or(false), a.get(2).and_then(Value::as_str).map(Into::into))),
        "FileCancel" => {
            let id = d.as_str().map(Into::into).unwrap_or_else(|| s(0));
            reply(&id, Reply::Cancel);
            receive_cancel(rdp, &id);
        }
        _ if !from_peer => {}
        "FileStart" => receive_start(rdp, ip, &s(0), &s(1), u(2), u(3)),
        "FileData" => receive_chunk(rdp, &s(0), u(1), &s(2)),
        "FileBatchEnd" => {
            let id = s(0);
            let (start, end) = (u(1), u(2));
            let missing = match rdp.files.incoming.lock().unwrap().get_mut(&id) {
                Some(f) => {
                    let m: Vec<u64> = (start..end.min(f.chunks)).filter(|i| !f.got.contains(i)).collect();
                    if m.is_empty() && f.last_progress.elapsed() > Duration::from_millis(50) {
                        f.last_progress = Instant::now();
                        let done = (f.got.len() as u64 * FILE_CHUNK as u64).min(f.size);
                        event(rdp, &id, &f.name, "", "receive", "progress", f.size, done, f.started, None);
                    }
                    m
                }
                None => (start..end).collect(),
            };
            rdp.send(ip, "FileBatchAck", json!([id, start, end, missing]));
        }
        "FileEnd" => {
            let id = d.as_str().map(Into::into).unwrap_or_else(|| s(0));
            receive_end(rdp, ip, &id);
        }
        "FileName" => log::debug!("FileName {d}"),
        _ => {}
    }
}

fn receive_start(rdp: &Rdp, ip: IpAddr, id: &str, name: &str, chunks: u64, size: u64) {
    if id.is_empty() || rdp.files.incoming.lock().unwrap().contains_key(id) || rdp.files.done.lock().unwrap().contains_key(id) {
        return; // FileStart is repeated
    }
    if !rdp.st.lock().unwrap().file_enabled {
        rdp.send(ip, "FileResult", json!([id, false, "file_transfer_disabled"]));
        return;
    }
    let name = safe_name(name);
    let dir = rdp.files.receive_dir.lock().unwrap().clone();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!(".{name}.{id}.part"));
    let file = match File::options().create(true).truncate(true).read(true).write(true).open(&tmp) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("receive {name}: {e}");
            rdp.send(ip, "FileResult", json!([id, false, e.to_string()]));
            return;
        }
    };
    let _ = file.set_len(size);
    log::info!("[file] receiving {name} ({size} bytes, {chunks} chunks) from {ip}");
    let f = Incoming { peer: ip, name: name.clone(), file, tmp, size, chunks, got: BTreeSet::new(), started: Instant::now(), started_at: unix_ms(), last_progress: Instant::now() };
    event(rdp, id, &name, "", "receive", "started", size, 0, f.started, None);
    rdp.files.incoming.lock().unwrap().insert(id.into(), f);
}

fn receive_chunk(rdp: &Rdp, id: &str, idx: u64, b64: &str) {
    let mut map = rdp.files.incoming.lock().unwrap();
    let Some(f) = map.get_mut(id) else { return };
    if idx >= f.chunks || f.got.contains(&idx) {
        return;
    }
    let Ok(bytes) = B64.decode(b64) else { return };
    let off = idx * FILE_CHUNK as u64;
    if off + bytes.len() as u64 > f.size {
        return;
    }
    if f.file.seek(SeekFrom::Start(off)).and_then(|_| f.file.write_all(&bytes)).is_ok() {
        f.got.insert(idx);
    }
}

fn receive_end(rdp: &Rdp, ip: IpAddr, id: &str) {
    if let Some((ok, err, _)) = rdp.files.done.lock().unwrap().get(id).cloned() {
        rdp.send(ip, "FileResult", json!([id, ok, err]));
        return;
    }
    let missing: Vec<u64> = {
        let map = rdp.files.incoming.lock().unwrap();
        let Some(f) = map.get(id) else { return };
        (0..f.chunks).filter(|i| !f.got.contains(i)).collect()
    };
    if !missing.is_empty() {
        rdp.send(ip, "FileMissing", json!([id, missing]));
        return;
    }
    let Some(mut f) = rdp.files.incoming.lock().unwrap().remove(id) else { return };
    let dir = f.tmp.parent().unwrap().to_path_buf();
    let dest = free_path(&dir, &f.name);
    let res = f.file.flush().and_then(|_| std::fs::rename(&f.tmp, &dest));
    let (ok, err) = match &res {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };
    rdp.files.done.lock().unwrap().insert(id.into(), (ok, err.clone(), Instant::now()));
    rdp.files.done.lock().unwrap().retain(|_, v| v.2.elapsed() < Duration::from_secs(120));
    rdp.send(ip, "FileResult", json!([id, ok, err]));
    let path = dest.to_string_lossy().into_owned();
    if ok {
        log::info!("[file] received {}", path);
        event(rdp, id, &f.name, &path, "receive", "completed", f.size, f.size, f.started, None);
        history(rdp, id, &f.name, &path, "receive", "received", f.size, f.size, f.started, f.started_at, None);
    } else {
        let _ = std::fs::remove_file(&f.tmp);
        event(rdp, id, &f.name, "", "receive", "failed", f.size, 0, f.started, err.as_deref());
        history(rdp, id, &f.name, "", "receive", "receive-error", f.size, 0, f.started, f.started_at, err.as_deref());
    }
    let _ = f.peer;
}

fn receive_cancel(rdp: &Rdp, id: &str) {
    let Some(f) = rdp.files.incoming.lock().unwrap().remove(id) else { return };
    let _ = std::fs::remove_file(&f.tmp);
    let done = (f.got.len() as u64 * FILE_CHUNK as u64).min(f.size);
    event(rdp, id, &f.name, "", "receive", "cancelled", f.size, done, f.started, Some("peer_cancelled"));
    history(rdp, id, &f.name, "", "receive", "cancelled", f.size, done, f.started, f.started_at, Some("peer_cancelled"));
}

/// Session ended: fail everything in flight.
pub fn terminate_for_disconnect(rdp: &Rdp) {
    let ids: Vec<String> = rdp.files.incoming.lock().unwrap().keys().cloned().collect();
    for id in ids {
        if let Some(f) = rdp.files.incoming.lock().unwrap().remove(&id) {
            let _ = std::fs::remove_file(&f.tmp);
            let done = (f.got.len() as u64 * FILE_CHUNK as u64).min(f.size);
            event(rdp, &id, &f.name, "", "receive", "cancelled", f.size, done, f.started, Some("disconnected"));
            history(rdp, &id, &f.name, "", "receive", "cancelled", f.size, done, f.started, f.started_at, Some("disconnected"));
        }
    }
    for tx in rdp.files.outgoing.lock().unwrap().values() {
        let _ = tx.send(Reply::Cancel);
    }
}

// ---------------- sending ----------------

/// User cancelled one of our transfers (either direction).
pub fn cancel(rdp: &Rdp, id: &str) -> bool {
    let out = rdp.files.outgoing.lock().unwrap().get(id).cloned();
    let peer = rdp.peer_ip();
    if let Some(ip) = peer {
        for _ in 0..3 {
            rdp.send(ip, "FileCancel", json!(id));
        }
    }
    if let Some(tx) = out {
        let _ = tx.send(Reply::Cancel);
        return true;
    }
    if let Some(f) = rdp.files.incoming.lock().unwrap().remove(id) {
        let _ = std::fs::remove_file(&f.tmp);
        let done = (f.got.len() as u64 * FILE_CHUNK as u64).min(f.size);
        event(rdp, id, &f.name, "", "receive", "cancelled", f.size, done, f.started, None);
        history(rdp, id, &f.name, "", "receive", "cancelled", f.size, done, f.started, f.started_at, None);
        return true;
    }
    false
}

pub fn send_file(rdp: Arc<Rdp>, id: String, path: String, name: String, record_id: Option<String>) {
    let (tx, rx) = mpsc::channel();
    rdp.files.outgoing.lock().unwrap().insert(id.clone(), tx);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    event(&rdp, &id, &name, &path, "send", "started", size, 0, Instant::now(), None);
    std::thread::spawn(move || {
        let _q = rdp.files.queue.lock().unwrap(); // one file at a time
        let started = Instant::now();
        let started_at = unix_ms();
        let res = run_send(&rdp, &id, &path, &name, record_id.as_deref().unwrap_or(&id), size, &rx, started);
        rdp.files.outgoing.lock().unwrap().remove(&id);
        match res {
            Ok(()) => {
                log::info!("[file] sent {name}");
                event(&rdp, &id, &name, &path, "send", "completed", size, size, started, None);
                history(&rdp, &id, &name, &path, "send", "sent", size, size, started, started_at, None);
            }
            Err((status, err, done)) => {
                log::info!("[file] send {name} {status}: {err:?}");
                event(&rdp, &id, &name, &path, "send", status, size, done, started, err.as_deref());
                let hs = if status == "cancelled" { "cancelled" } else { "failed" };
                history(&rdp, &id, &name, &path, "send", hs, size, done, started, started_at, err.as_deref());
            }
        }
    });
}

type SendErr = (&'static str, Option<String>, u64);

#[allow(clippy::too_many_arguments)]
fn run_send(rdp: &Rdp, id: &str, path: &str, name: &str, record_id: &str, size: u64, rx: &mpsc::Receiver<Reply>, started: Instant) -> Result<(), SendErr> {
    let ip = rdp.peer_ip().ok_or(("failed", Some("not_connected".into()), 0))?;
    if !rdp.st.lock().unwrap().peer_file_enabled {
        return Err(("failed", Some("peer_file_transfer_disabled".into()), 0));
    }
    let mut file = File::open(path).map_err(|_| ("failed", Some("file_missing".into()), 0))?;
    let chunks = size.div_ceil(FILE_CHUNK as u64).max(if size == 0 { 0 } else { 1 });
    let read = |file: &mut File, i: u64| -> Option<String> {
        let mut buf = vec![0u8; FILE_CHUNK];
        file.seek(SeekFrom::Start(i * FILE_CHUNK as u64)).ok()?;
        let mut n = 0;
        while n < FILE_CHUNK {
            match file.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(_) => return None,
            }
        }
        buf.truncate(n);
        Some(B64.encode(&buf))
    };
    let start_msg = json!([id, name, chunks, size, record_id]);
    for _ in 0..3 {
        rdp.send(ip, "FileStart", start_msg.clone());
    }
    let mut acked = 0u64; // chunks acknowledged
    let mut last_event = Instant::now();
    let batches: Vec<(u64, u64)> = (0..chunks).step_by(FILE_BATCH as usize).map(|s| (s, (s + FILE_BATCH).min(chunks))).collect();
    let mut pending: BTreeSet<usize> = BTreeSet::new(); // sent, not yet acked
    let mut next = 0usize;
    let mut last_rx = Instant::now();
    let mut backoff = Duration::from_millis(400);
    const WINDOW: usize = 8;
    let send_batch = |file: &mut File, s: u64, e: u64, only: Option<&[u64]>| -> Result<(), SendErr> {
        let idx: Vec<u64> = only.map(|o| o.to_vec()).unwrap_or_else(|| (s..e).collect());
        for (k, i) in idx.iter().enumerate() {
            let data = read(file, *i).ok_or(("failed", Some("read_error".into()), 0))?;
            rdp.send(ip, "FileData", json!([id, i, data]));
            if k % 16 == 15 {
                std::thread::sleep(Duration::from_micros(300));
            }
        }
        rdp.send(ip, "FileBatchEnd", json!([id, s, e]));
        Ok(())
    };
    while acked < chunks {
        while pending.len() < WINDOW && next < batches.len() {
            let (s, e) = batches[next];
            send_batch(&mut file, s, e, None)?;
            pending.insert(next);
            next += 1;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Reply::Cancel) => return Err(("cancelled", None, acked * FILE_CHUNK as u64)),
            Ok(Reply::Ack(s, _e, missing)) => {
                last_rx = Instant::now();
                backoff = Duration::from_millis(400);
                let Some(b) = batches.iter().position(|x| x.0 == s) else { continue };
                if !pending.contains(&b) {
                    continue;
                }
                if missing.is_empty() {
                    pending.remove(&b);
                    acked += batches[b].1 - batches[b].0;
                    if last_event.elapsed() > Duration::from_millis(100) || acked == chunks {
                        last_event = Instant::now();
                        let done = (acked * FILE_CHUNK as u64).min(size);
                        event(rdp, id, name, path, "send", "progress", size, done, started, None);
                    }
                } else {
                    send_batch(&mut file, batches[b].0, batches[b].1, Some(&missing))?;
                }
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if rdp.peer_ip() != Some(ip) {
                    return Err(("failed", Some("disconnected".into()), acked * FILE_CHUNK as u64));
                }
                if last_rx.elapsed() > backoff {
                    // No acks: re-announce and re-request acks for everything in flight.
                    last_rx = Instant::now();
                    backoff = (backoff * 2).min(Duration::from_secs(3));
                    rdp.send(ip, "FileStart", start_msg.clone());
                    for b in &pending {
                        rdp.send(ip, "FileBatchEnd", json!([id, batches[*b].0, batches[*b].1]));
                    }
                }
                if started.elapsed() > Duration::from_secs(30) && acked == 0 {
                    return Err(("failed", Some("timeout".into()), 0));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(("failed", None, 0)),
        }
    }
    // All batches acknowledged: finish.
    let mut tries = 0;
    loop {
        rdp.send(ip, "FileEnd", json!(id));
        let deadline = Instant::now() + Duration::from_secs(1);
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(Reply::Result(true, _)) => return Ok(()),
                Ok(Reply::Result(false, e)) => return Err(("failed", e, size)),
                Ok(Reply::Cancel) => return Err(("cancelled", None, size)),
                Ok(Reply::Missing(m)) => {
                    for i in m {
                        if let Some(d) = read(&mut file, i) {
                            rdp.send(ip, "FileData", json!([id, i, d]));
                        }
                    }
                    break;
                }
                Ok(Reply::Ack(..)) => {}
                Err(_) => break,
            }
        }
        tries += 1;
        if tries > 15 || rdp.peer_ip() != Some(ip) {
            return Err(("failed", Some("timeout".into()), size));
        }
    }
}
