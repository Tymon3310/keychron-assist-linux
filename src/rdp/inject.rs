//! Controlled side: replay the peer's input through a uinput device.

use super::{keymap, screen, wire::*};
use evdev::{uinput::VirtualDevice, uinput::VirtualDeviceBuilder, AttributeSet, EventType, InputEvent, Key, RelativeAxisType};
use std::collections::HashSet;
use std::sync::Mutex;

pub const DEVICE_NAME: &str = "Keychron Assist Virtual Input";

pub struct Injector {
    dev: Mutex<Option<VirtualDevice>>,
    held: Mutex<HashSet<u16>>,
}

fn build() -> std::io::Result<VirtualDevice> {
    let mut keys = AttributeSet::<Key>::new();
    for c in 1u16..=255 {
        keys.insert(Key::new(c));
    }
    keys.insert(Key::KEY_FN);
    for c in 0x110u16..=0x117 {
        keys.insert(Key::new(c));
    }
    let mut rel = AttributeSet::<RelativeAxisType>::new();
    for a in [
        RelativeAxisType::REL_X,
        RelativeAxisType::REL_Y,
        RelativeAxisType::REL_WHEEL,
        RelativeAxisType::REL_HWHEEL,
        RelativeAxisType::REL_WHEEL_HI_RES,
        RelativeAxisType::REL_HWHEEL_HI_RES,
    ] {
        rel.insert(a);
    }
    VirtualDeviceBuilder::new()?
        .name(DEVICE_NAME)
        .with_keys(&keys)?
        .with_relative_axes(&rel)?
        .build()
}

fn button(b: MouseAction) -> u16 {
    match b {
        MouseAction::Left => 0x110,
        MouseAction::Right => 0x111,
        MouseAction::Middle => 0x112,
        MouseAction::Back => 0x113,    // BTN_SIDE
        MouseAction::Forward => 0x114, // BTN_EXTRA
    }
}

impl Injector {
    pub fn new() -> Self {
        let dev = match build() {
            Ok(d) => {
                // Remote deltas are already accelerated by the sender; replay them 1:1.
                std::thread::spawn(|| {
                    for _ in 0..20 {
                        std::thread::sleep(std::time::Duration::from_millis(250));
                        if screen::flat_pointer(DEVICE_NAME) {
                            log::info!("virtual pointer set to flat acceleration");
                            break;
                        }
                    }
                });
                Some(d)
            }
            Err(e) => {
                log::warn!("uinput unavailable, remote input disabled: {e}");
                None
            }
        };
        Self { dev: Mutex::new(dev), held: Mutex::new(HashSet::new()) }
    }

    pub fn available(&self) -> bool {
        self.dev.lock().unwrap().is_some()
    }

    fn emit(&self, ev: &[InputEvent]) {
        if let Some(d) = self.dev.lock().unwrap().as_mut() {
            if let Err(e) = d.emit(ev) {
                log::warn!("uinput emit: {e}");
            }
        }
    }

    fn key(&self, code: u16, down: bool) {
        {
            let mut held = self.held.lock().unwrap();
            if down {
                held.insert(code);
            } else if !held.remove(&code) {
                // Release of a key we never pressed (e.g. pressed before control arrived): ignore.
                return;
            }
        }
        self.emit(&[InputEvent::new(EventType::KEY, code, down as i32)]);
    }

    pub fn apply(&self, t: &Toc) {
        match *t {
            Toc::MouseMove(dx, dy) => {
                let c = |v: i64| v.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
                self.emit(&[
                    InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_X.0, c(dx)),
                    InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_Y.0, c(dy)),
                ]);
            }
            Toc::MouseClick(b, s) => self.key(button(b), s == InputState::Press),
            Toc::MouseScroll(h, v) => {
                let mut ev = Vec::new();
                if v != 0 {
                    ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL.0, v));
                    ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_WHEEL_HI_RES.0, v * 120));
                }
                if h != 0 {
                    ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL.0, h));
                    ev.push(InputEvent::new(EventType::RELATIVE, RelativeAxisType::REL_HWHEEL_HI_RES.0, h * 120));
                }
                if !ev.is_empty() {
                    self.emit(&ev);
                }
            }
            Toc::Keyboard(s, code, plat) => match keymap::wire_to_evdev(code, plat) {
                Some(k) => self.key(k, s == InputState::Press),
                None => log::debug!("unmapped key {code} ({plat:?})"),
            },
            Toc::Init(..) | Toc::Release(..) => {}
        }
    }

    /// Release everything still held (control handed back, disconnect).
    pub fn release_all(&self) {
        let held: Vec<u16> = self.held.lock().unwrap().drain().collect();
        if held.is_empty() {
            return;
        }
        let ev: Vec<_> = held.iter().map(|k| InputEvent::new(EventType::KEY, *k, 0)).collect();
        self.emit(&ev);
    }
}
