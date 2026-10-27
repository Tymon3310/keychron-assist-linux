//! Controlling side: watch local keyboards/mice, detect the cursor pushing through the
//! screen edge, then grab the devices and stream their input to the peer.
//! Also implements the global shortcuts (Shift + Esc + key).

use super::{inject::DEVICE_NAME, keymap, screen, wire::*};
use evdev::{Device, EventType, InputEventKind, Key, RelativeAxisType};
use std::{
    collections::{HashMap, HashSet},
    os::fd::AsRawFd,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Disconnect,
    Lock,
}

/// What the capture loop needs from the session.
pub trait Host: Send + Sync + 'static {
    /// Peer side, when connected and neither controlling nor controlled.
    fn idle_dir(&self) -> Option<RdpDir>;
    /// Start controlling the peer; `exit` is our exit point ratio.
    fn begin_control(&self, exit: (f32, f32)) -> bool;
    fn send_toc(&self, t: Toc);
    fn shortcut(&self, s: Shortcut);
    /// Configured shortcut keys ("A", "5", "F1"...).
    fn shortcut_keys(&self) -> (Option<String>, Option<String>);
}

enum Ev {
    Key(u16, i32),
    Rel(u16, i32),
    Syn,
}

pub struct Capture {
    grab: Arc<AtomicBool>,
}

impl Capture {
    pub fn start(host: Arc<dyn Host>) -> Arc<Self> {
        let grab = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<Ev>();
        {
            let grab = grab.clone();
            std::thread::Builder::new()
                .name("rdp-devices".into())
                .spawn(move || device_watcher(tx, grab))
                .unwrap();
        }
        let me = Arc::new(Self { grab });
        {
            let me = me.clone();
            std::thread::Builder::new().name("rdp-capture".into()).spawn(move || me.run(host, rx)).unwrap();
        }
        me
    }

    pub fn forwarding(&self) -> bool {
        self.grab.load(Ordering::SeqCst)
    }

    /// Stop forwarding (peer released control, disconnect, shortcut).
    pub fn stop(&self) {
        self.grab.store(false, Ordering::SeqCst);
    }

