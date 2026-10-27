//! X11 sessions (any window manager): RandR monitors, QueryPointer, WarpPointer.

use super::Mon;
use std::sync::{Mutex, OnceLock};
use x11rb::{connection::Connection, protocol::randr::ConnectionExt as _, protocol::xproto::ConnectionExt as _, rust_connection::RustConnection};

struct X {
    conn: RustConnection,
    root: u32,
}

fn x() -> Option<&'static Mutex<X>> {
    static X: OnceLock<Option<Mutex<X>>> = OnceLock::new();
    X.get_or_init(|| {
        let (conn, n) = x11rb::connect(None).map_err(|e| log::warn!("X11 connect: {e}")).ok()?;
        let root = conn.setup().roots.get(n)?.root;
        Some(Mutex::new(X { conn, root }))
    })
    .as_ref()
}

pub fn available() -> bool {
    x().is_some()
}

pub fn monitors() -> Vec<Mon> {
    let Some(x) = x() else { return Vec::new() };
    let x = x.lock().unwrap();
    let Some(reply) = x.conn.randr_get_monitors(x.root, true).ok().and_then(|c| c.reply().ok()) else {
        // No RandR 1.5: treat the root window as one screen.
        let s = &x.conn.setup().roots[0];
        return vec![Mon { x: 0.0, y: 0.0, w: s.width_in_pixels as f64, h: s.height_in_pixels as f64, focused: true }];
    };
    reply
        .monitors
        .iter()
        .map(|m| Mon { x: m.x as f64, y: m.y as f64, w: m.width as f64, h: m.height as f64, focused: m.primary })
        .collect()
}

pub fn cursor() -> Option<(f64, f64)> {
    let x = x()?.lock().unwrap();
    let r = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    Some((r.root_x as f64, r.root_y as f64))
}

pub fn warp(px: f64, py: f64) -> bool {
    let Some(x) = x() else { return false };
    let x = x.lock().unwrap();
    let ok = x.conn.warp_pointer(x11rb::NONE, x.root, 0, 0, 0, 0, px.round() as i16, py.round() as i16).is_ok();
    ok && x.conn.flush().is_ok()
}

/// Best effort through `xinput`: libinput drivers expose the profile as a property.
pub fn flat_pointer(name: &str) -> bool {
    let set = |vals: &[&str]| {
        std::process::Command::new("xinput")
            .args(["set-prop", &format!("pointer:{name}"), "libinput Accel Profile Enabled"])
            .args(vals)
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    // Newer libinput has three profiles (adaptive, flat, custom), older two.
    let ok = set(&["0", "1", "0"]) || set(&["0", "1"]);
    if ok {
        let _ = std::process::Command::new("xinput").args(["set-prop", &format!("pointer:{name}"), "libinput Accel Speed", "0"]).status();
    }
    ok
}
