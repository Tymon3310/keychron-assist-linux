//! KDE Plasma (KWin, Wayland or X11). Wayland gives apps no cursor position or warping,
//! so a small KWin script pushes the cursor position and screen layout to us over D-Bus.
//! Warping is done by the caller with relative moves through the virtual pointer
//! (see `Rdp::warp`), checked against the reported position.

use super::Mon;
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use zbus::blocking::{connection, Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const BUS_NAME: &str = "io.github.tymon3310.KeychronAssistLinux";
const PATH: &str = "/KWin";
const PLUGIN: &str = "keychron-assist-cursor";
const DIALOG_PLUGIN: &str = "keychron-assist-dialog";

#[derive(Default)]
struct Shared {
    cursor: Option<(f64, f64)>,
    seq: u64,
    mons: Vec<Mon>,
}

static SHARED: Mutex<Shared> = Mutex::new(Shared { cursor: None, seq: 0, mons: Vec::new() });
static CHANGED: Condvar = Condvar::new();

struct Sink;

#[zbus::interface(name = "io.github.tymon3310.KeychronAssistLinux.KWin")]
impl Sink {
    /// "x,y" in logical layout pixels.
    #[zbus(name = "cursor")]
    fn cursor(&self, pos: String) {
        let Some((x, y)) = pos.split_once(',') else { return };
        let (Ok(x), Ok(y)) = (x.trim().parse(), y.trim().parse()) else { return };
        let mut s = SHARED.lock().unwrap();
        s.cursor = Some((x, y));
        s.seq += 1;
        CHANGED.notify_all();
    }

    /// JSON list of `{x,y,w,h}` output geometries.
    #[zbus(name = "screens")]
    fn screens(&self, json: String) {
        let Ok(list) = serde_json::from_str::<Vec<serde_json::Value>>(&json) else { return };
        let mons: Vec<Mon> = list
            .iter()
            .filter_map(|m| Some(Mon { x: m["x"].as_f64()?, y: m["y"].as_f64()?, w: m["w"].as_f64()?, h: m["h"].as_f64()?, focused: m["active"].as_bool().unwrap_or(false) }))
            .collect();
        log::info!("[screen] KWin outputs: {}", mons.iter().map(|m| format!("{}x{}@{},{}", m.w, m.h, m.x, m.y)).collect::<Vec<_>>().join(" "));
        SHARED.lock().unwrap().mons = mons;
    }
}

const SCRIPT: &str = r#"
function send(m, a) { callDBus("io.github.tymon3310.KeychronAssistLinux", "/KWin", "io.github.tymon3310.KeychronAssistLinux.KWin", m, a); }
function screens() {
    var out = [];
    var list = workspace.screens;
    for (var i = 0; i < list.length; i++) {
        var g = list[i].geometry;
        out.push({ x: g.x, y: g.y, w: g.width, h: g.height, active: list[i] === workspace.activeScreen });
    }
    send("screens", JSON.stringify(out));
}
function cursor() { var p = workspace.cursorPos; send("cursor", p.x + "," + p.y); }
workspace.cursorPosChanged.connect(cursor);
workspace.screensChanged.connect(screens);
screens();
cursor();
"#;

/// Session bus connection that owns `io.github.tymon3310.KeychronAssistLinux` and serves the script's callbacks.
fn conn() -> Option<&'static Connection> {
    static CONN: OnceLock<Option<Connection>> = OnceLock::new();
    CONN.get_or_init(|| {
        connection::Builder::session()
            .and_then(|b| b.name(BUS_NAME))
            .and_then(|b| b.serve_at(PATH, Sink))
            .and_then(|b| b.build())
            .map_err(|e| log::warn!("[screen] D-Bus: {e}"))
            .ok()
    })
    .as_ref()
}

/// Is KWin on the session bus?
pub fn available() -> bool {
    let Ok(c) = Connection::session() else { return false };
    let Ok(p) = Proxy::new(&c, "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus") else { return false };
    p.call::<_, _, bool>("NameHasOwner", &("org.kde.KWin",)).unwrap_or(false)
}