    fn run(&self, host: Arc<dyn Host>, rx: mpsc::Receiver<Ev>) {
        let mut held: HashSet<u16> = HashSet::new(); // locally held keys/buttons
        let mut sent: HashSet<u16> = HashSet::new(); // pressed on the peer
        let mut esc_at: Option<Instant> = None;
        let (mut dx, mut dy) = (0i64, 0i64);
        let mut last_edge_check = Instant::now() - Duration::from_secs(1);
        let mut was_forwarding = false;
        for ev in rx {
            let fwd = self.forwarding();
            if was_forwarding && !fwd {
                // Control ended: release on the peer whatever is still down there.
                for k in sent.drain() {
                    if let Some(t) = key_toc(k, false) {
                        host.send_toc(t);
                    }
                }
                dx = 0;
                dy = 0;
            }
            was_forwarding = fwd;
            match ev {
                Ev::Key(code, value) => {
                    let down = value != 0;
                    if value == 1 {
                        held.insert(code);
                    } else if value == 0 {
                        held.remove(&code);
                    }
                    if value == 1 {
                        if code == Key::KEY_ESC.code() {
                            esc_at = Some(Instant::now());
                        } else if let Some(s) = self.match_shortcut(&host, code, &held, esc_at) {
                            host.shortcut(s);
                            continue;
                        }
                    }
                    if fwd {
                        if let Some(t) = key_toc(code, down) {
                            if down {
                                sent.insert(code);
                            } else if !sent.remove(&code) {
                                continue;
                            }
                            host.send_toc(t);
                        }
                    }
                }
                Ev::Rel(axis, v) => {
                    if axis == RelativeAxisType::REL_X.0 {
                        dx += v as i64;
                    } else if axis == RelativeAxisType::REL_Y.0 {
                        dy += v as i64;
                    } else if fwd && axis == RelativeAxisType::REL_WHEEL.0 {
                        host.send_toc(Toc::MouseScroll(0, v));
                    } else if fwd && axis == RelativeAxisType::REL_HWHEEL.0 {
                        host.send_toc(Toc::MouseScroll(v, 0));
                    }
                }
                Ev::Syn => {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    if fwd {
                        host.send_toc(Toc::MouseMove(dx, dy));
                    } else if held.iter().all(|k| !is_button(*k)) && last_edge_check.elapsed() >= Duration::from_millis(4) {
                        last_edge_check = Instant::now();
                        if let Some(dir) = host.idle_dir() {
                            let pushing = match dir {
                                RdpDir::Left => dx < 0,
                                RdpDir::Right => dx > 0,
                                RdpDir::Top => dy < 0,
                                RdpDir::Bottom => dy > 0,
                            };
                            if pushing && held.is_empty() {
                                if let Some((x, y)) = screen::cursor() {
                                    if let Some(exit) = screen::edge_exit(&screen::monitors(), x, y, dir) {
                                        if host.begin_control(exit) {
                                            self.grab.store(true, Ordering::SeqCst);
                                            was_forwarding = true;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    dx = 0;
                    dy = 0;
                }
            }
        }
    }

    fn match_shortcut(&self, host: &Arc<dyn Host>, code: u16, held: &HashSet<u16>, esc_at: Option<Instant>) -> Option<Shortcut> {
        let shift = held.contains(&Key::KEY_LEFTSHIFT.code()) || held.contains(&Key::KEY_RIGHTSHIFT.code());
        let esc = held.contains(&Key::KEY_ESC.code()) || esc_at.is_some_and(|t| t.elapsed() < Duration::from_millis(1500));
        if !shift || !esc {
            return None;
        }
        let (disc, lock) = host.shortcut_keys();
        let key = |s: Option<String>| s.as_deref().and_then(key_from_label);
        if key(disc.or(Some("A".into()))) == Some(code) {
            Some(Shortcut::Disconnect)
        } else if key(lock) == Some(code) {
            Some(Shortcut::Lock)
        } else {
            None
        }
    }
}

fn is_button(code: u16) -> bool {
    (0x110..0x118).contains(&code)
}

fn key_toc(code: u16, down: bool) -> Option<Toc> {
    let state = if down { InputState::Press } else { InputState::Release };
    let button = match code {
        0x110 => Some(MouseAction::Left),
        0x111 => Some(MouseAction::Right),
        0x112 => Some(MouseAction::Middle),
        0x113 => Some(MouseAction::Back),
        0x114 => Some(MouseAction::Forward),
        0x115 => Some(MouseAction::Forward), // BTN_FORWARD
        0x116 => Some(MouseAction::Back),    // BTN_BACK
        _ => None,
    };
    if let Some(b) = button {
        return Some(Toc::MouseClick(b, state));
    }
    keymap::evdev_to_vk(code).map(|vk| Toc::Keyboard(state, vk as i64, GlobalPlat::Windows))
}

/// Shortcut label from the UI ("A".."Z", "0".."9", "F1".."F12") -> evdev key.
pub fn key_from_label(label: &str) -> Option<u16> {
    let l = label.trim().to_ascii_uppercase();
    if let Some(n) = l.strip_prefix('F').and_then(|n| n.parse::<u16>().ok()) {
        return match n {
            1..=10 => Some(58 + n),
            11 => Some(87),
            12 => Some(88),
            _ => None,
        };
    }
    let c = l.chars().next()?;
    if l.len() != 1 {
        return None;
    }
    let vk = c as u16; // VK codes for A-Z / 0-9 equal their ASCII values
    keymap::vk_to_evdev(vk)
}

fn interesting(d: &Device) -> bool {
    if d.name() == Some(DEVICE_NAME) {
        return false;
    }
    let mouse = d.supported_relative_axes().is_some_and(|a| a.contains(RelativeAxisType::REL_X));
    let kbd = d.supported_keys().is_some_and(|k| k.contains(Key::KEY_A) || k.contains(Key::KEY_ESC));
    mouse || kbd
}

/// Opens every keyboard/mouse (and new ones as they appear) with a reader thread each.
fn device_watcher(tx: mpsc::Sender<Ev>, grab: Arc<AtomicBool>) {
    let mut open: HashMap<std::path::PathBuf, Arc<AtomicBool>> = HashMap::new();
    let mut warned = false;
    loop {
        open.retain(|_, alive| alive.load(Ordering::SeqCst));
        let mut any = false;
        for (path, dev) in evdev::enumerate() {
            any = true;
            if open.contains_key(&path) || !interesting(&dev) {
                continue;
            }
            let alive = Arc::new(AtomicBool::new(true));
            open.insert(path.clone(), alive.clone());
            let (tx, grab) = (tx.clone(), grab.clone());
            std::thread::spawn(move || reader(dev, tx, grab, alive));
        }
        if !any && !warned {
            warned = true;
            log::warn!("no readable /dev/input devices (is the user in the `input` group?)");
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

fn reader(mut dev: Device, tx: mpsc::Sender<Ev>, grab: Arc<AtomicBool>, alive: Arc<AtomicBool>) {
    let fd = dev.as_raw_fd();
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    let mut grabbed = false;
    let name = dev.name().unwrap_or("?").to_owned();
    'outer: loop {
        let want = grab.load(Ordering::SeqCst);
        if want != grabbed {
            let r = if want { dev.grab() } else { dev.ungrab() };
            match r {
                Ok(()) => grabbed = want,
                Err(e) => {
                    log::warn!("{} {name}: {e}", if want { "grab" } else { "ungrab" });
                    grabbed = want; // don't retry in a tight loop
                }
            }
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, 15) };
        if n < 0 || pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            break;
        }
        if n == 0 {
            continue;
        }
        match dev.fetch_events() {
            Ok(evs) => {
                for e in evs {
                    let m = match e.kind() {
                        InputEventKind::Key(k) => Ev::Key(k.code(), e.value()),
                        InputEventKind::RelAxis(a) => Ev::Rel(a.0, e.value()),
                        InputEventKind::Synchronization(_) if e.event_type() == EventType::SYNCHRONIZATION => Ev::Syn,
                        _ => continue,
                    };
                    if tx.send(m).is_err() {
                        break 'outer;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => break,
        }
    }
    if grabbed {
        let _ = dev.ungrab();
    }
    alive.store(false, Ordering::SeqCst);
}
