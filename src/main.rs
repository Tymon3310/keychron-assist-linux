mod commands;
mod linux;
mod logger;
mod model;
mod rdp;
mod service;
mod state;
mod uipack;

use state::AppState;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager,
};

fn show_main(app: &tauri::AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// `--screen-probe [warp]`: print what the cross-screen code sees on this desktop.
fn screen_probe(warp: bool) {
    use rdp::screen;
    println!("backend: {:?}", screen::init());
    for m in screen::monitors() {
        println!("monitor: {}x{} at {},{}", m.w, m.h, m.x, m.y);
    }
    println!("cursor: {:?}", screen::cursor());
    if !warp {
        return;
    }
    let inj = rdp::inject::Injector::new();
    std::thread::sleep(std::time::Duration::from_secs(1));
    println!("flat pointer: {}", screen::flat_pointer(rdp::inject::DEVICE_NAME));
    let start = screen::cursor();
    for m in screen::monitors() {
        let (x, y) = (m.x + m.w / 2.0, m.y + m.h / 2.0);
        let ok = if screen::can_warp() {
            screen::warp(x, y)
        } else {
            screen::warp_relative(x, y, |dx, dy| inj.apply(&rdp::wire::Toc::MouseMove(dx, dy)))
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        println!("warp to ({x}, {y}): {ok}, cursor now {:?}", screen::cursor());
    }
    if let Some((x, y)) = start {
        let _ = screen::warp(x, y) || screen::warp_relative(x, y, |dx, dy| inj.apply(&rdp::wire::Toc::MouseMove(dx, dy)));
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--screen-probe") {
        logger::init_stderr();
        screen_probe(args.get(i + 1).is_some_and(|a| a == "warp"));
        return;
    }
    if let Some(i) = args.iter().position(|a| a == "--install-ui") {
        logger::init_stderr();
        let bytes = match args.get(i + 1) {
            Some(f) => std::fs::read(f).map_err(|e| format!("{f}: {e}")),
            None => {
                println!("downloading {}", uipack::DOWNLOAD_URL);
                uipack::download()
            }
        };
        match bytes.and_then(|b| uipack::install(&b)) {
            Ok(n) => println!("installed {n} files into {}", uipack::ui_dir().display()),
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let state = Arc::new(AppState::load().expect("load Keychron Assistant state"));
    logger::init(&state.root);
    log::info!("-------- Assist Started ---------");
    let background = std::env::args().any(|a| a == "--background" || a == "--minimized");
    let (ws_tx, _) = tokio::sync::broadcast::channel(128);
    // Keychron's interface is installed separately (see uipack); serve it from disk.
    let mut context = tauri::generate_context!();
    let builtin = context.set_assets(Box::new(uipack::NoAssets));
    context.set_assets(Box::new(uipack::DiskAssets { fallback: builtin }));
    if !uipack::installed() {
        log::warn!("Keychron interface not installed yet: showing setup page");
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--background"])))
        .manage(state.clone())
        .manage(rdp::commands::WebState(std::sync::Mutex::new(serde_json::Value::Null)))
        .setup(move |app| {
            let show = MenuItem::with_id(app, "show", "Open Keychron Assistant", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .menu(&menu)
                .show_menu_on_left_click(false)
                .icon(app.default_window_icon().expect("configured app icon").clone())
                .tooltip("Keychron Assistant")
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            let handle = app.handle().clone();
            match rdp::Rdp::start(handle.clone(), state.root.clone()) {
                Ok(r) => {
                    app.manage(r);
                }
                Err(e) => {
                    log::error!("cross-screen service failed to start (UDP 7654/40001 in use?): {e}");
                    return Err(e.into());
                }
            }
            log::warn!("Starting UDP server at 0.0.0.0:{}", rdp::wire::SESSION_PORT);

            let ws_state = state.clone();
            tauri::async_runtime::spawn(async move { service::launcher_ws::run(ws_state, ws_tx).await });
            tauri::async_runtime::spawn(service::netwatch::run());
            service::input::start(state.clone(), handle.clone());

            if !background {
                show_main(&handle);
            }
            #[cfg(debug_assertions)]
            if std::env::var("KEYCHRON_DEVTOOLS").is_ok() {
                if let Some(w) = handle.get_webview_window("main") {
                    w.open_devtools();
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the main window keeps the app running in the tray, like the native client.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_installed,
            commands::installed_app_add,
            commands::installed_app_remove,
            commands::get_connected_kb_list,
            commands::clear_key_bind,
            commands::lan_device_discover_start,
            commands::lan_device_discover_stop,
            commands::lan_broadcast_start,
            commands::lan_broadcast_stop,
            commands::get_plat,
            commands::check_accessibility_settings,
            commands::open_accessibility_settings,
            commands::check_input_permission,
            commands::open_input_permission,
            commands::open_mouse_setting,
            commands::read_language_preference,
            commands::write_language_preference,
            commands::read_language_cache,
            commands::write_language_cache,
            commands::update_ai_key_template,
            commands::clear_app_log,
            rdp::commands::switch_rdp_scan,
            rdp::commands::get_local_device_info,
            rdp::commands::validate_port_status,
            rdp::commands::rdp_connect,
            rdp::commands::rdp_cancel_pairing,
            rdp::commands::rdp_complete_connection,
            rdp::commands::rdp_confirm,
            rdp::commands::rdp_confirm_dialog,
            rdp::commands::rdp_reject,
            rdp::commands::rdp_reject_dialog,
            rdp::commands::rdp_disconnect,
            rdp::commands::rdp_reset_stale_pairing_state,
            rdp::commands::rdp_should_initiate_automatic,
            rdp::commands::rdp_trust_current_peer,
            rdp::commands::sync_rdp_web_state,
            rdp::commands::rdp_update_screen_position,
            rdp::commands::rdp_release_mouse,
            rdp::commands::set_rdp_disconnect_shortcut,
            rdp::commands::set_system_lock_shortcut,
            rdp::commands::switch_file_transfer,
            rdp::commands::select_transfer_files,
            rdp::commands::send_file_by_path,
            rdp::commands::cancel_file_transfer,
            rdp::commands::transfer_file_exists,
            rdp::commands::open_rdp_file_folder,
            rdp::commands::set_rdp_file_folder,
            rdp::commands::reveal_file_transfer_file,
            rdp::commands::get_file_transfer_history,
            rdp::commands::record_file_transfer_history,
            rdp::commands::remove_file_transfer_history_record,
            rdp::commands::clear_file_transfer_history,
            rdp::commands::get_rdp_connection_history,
            rdp::commands::record_rdp_connection_history,
            rdp::commands::clear_rdp_connection_history,
            rdp::commands::remove_rdp_trusted_device,
            rdp::commands::read_rdp_automatic_connection_device,
            rdp::commands::write_rdp_automatic_connection_device,
            uipack::ui_setup_status,
            uipack::ui_setup_download,
            uipack::ui_setup_from_file,
        ])
        .run(context)
        .expect("run Keychron Assistant");
}
