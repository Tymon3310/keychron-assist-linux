//! Screen geometry, cursor position and cursor warping per desktop (Hyprland IPC,
//! KWin script, X11), plus the edge math for crossing between machines. All coordinates are logical layout pixels.

use super::wire::{Monitor, RdpDir, ScreenResolution};
use std::{sync::OnceLock, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mon {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub focused: bool,
}

impl Mon {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

mod hypr;
mod kwin;
mod x11;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Hyprland,
    KWin,
    X11,
    None,
}

static BACKEND: OnceLock<Backend> = OnceLock::new();

/// Pick the desktop backend (override with `KEYCHRON_SCREEN=hyprland|kwin|x11|none`).
pub fn init() -> Backend {
    *BACKEND.get_or_init(|| {
        let forced = std::env::var("KEYCHRON_SCREEN").unwrap_or_default().to_lowercase();
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11_session = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "x11") || (!wayland && std::env::var_os("DISPLAY").is_some());
        let b = match forced.as_str() {
            "hyprland" => Backend::Hyprland,
            "kwin" => Backend::KWin,
            "x11" => Backend::X11,
            "none" => Backend::None,
            _ if hypr::available() => Backend::Hyprland,
            _ if x11_session && x11::available() => Backend::X11,
            _ if kwin::available() => Backend::KWin,
            _ => Backend::None,
        };
        let b = match b {
            Backend::KWin if !kwin::init() => Backend::None,
            Backend::X11 if !x11::available() => Backend::None,
            b => b,
        };
        if b == Backend::None {
            log::warn!("[screen] no supported desktop (Hyprland, KDE Plasma or X11): mouse crossing disabled");
        } else {
            log::info!("[screen] desktop backend: {b:?}");
        }
        b
    })
}

pub fn backend() -> Backend {
    init()
}

pub fn monitors() -> Vec<Mon> {
    match backend() {
        Backend::Hyprland => hypr::monitors(),
        Backend::KWin => kwin::monitors(),
        Backend::X11 => x11::monitors(),
        Backend::None => Vec::new(),
    }
}

pub fn cursor() -> Option<(f64, f64)> {
    match backend() {
        Backend::Hyprland => hypr::cursor(),
        Backend::KWin => kwin::cursor(),
        Backend::X11 => x11::cursor(),
        Backend::None => None,
    }
}

/// Absolute warp, where the desktop allows it. KWin doesn't: see [`warp_relative`].
pub fn warp(x: f64, y: f64) -> bool {
    match backend() {
        Backend::Hyprland => hypr::warp(x, y),
        Backend::X11 => x11::warp(x, y),
        Backend::KWin | Backend::None => false,
    }
}

pub fn can_warp() -> bool {
    matches!(backend(), Backend::Hyprland | Backend::X11)
}

/// Move the cursor to (x, y) by relative steps through `mover`, checking the reported
/// position after each step (pointer acceleration and edge clamping make one step inexact).
pub fn warp_relative(x: f64, y: f64, mover: impl Fn(i64, i64)) -> bool {
    if backend() != Backend::KWin {
        return false;
    }
    for _ in 0..8 {
        let Some((cx, cy)) = kwin::cursor() else { return false };
        let (dx, dy) = ((x - cx).round() as i64, (y - cy).round() as i64);
        if dx.abs() <= 1 && dy.abs() <= 1 {
            return true;
        }
        let seq = kwin::cursor_seq();
        mover(dx, dy);
        if !kwin::wait_cursor(seq, Duration::from_millis(80)) {
            // No movement reported: clamped against an edge, or KWin isn't listening.
            return false;
        }
    }
    false
}

/// Hyprland lowercases device names and replaces spaces with dashes.
fn hypr_device_name(name: &str) -> String {
    name.to_lowercase().replace(' ', "-")
}

/// Flat acceleration at neutral speed for the pointer called `name`, so remote deltas
/// (already accelerated by the sender) are replayed 1:1.
pub fn flat_pointer(name: &str) -> bool {
    match backend() {
        Backend::Hyprland => hypr::flat_pointer(name),
        Backend::KWin => kwin::flat_pointer(name),
        Backend::X11 => x11::flat_pointer(name),
        Backend::None => false,
    }
}

/// Make the pair prompt float, centred and visible on every workspace like the native
/// always-on-top dialog. Blocks for a moment; call from a worker thread.
pub fn float_dialog(title: &str, w: u32, h: u32) {
    match backend() {
        Backend::Hyprland => hypr::float_dialog(title, w, h),
        Backend::KWin => kwin::float_dialog(title),
        // X11 window managers honour GTK's keep-above and sticky hints set by the caller.
        Backend::X11 | Backend::None => {}
    }
}

/// The monitor reported as "the screen": the one at the layout origin, else the first.
fn primary(mons: &[Mon]) -> Option<Mon> {
    mons.iter().find(|m| m.contains(0.0, 0.0)).or(mons.first()).copied()
}

pub fn resolution() -> ScreenResolution {
    primary(&monitors())
        .map(|m| ScreenResolution { width: m.w.round() as u32, height: m.h.round() as u32 })
        .unwrap_or(ScreenResolution { width: 1920, height: 1080 })
}

