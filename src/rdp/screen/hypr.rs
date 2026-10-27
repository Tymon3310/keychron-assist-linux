//! Hyprland: geometry, cursor and warping over the IPC socket. Hyprland 0.56+ takes Lua
//! (`hl.dsp...`), older builds the classic `dispatch name args` syntax.

use super::Mon;
use std::{io::Read, io::Write, os::unix::net::UnixStream, path::PathBuf, sync::OnceLock, time::Duration};

fn socket() -> Option<PathBuf> {
    let rt = std::env::var_os("XDG_RUNTIME_DIR")?;
    let sig = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    Some(PathBuf::from(rt).join("hypr").join(sig).join(".socket.sock"))
}

pub fn available() -> bool {
    socket().is_some_and(|p| p.exists())
}

fn hypr(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket()?).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(300))).ok()?;
    s.write_all(cmd.as_bytes()).ok()?;
    let mut out = String::new();
    s.read_to_string(&mut out).ok()?;
    Some(out)
}

fn lua() -> bool {
    static LUA: OnceLock<bool> = OnceLock::new();
    *LUA.get_or_init(|| hypr("eval return 1").is_some_and(|r| r.trim() == "ok"))
}

fn ok(r: Option<String>) -> bool {
    r.is_some_and(|r| r.trim() == "ok")
}

pub fn monitors() -> Vec<Mon> {
    let Some(text) = hypr("j/monitors") else { return Vec::new() };
    let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else { return Vec::new() };
    list.iter()
        .filter(|m| !m["disabled"].as_bool().unwrap_or(false))
        .filter_map(|m| {
            let scale = m["scale"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
            let (mut w, mut h) = (m["width"].as_f64()? / scale, m["height"].as_f64()? / scale);
            // transform 1/3/5/7 rotate by 90°
            if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
                std::mem::swap(&mut w, &mut h);
            }
            Some(Mon { x: m["x"].as_f64()?, y: m["y"].as_f64()?, w, h, focused: m["focused"].as_bool().unwrap_or(false) })
        })
        .collect()
}

pub fn cursor() -> Option<(f64, f64)> {
    let reply = hypr("cursorpos")?;
    let (x, y) = reply.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

pub fn warp(x: f64, y: f64) -> bool {
    let (x, y) = (x.round() as i64, y.round() as i64);
    if lua() {
        ok(hypr(&format!("dispatch hl.dsp.cursor.move({{ x = {x}, y = {y} }})")))
    } else {
        ok(hypr(&format!("dispatch movecursor {x} {y}")))
    }
}

pub fn flat_pointer(name: &str) -> bool {
    let prefix = super::hypr_device_name(name);
    let Some(devs) = hypr("j/devices") else { return false };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&devs) else { return false };
    let names: Vec<String> = v["mice"].as_array().into_iter().flatten().filter_map(|m| m["name"].as_str()).filter(|n| n.starts_with(&prefix)).map(Into::into).collect();
    let mut all = !names.is_empty();
    for n in names {
        all &= if lua() {
            ok(hypr(&format!("eval hl.device({{ name = \"{n}\", accel_profile = \"flat\", sensitivity = 0 }})")))
        } else {
            ok(hypr(&format!("keyword device[{n}]:accel_profile flat"))) && ok(hypr(&format!("keyword device[{n}]:sensitivity 0")))
        };
    }
    all
}

pub fn float_dialog(title: &str, w: u32, h: u32) {
    let sel = format!("title:^({title})$");
    for _ in 0..40 {
        if hypr("j/clients").is_some_and(|c| c.contains(&format!("\"title\": \"{title}\""))) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if lua() {
        let win = format!("window = \"{sel}\"");
        for d in [
            format!("hl.dsp.window.float({{ action = \"on\", {win} }})"),
            format!("hl.dsp.window.resize({{ x = {w}, y = {h}, {win} }})"),
            format!("hl.dsp.window.center({{ {win} }})"),
            format!("hl.dsp.window.pin({{ action = \"on\", {win} }})"),
        ] {
            let _ = hypr(&format!("dispatch {d}"));
        }
    } else {
        let _ = hypr(&format!("[[BATCH]]dispatch setfloating {sel};dispatch resizewindowpixel exact {w} {h},{sel};dispatch focuswindow {sel};dispatch centerwindow;dispatch pin {sel}"));
    }
}
