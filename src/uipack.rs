//! Keychron's interface is not redistributed with this port. On first run it is taken
//! from the official Windows installer (downloaded from Keychron, checked against a
//! pinned hash): zip -> NSIS installer -> the Tauri app -> its embedded web assets.
//! The files land in `~/.local/share/keychron-assist-linux/ui-<version>/` and are
//! served from there (see [`DiskAssets`]).

use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use tauri::{utils::assets::AssetKey, Assets, Runtime};

pub const UI_VERSION: &str = "1.1.6";
pub const DOWNLOAD_URL: &str = "https://launcher.keychron.com/static/assist/KeychronAssist-1.1.6.zip";
/// sha256 of the official `KeychronAssist-1.1.6.zip` and of the installer inside it.
const ZIP_SHA256: &str = "51b39182f5a26df7a3a3f2550ec7b28351d7795e5c973d21e616faa1377f97ab";
const EXE_SHA256: &str = "1573e4b6414b7843356dfb84d394a26cdc03ac7ebb463a9426efab3b45e6c815";

pub fn ui_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("keychron-assist-linux").join(format!("ui-{UI_VERSION}"))
}

pub fn installed() -> bool {
    ui_dir().join("index.html").is_file()
}

/// Progress for the setup page: (phase, done bytes, total bytes, error).
#[derive(Clone, Default, serde::Serialize)]
pub struct Progress {
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub error: Option<String>,
}

pub static PROGRESS: Mutex<Option<Progress>> = Mutex::new(None);

fn set_progress(phase: &str, done: u64, total: u64) {
    *PROGRESS.lock().unwrap() = Some(Progress { phase: phase.into(), done, total, error: None });
}

fn sha256(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// Download the official zip from Keychron.
pub fn download() -> Result<Vec<u8>, String> {
    set_progress("download", 0, 0);
    let mut resp = ureq::get(DOWNLOAD_URL).call().map_err(|e| format!("download failed: {e}"))?;
    let total = resp.body().content_length().unwrap_or(0);
    let mut reader = resp.body_mut().with_config().limit(64 << 20).reader();
    let mut out = Vec::with_capacity(total as usize);
    let mut buf = [0u8; 64 << 10];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("download failed: {e}"))?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        set_progress("download", out.len() as u64, total);
    }
    Ok(out)
}

/// Install from the official zip or the installer `.exe` inside it.
pub fn install(file: &[u8]) -> Result<usize, String> {
    set_progress("extract", 0, 0);
    let exe: Cow<[u8]> = if file.starts_with(b"PK") {
        if sha256(file) != ZIP_SHA256 {
            return Err(format!("this is not the official KeychronAssist-{UI_VERSION}.zip (hash mismatch)"));
        }
        let mut z = zip::ZipArchive::new(Cursor::new(file)).map_err(|e| format!("zip: {e}"))?;
        let name = format!("KeychronAssist-{UI_VERSION}.exe");
        let mut f = z.by_name(&name).map_err(|e| format!("zip: {name}: {e}"))?;
        let mut v = Vec::new();
        f.read_to_end(&mut v).map_err(|e| format!("zip: {e}"))?;
        Cow::Owned(v)
    } else {
        Cow::Borrowed(file)
    };
    if sha256(&exe) != EXE_SHA256 {
        return Err(format!("this is not the official KeychronAssist-{UI_VERSION}.exe (hash mismatch)"));
    }
    let app = nsis_largest_exe(&exe)?;
    let assets = tauri_assets(&app)?;
    if !assets.iter().any(|(k, _)| k == "/index.html") {
        return Err("no index.html among the embedded assets".into());
    }
    let dir = ui_dir();
    let tmp = dir.with_extension("tmp");
    let _ = std::fs::remove_dir_all(&tmp);
    for (key, data) in &assets {
        let rel = safe_rel(key).ok_or_else(|| format!("bad asset path {key}"))?;
        let p = tmp.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, data).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir).map_err(|e| e.to_string())?;
    set_progress("done", assets.len() as u64, assets.len() as u64);
    log::info!("[setup] installed {} interface files into {}", assets.len(), dir.display());
    Ok(assets.len())
}