/// Load a JS file into KWin under `plugin` (replacing an older copy) and run it.
fn run_script(c: &Connection, plugin: &str, source: &str) -> bool {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let path = dir.join(format!("{plugin}.js"));
    if std::fs::write(&path, source).is_err() {
        return false;
    }
    let Ok(s) = Proxy::new(c, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting") else { return false };
    let _ = s.call::<_, _, bool>("unloadScript", &(plugin,));
    let id: i32 = match s.call("loadScript", &(path.to_string_lossy().as_ref(), plugin)) {
        Ok(id) => id,
        Err(e) => {
            log::warn!("[screen] KWin loadScript: {e}");
            return false;
        }
    };
    // Plasma 6 path first, then Plasma 5.
    for p in [format!("/Scripting/Script{id}"), format!("/{id}")] {
        if let Ok(script) = Proxy::new(c, "org.kde.KWin", p, "org.kde.kwin.Script") {
            if script.call::<_, _, ()>("run", &()).is_ok() {
                return true;
            }
        }
    }
    log::warn!("[screen] KWin script {plugin} did not start");
    false
}

pub fn init() -> bool {
    let Some(c) = conn() else { return false };
    if !run_script(c, PLUGIN, SCRIPT) {
        return false;
    }
    // Wait briefly for the first report so the layout is known right away.
    let s = SHARED.lock().unwrap();
    let _ = CHANGED.wait_timeout_while(s, Duration::from_millis(500), |s| s.cursor.is_none());
    true
}

pub fn monitors() -> Vec<Mon> {
    SHARED.lock().unwrap().mons.clone()
}

pub fn cursor() -> Option<(f64, f64)> {
    SHARED.lock().unwrap().cursor
}

pub fn cursor_seq() -> u64 {
    SHARED.lock().unwrap().seq
}

/// Wait until KWin reports a cursor position newer than `seq`.
pub fn wait_cursor(seq: u64, timeout: Duration) -> bool {
    let end = Instant::now() + timeout;
    let mut s = SHARED.lock().unwrap();
    while s.seq <= seq {
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        s = CHANGED.wait_timeout(s, left).unwrap().0;
    }
    true
}

fn prop(c: &Connection, path: &str, iface: &str, name: &str) -> Option<OwnedValue> {
    let p = Proxy::new(c, "org.kde.KWin", path.to_owned(), "org.freedesktop.DBus.Properties").ok()?;
    p.call("Get", &(iface, name)).ok()
}

fn set_prop(c: &Connection, path: &str, iface: &str, name: &str, v: Value<'_>) -> bool {
    let Ok(p) = Proxy::new(c, "org.kde.KWin", path.to_owned(), "org.freedesktop.DBus.Properties") else { return false };
    p.call::<_, _, ()>("Set", &(iface, name, v)).is_ok()
}

/// Flat acceleration at neutral speed for the input device called `name`.
pub fn flat_pointer(name: &str) -> bool {
    let Some(c) = conn() else { return false };
    let mgr = |p| prop(c, p, "org.kde.KWin.InputDeviceManager", "devicesSysNames");
    let Some(list) = mgr("/org/kde/KWin/InputDevice").or_else(|| mgr("/org/kde/KWin/InputDeviceManager")) else { return false };
    let Ok(list) = <Vec<String>>::try_from(list) else { return false };
    let mut found = false;
    for sys in list {
        let path = format!("/org/kde/KWin/InputDevice/{sys}");
        let iface = "org.kde.KWin.InputDevice";
        let is_ours = prop(c, &path, iface, "name").and_then(|v| String::try_from(v).ok()).is_some_and(|n| n == name);
        let is_pointer = prop(c, &path, iface, "pointer").and_then(|v| bool::try_from(v).ok()).unwrap_or(false);
        if is_ours && is_pointer {
            found = set_prop(c, &path, iface, "pointerAccelerationProfileFlat", Value::from(true))
                && set_prop(c, &path, iface, "pointerAcceleration", Value::from(0.0f64));
        }
    }
    found
}

/// KWin places dialogs itself; keep the prompt above other windows and on every desktop.
pub fn float_dialog(title: &str) {
    let Some(c) = conn() else { return };
    let title = serde_json::to_string(title).unwrap_or_default();
    let src = format!(
        r#"
function fix(w) {{ if (w.caption !== {title}) return; w.keepAbove = true; w.onAllDesktops = true; workspace.activeWindow = w; }}
var list = workspace.windowList();
for (var i = 0; i < list.length; i++) fix(list[i]);
workspace.windowAdded.connect(fix);
"#
    );
    if run_script(c, DIALOG_PLUGIN, &src) {
        std::thread::sleep(Duration::from_secs(3));
        if let Ok(s) = Proxy::new(c, "org.kde.KWin", "/Scripting", "org.kde.kwin.Scripting") {
            let _ = s.call::<_, _, bool>("unloadScript", &(DIALOG_PLUGIN,));
        }
    }
}
