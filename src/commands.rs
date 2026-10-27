#![allow(non_snake_case)]

use crate::{
    linux::{apps, devices},
    model::*,
    state::AppState,
};
use serde_json::Value;
use std::{process::Command, sync::Arc};
use tauri::State;

#[tauri::command]
pub async fn get_installed(
    refresh: Option<bool>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Installed>, String> {
    // Caches written before icon support have no `icon` URLs at all; a fresh
    // scan on any real system resolves the vast majority, so an entirely
    // iconless cache is stale by definition. (Apps without any system icon
    // keep empty strings and fall back to the UI placeholder.)
    let needs_rescan = refresh.unwrap_or(false) || {
        let apps = &state.config.read().await.quick_start.apps;
        apps.is_empty() || apps.iter().all(|app| app.icon.is_empty())
    };
    if needs_rescan {
        let result = apps::scan();
        state.config.write().await.quick_start.apps = result.clone();
        state.save_config().await?;
        return Ok(result);
    }
    Ok(state.config.read().await.quick_start.apps.clone())
}

#[tauri::command]
pub async fn installed_app_add(
    installed: Installed,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let mut cfg = state.config.write().await;
    cfg.quick_start.apps.retain(|a| a.name != installed.name);
    cfg.quick_start.apps.push(installed);
    drop(cfg);
    state.save_config().await.map(|_| true)
}

#[tauri::command]
pub async fn installed_app_remove(
    name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    state
        .config
        .write()
        .await
        .quick_start
        .apps
        .retain(|app| app.name != name);
    state.save_config().await.map(|_| true)
}

#[tauri::command]
pub async fn get_connected_kb_list() -> Result<Vec<Value>, String> {
    Ok(devices::enumerate_keychron())
}

#[tauri::command]
pub async fn clear_key_bind(
    vp_id: Option<String>,
    vpId: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let id = vp_id.or(vpId).unwrap_or_default();
    let mut cfg = state.config.write().await;
    if id.is_empty() || id == "0" {
        cfg.quick_start.keybinds.clear();
    } else {
        cfg.quick_start.keybinds.remove(&id);
    }
    drop(cfg);
    state.save_config().await.map(|_| true)
}

#[tauri::command]
pub async fn lan_device_discover_start() -> Result<bool, String> {
    if let Some(r) = crate::rdp::instance() {
        r.st.lock().unwrap().scanning = true;
    }
    Ok(true)
}
#[tauri::command]
pub async fn lan_device_discover_stop() -> Result<bool, String> {
    if let Some(r) = crate::rdp::instance() {
        r.st.lock().unwrap().scanning = false;
    }
    Ok(true)
}
#[tauri::command]
pub async fn lan_broadcast_start() -> Result<bool, String> {
    if let Some(r) = crate::rdp::instance() {
        r.st.lock().unwrap().scanning = true;
    }
    Ok(true)
}
#[tauri::command]
pub async fn lan_broadcast_stop() -> Result<bool, String> {
    if let Some(r) = crate::rdp::instance() {
        r.st.lock().unwrap().scanning = false;
    }
    Ok(true)
}

#[tauri::command]
pub async fn get_plat() -> Result<String, String> {
    Ok("Linux".into())
}

#[tauri::command]
pub async fn check_accessibility_settings() -> Result<bool, String> {
    Ok(std::fs::read_dir("/dev/input").is_ok())
}
#[tauri::command]
pub async fn open_accessibility_settings() -> Result<bool, String> {
    open_settings("privacy")
}
#[tauri::command]
pub async fn check_input_permission() -> Result<bool, String> {
    // Linux has no permission prompt: capture needs readable /dev/input/event*, injection needs /dev/uinput.
    let readable = evdev::enumerate().next().is_some();
    Ok(readable && crate::rdp::instance().is_some_and(|r| r.inj.available()))
}
#[tauri::command]
pub async fn open_input_permission() -> Result<bool, String> {
    open_settings("privacy")
}
#[tauri::command]
pub async fn open_mouse_setting() -> Result<bool, String> {
    open_settings("mouse")
}

#[tauri::command]
pub async fn read_language_preference(state: State<'_, Arc<AppState>>) -> Result<Option<String>, String> {
    // null until the user picks a language (the UI then follows the system locale).
    if !state.root.join("language.json").exists() {
        return Ok(None);
    }
    Ok(Some(state.language.read().await.clone()))
}
#[tauri::command]
pub async fn write_language_preference(
    language_code: Option<String>,
    languageCode: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let value = language_code
        .or(languageCode)
        .unwrap_or_else(|| "en-US".into());
    std::fs::write(
        state.root.join("language.json"),
        serde_json::to_vec(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    *state.language.write().await = value;
    Ok(true)
}
#[tauri::command]
pub async fn read_language_cache(
    language_code: Option<String>,
    languageCode: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<Value>, String> {
    Ok(state
        .translations
        .read()
        .await
        .get(language_code.or(languageCode).unwrap_or_default())
        .cloned())
}
#[tauri::command]
pub async fn write_language_cache(
    language_code: Option<String>,
    languageCode: Option<String>,
    translations: Value,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let code = language_code.or(languageCode).unwrap_or_default();
    if let Some(obj) = state.translations.write().await.as_object_mut() {
        obj.insert(code, translations);
    }
    state.save_translations().await.map(|_| true)
}

#[tauri::command]
pub async fn update_ai_key_template(
    template: String,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let path = state.root.join("assist_key.json");
    let mut value: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({"list":[]}));
    value["template"] = Value::String(template);
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
    .map(|_| true)
}
#[tauri::command]
pub async fn clear_app_log(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    std::fs::write(state.root.join("log.json"), "[]")
        .map_err(|e| e.to_string())
        .map(|_| true)
}

fn open_settings(section: &str) -> Result<bool, String> {
    let candidates: &[(&str, &[&str])] = &[
        ("gnome-control-center", &[section]),
        ("kcmshell6", &["kcm_mouse"]),
        ("xfce4-settings-manager", &[]),
    ];
    for (program, args) in candidates {
        if Command::new(program).args(*args).spawn().is_ok() {
            return Ok(true);
        }
    }
    Err("No supported desktop settings utility found".into())
}