/// `/a/b.js` -> `a/b.js`, refusing anything that could leave the folder.
fn safe_rel(key: &str) -> Option<PathBuf> {
    let p = Path::new(key.trim_start_matches('/'));
    p.components().all(|c| matches!(c, Component::Normal(_))).then(|| p.to_path_buf())
}

/// NSIS installer (solid LZMA): return the largest PE file packed inside, the app.
fn nsis_largest_exe(exe: &[u8]) -> Result<Vec<u8>, String> {
    let sig = find(exe, b"NullsoftInst").ok_or("not an NSIS installer")?;
    let fh = sig.checked_sub(8).ok_or("bad NSIS header")?;
    let data = exe.get(fh + 28..).ok_or("bad NSIS header")?;
    if data.len() < 5 || data[0] != 0x5d {
        return Err("unsupported NSIS compression (expected solid LZMA)".into());
    }
    // Raw LZMA stream: give it a .lzma header with unknown size and read until the input ends.
    let mut stream = Vec::with_capacity(data.len() + 8);
    stream.extend_from_slice(&data[..5]);
    stream.extend_from_slice(&u64::MAX.to_le_bytes());
    stream.extend_from_slice(&data[5..]);
    let mut out = Vec::new();
    let opts = lzma_rs::decompress::Options { allow_incomplete: true, ..Default::default() };
    // NSIS appends data after the stream's end marker, which lzma-rs reports as an error
    // once the output is already complete.
    if let Err(e) = lzma_rs::lzma_decompress_with_options(&mut Cursor::new(stream), &mut out, &opts) {
        if out.is_empty() || !e.to_string().contains("end-of-stream marker") {
            return Err(format!("lzma: {e}"));
        }
    }
    // Blocks: [u32 len][data]...; the first is the NSIS script header.
    let (mut off, mut best): (usize, &[u8]) = (0, &[]);
    while off + 4 <= out.len() {
        let len = (u32::from_le_bytes(out[off..off + 4].try_into().unwrap()) & 0x7fff_ffff) as usize;
        let Some(blk) = out.get(off + 4..off + 4 + len) else { break };
        if blk.starts_with(b"MZ") && blk.len() > best.len() {
            best = blk;
        }
        off += 4 + len;
    }
    if best.is_empty() {
        return Err("no program found in the installer".into());
    }
    Ok(best.to_vec())
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// Tauri embeds assets as a table of `(&str path, &[u8] brotli data)` pairs, 32 bytes
/// each on x86-64. Find the table through the `/index.html` key and walk it.
fn tauri_assets(pe: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let rd32 = |o: usize| pe.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let rd64 = |o: usize| pe.get(o..o + 8).map(|b| u64::from_le_bytes(b.try_into().unwrap()));
    let e = rd32(0x3c).ok_or("bad PE")? as usize;
    let nsec = pe.get(e + 6..e + 8).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or("bad PE")? as usize;
    let optsz = pe.get(e + 20..e + 22).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or("bad PE")? as usize;
    let base = rd64(e + 24 + 24).ok_or("bad PE")?;
    // (virtual address, raw size, raw offset)
    let secs: Vec<(u64, u64, u64)> = (0..nsec)
        .filter_map(|i| {
            let s = e + 24 + optsz + 40 * i;
            Some((rd32(s + 12)? as u64, rd32(s + 16)? as u64, rd32(s + 20)? as u64))
        })
        .collect();
    let va2f = |v: u64| secs.iter().find(|(va, rs, _)| v >= base + va && v < base + va + rs).map(|(va, _, ra)| (ra + v - base - va) as usize);
    let f2va = |o: usize| secs.iter().find(|(_, rs, ra)| (o as u64) >= *ra && (o as u64) < ra + rs).map(|(va, _, ra)| base + va + o as u64 - ra);
    let entry = |o: usize| -> Option<(String, &[u8])> {
        let (kp, kl, dp, dl) = (rd64(o)?, rd64(o + 8)? as usize, rd64(o + 16)?, rd64(o + 24)? as usize);
        if kl == 0 || kl > 512 || dl == 0 || dl > 64 << 20 {
            return None;
        }
        let k = std::str::from_utf8(pe.get(va2f(kp)?..va2f(kp)? + kl)?).ok()?;
        let d = pe.get(va2f(dp)?..va2f(dp)? + dl)?;
        k.starts_with('/').then(|| (k.to_owned(), d))
    };
    let key = b"/index.html";
    let mut at = 0;
    while let Some(i) = find(&pe[at..], key).map(|i| i + at) {
        at = i + 1;
        let Some(va) = f2va(i) else { continue };
        let mut r = Vec::with_capacity(16);
        r.extend_from_slice(&va.to_le_bytes());
        r.extend_from_slice(&(key.len() as u64).to_le_bytes());
        let Some(mut j) = find(pe, &r) else { continue };
        while j >= 32 && entry(j - 32).is_some() {
            j -= 32;
        }
        let mut out = Vec::new();
        while let Some((k, d)) = entry(j) {
            let mut plain = Vec::new();
            brotli_decompressor::BrotliDecompress(&mut Cursor::new(d), &mut plain).map_err(|e| format!("{k}: {e}"))?;
            out.push((k, plain));
            j += 32;
        }
        if !out.is_empty() {
            return Ok(out);
        }
    }
    Err("no embedded web assets found".into())
}

/// Serves the installed interface from disk, falling back to the built-in setup page.
pub struct DiskAssets<R: Runtime> {
    pub fallback: Box<dyn Assets<R>>,
}

impl<R: Runtime> Assets<R> for DiskAssets<R> {
    fn get(&self, key: &AssetKey) -> Option<Cow<'_, [u8]>> {
        if installed() {
            if let Some(rel) = safe_rel(key.as_ref()) {
                if let Ok(b) = std::fs::read(ui_dir().join(rel)) {
                    return Some(Cow::Owned(b));
                }
            }
        }
        self.fallback.get(key)
    }

    fn iter(&self) -> Box<tauri::utils::assets::AssetsIter<'_>> {
        self.fallback.iter()
    }

    fn csp_hashes(&self, html_path: &AssetKey) -> Box<dyn Iterator<Item = tauri::utils::assets::CspHash<'_>> + '_> {
        self.fallback.csp_hashes(html_path)
    }
}