pub fn monitor_list() -> Vec<Monitor> {
    let mons = monitors();
    let p = primary(&mons);
    mons.iter()
        .enumerate()
        .map(|(i, m)| Monitor { width: m.w.round() as u32, height: m.h.round() as u32, is_primary: Some(*m) == p, index: i })
        .collect()
}

/// Is there another monitor directly beyond `m`'s `dir` edge at position (x, y)?
fn neighbour(mons: &[Mon], m: &Mon, dir: RdpDir, x: f64, y: f64) -> bool {
    let (px, py) = match dir {
        RdpDir::Left => (m.x - 1.0, y),
        RdpDir::Right => (m.x + m.w + 0.5, y),
        RdpDir::Top => (x, m.y - 1.0),
        RdpDir::Bottom => (x, m.y + m.h + 0.5),
    };
    mons.iter().any(|o| o != m && o.contains(px, py))
}

/// If the cursor at (x, y) sits on the outer `dir` edge of the layout, return the exit
/// point as `(x_ratio, y_ratio)` of the monitor it's on (what `Init`/`Release` carry).
pub fn edge_exit(mons: &[Mon], x: f64, y: f64, dir: RdpDir) -> Option<(f32, f32)> {
    let m = mons.iter().find(|m| m.contains(x, y))?;
    let at = match dir {
        RdpDir::Left => x <= m.x + 0.5,
        RdpDir::Right => x >= m.x + m.w - 1.5,
        RdpDir::Top => y <= m.y + 0.5,
        RdpDir::Bottom => y >= m.y + m.h - 1.5,
    };
    if !at || neighbour(mons, m, dir, x, y) {
        return None;
    }
    let rx = ((x - m.x) / m.w).clamp(0.0, 1.0) as f32;
    let ry = ((y - m.y) / m.h).clamp(0.0, 1.0) as f32;
    Some(match dir {
        RdpDir::Left => (0.0, ry),
        RdpDir::Right => (1.0, ry),
        RdpDir::Top => (rx, 0.0),
        RdpDir::Bottom => (rx, 1.0),
    })
}

/// Where to put the cursor when entering through our `dir` edge at the peer's exit
/// ratio. `inset` keeps it off the edge so it doesn't bounce straight back.
pub fn entry_point(mons: &[Mon], dir: RdpDir, ratio: (f32, f32), inset: f64) -> Option<(f64, f64)> {
    let (rx, ry) = (ratio.0.clamp(0.0, 1.0) as f64, ratio.1.clamp(0.0, 1.0) as f64);
    let pick = |key: &dyn Fn(&Mon) -> f64| mons.iter().copied().max_by(|a, b| key(a).total_cmp(&key(b)));
    Some(match dir {
        RdpDir::Left => {
            let m = pick(&|m| -m.x)?;
            (m.x + inset, m.y + ry * (m.h - 1.0))
        }
        RdpDir::Right => {
            let m = pick(&|m| m.x + m.w)?;
            (m.x + m.w - 1.0 - inset, m.y + ry * (m.h - 1.0))
        }
        RdpDir::Top => {
            let m = pick(&|m| -m.y)?;
            (m.x + rx * (m.w - 1.0), m.y + inset)
        }
        RdpDir::Bottom => {
            let m = pick(&|m| m.y + m.h)?;
            (m.x + rx * (m.w - 1.0), m.y + m.h - 1.0 - inset)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The dev box: 2560x1440 at -2560,0 and 1920x1080 at 0,-70.
    fn layout() -> Vec<Mon> {
        vec![
            Mon { x: -2560.0, y: 0.0, w: 2560.0, h: 1440.0, focused: false },
            Mon { x: 0.0, y: -70.0, w: 1920.0, h: 1080.0, focused: true },
        ]
    }

    #[test]
    fn edges_only_on_outer_sides() {
        let l = layout();
        assert_eq!(edge_exit(&l, 1919.0, 470.0, RdpDir::Right), Some((1.0, 0.5)));
        assert_eq!(edge_exit(&l, -2560.0, 720.0, RdpDir::Left), Some((0.0, 0.5)));
        // Inner edge between the two monitors is not an exit.
        assert_eq!(edge_exit(&l, 0.0, 100.0, RdpDir::Left), None);
        assert_eq!(edge_exit(&l, -1.0, 100.0, RdpDir::Right), None);
        // Part of the left monitor's right edge below the right monitor is an exit.
        assert_eq!(edge_exit(&l, -1.0, 1200.0, RdpDir::Right).map(|r| r.0), Some(1.0));
        assert_eq!(edge_exit(&l, 900.0, 470.0, RdpDir::Right), None);
    }

    #[test]
    fn entry_uses_outermost_monitor() {
        let l = layout();
        assert_eq!(entry_point(&l, RdpDir::Right, (0.0, 0.5), 2.0), Some((1917.0, -70.0 + 0.5 * 1079.0)));
        assert_eq!(entry_point(&l, RdpDir::Left, (1.0, 0.0), 2.0), Some((-2558.0, 0.0)));
    }
}
