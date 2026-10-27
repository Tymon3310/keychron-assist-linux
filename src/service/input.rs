use crate::{linux::devices::qk_cr_slot, state::AppState};
use serde_json::Value;
use std::{collections::HashSet, process::Command, sync::Arc, time::Duration};
use tauri::{AppHandle, Emitter};

pub fn start(state: Arc<AppState>, app: AppHandle) {
    std::thread::spawn(move || {
        let mut active = HashSet::new();
        loop {
            for (path, mut device) in evdev::enumerate() {
                let path = path.to_string_lossy().into_owned();
                if active.contains(&path) {
                    continue;
                }
                let id = device.input_id();
                let name = device.name().unwrap_or_default().to_ascii_lowercase();
                if id.vendor() != 0x3434 && !name.contains("keychron") && !name.contains("lemokey")
                {
                    continue;
                }
                let has_consumer_slots = device
                    .supported_keys()
                    .is_some_and(|keys| keys.iter().any(|k| qk_cr_slot(k.0).is_some()));
                if !has_consumer_slots {
                    continue;
                }
                active.insert(path.clone());
                let vpid = format!("{}:{}", id.vendor(), id.product());
                let state = state.clone();
                let app = app.clone();
                std::thread::spawn(move || loop {
                    match device.fetch_events() {
                        Ok(events) => {
                            for event in events {
                                if event.event_type() != evdev::EventType::KEY || event.value() != 1
                                {
                                    continue;
                                }
                                let Some(slot) = qk_cr_slot(event.code()) else {
                                    continue;
                                };
                                let state = state.clone();
                                let app = app.clone();
                                let vpid = vpid.clone();
                                tauri::async_runtime::spawn(async move {
                                    handle_key(state, app, vpid, slot).await;
                                });
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10))
                        }
                        Err(e) => {
                            eprintln!("[input] {path}: {e}");
                            break;
                        }
                    }
                });
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    });
}

async fn handle_key(state: Arc<AppState>, app: AppHandle, vpid: String, key: String) {
    // While forwarding input to the peer, local bindings stay silent.
    if crate::rdp::forwarding() {
        return;
    }
    let cfg = state.config.read().await;
    let actual_vpid = cfg
        .quick_start
        .receiver_maps
        .get(&vpid)
        .map(|m| m.device_vpid.clone())
        .unwrap_or(vpid.clone());
    let bind = cfg
        .quick_start
        .keybinds
        .get(&actual_vpid)
        .and_then(|m| m.get(&key))
        .cloned();
    drop(cfg);
    let Some(bind) = bind else { return };
    let event = serde_json::json!({"evt_type":"QuickStart","evt_data":{"vp_id":actual_vpid,"key":key,"bind":bind}});
    let _ = app.emit("notify", event);
    let kind = bind
        .get("opt_type")
        .or_else(|| bind.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if kind.eq_ignore_ascii_case("Aikey") {
        // The decompiled client defines AiKeyCmd/AiKeyActionType, but the exact
        // trigger payload is not inferred here. Never fabricate a wire message.
        return;
    }
    let target = bind
        .get("opt_data")
        .or_else(|| bind.get("bind").and_then(|v| v.get("opt_data")))
        .and_then(|v| v.get("path"))
        .and_then(Value::as_str);
    let Some(target) = target else { return };
    let result = if kind.eq_ignore_ascii_case("Url")
        || target.starts_with("https://")
        || target.starts_with("http://")
    {
        Command::new("xdg-open").arg(target).spawn()
    } else if kind.eq_ignore_ascii_case("Dir") {
        Command::new("xdg-open").arg(target).spawn()
    } else {
        let Ok(args) = shell_words::split(target) else {
            return;
        };
        let Some((program, rest)) = args.split_first() else {
            return;
        };
        Command::new(program).args(rest).spawn()
    };
    if let Err(error) = result {
        eprintln!("[input] launch binding {actual_vpid}/{key}: {error}");
    }
}