/// Placeholder used while swapping the asset provider.
pub struct NoAssets;

impl<R: Runtime> Assets<R> for NoAssets {
    fn get(&self, _: &AssetKey) -> Option<Cow<'_, [u8]>> {
        None
    }

    fn iter(&self) -> Box<tauri::utils::assets::AssetsIter<'_>> {
        Box::new(std::iter::empty())
    }

    fn csp_hashes(&self, _: &AssetKey) -> Box<dyn Iterator<Item = tauri::utils::assets::CspHash<'_>> + '_> {
        Box::new(std::iter::empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_escaping_paths() {
        assert_eq!(safe_rel("/assets/a.js"), Some(PathBuf::from("assets/a.js")));
        assert_eq!(safe_rel("/../etc/passwd"), None);
        assert_eq!(safe_rel("/a/./b"), Some(PathBuf::from("a/b")));
    }
}

// ---------- setup page commands ----------

#[tauri::command]
pub fn ui_setup_status() -> serde_json::Value {
    serde_json::json!({
        "installed": installed(),
        "version": UI_VERSION,
        "url": DOWNLOAD_URL,
        "dir": ui_dir(),
        "progress": PROGRESS.lock().unwrap().clone(),
    })
}

fn finish(app: &tauri::AppHandle, r: Result<usize, String>) -> Result<(), String> {
    use tauri::Manager;
    match r {
        Ok(_) => {
            for w in app.webview_windows().values() {
                let _ = w.eval("location.reload()");
            }
            Ok(())
        }
        Err(e) => {
            log::warn!("[setup] {e}");
            if let Some(p) = PROGRESS.lock().unwrap().as_mut() {
                p.error = Some(e.clone());
            }
            Err(e)
        }
    }
}

#[tauri::command]
pub async fn ui_setup_download(app: tauri::AppHandle) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(|| download().and_then(|b| install(&b))).await.map_err(|e| e.to_string())?;
    finish(&app, r)
}

#[tauri::command]
pub async fn ui_setup_from_file(app: tauri::AppHandle) -> Result<(), String> {
    let r = tauri::async_runtime::spawn_blocking(|| {
        let Some(path) = rfd::FileDialog::new().add_filter("Keychron Assist for Windows", &["zip", "exe"]).pick_file() else {
            return Err("no file selected".to_string());
        };
        let b = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        install(&b)
    })
    .await
    .map_err(|e| e.to_string())?;
    finish(&app, r)
}
