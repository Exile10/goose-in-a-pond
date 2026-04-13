// Prevents console window from appearing on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod canvas_feed;
mod commands;
mod hotkey;
mod process;
mod tray;

use audio::AudioState;
use commands::{audio_cmd, server_cmd, window_cmd};
use process::ServerProcess;
use tauri::{Emitter, Manager};
use tauri_plugin_window_state::StateFlags;

fn main() {
    tracing_subscriber::fmt::init();

    tauri::Builder::default()
        // ── Plugins ─────────────────────────────────────────────────────────
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::new()
                .with_state_flags(StateFlags::POSITION | StateFlags::SIZE)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(tauri_plugin_shell::init())
        // ── Managed state ────────────────────────────────────────────────────
        .manage(ServerProcess::new())
        .manage(AudioState::new())
        // ── Commands ─────────────────────────────────────────────────────────
        .invoke_handler(tauri::generate_handler![
            server_cmd::get_server_url,
            server_cmd::set_server_url,
            server_cmd::server_health,
            window_cmd::show_canvas,
            window_cmd::hide_canvas,
            window_cmd::toggle_canvas,
            window_cmd::canvas_visible,
            window_cmd::position_canvas,
            audio_cmd::start_recording,
            audio_cmd::stop_recording,
            audio_cmd::abort_recording,
            audio_cmd::run_voice_pipeline,
        ])
        // ── App lifecycle ─────────────────────────────────────────────────────
        .setup(|app| {
            let handle = app.handle().clone();

            // Build system tray
            tray::build_tray(&handle)?;

            // Inject server URL as a global JS variable so api.ts uses the right base URL
            inject_server_url(&handle, "http://127.0.0.1:4000");

            // Connect to (or spawn) pond-server in background
            let handle_server = handle.clone();
            let resource_dir = app
                .path()
                .resource_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));

            tauri::async_runtime::spawn(async move {
                let server = handle_server.state::<ServerProcess>();
                match server.connect_or_spawn(&resource_dir).await {
                    Ok(url) => {
                        tracing::info!("pond-server ready at {url}");
                        tray::set_tray_tooltip(&handle_server, "Connected");
                        inject_server_url(&handle_server, &url);
                    }
                    Err(e) => {
                        tracing::warn!("pond-server unavailable: {e}");
                        tray::set_tray_tooltip(&handle_server, "Disconnected");
                        let _ = handle_server.emit("server-offline", &e);
                    }
                }
            });

            // Register global canvas hotkey
            if let Err(e) = hotkey::register_canvas_hotkey(&handle) {
                tracing::warn!("Failed to register hotkey: {e}");
            }

            // Periodic health check every 10s
            let handle_health = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    let server = handle_health.state::<ServerProcess>();
                    let url = server.get_url();
                    let ok = server.health_check(&url).await;
                    tray::set_tray_tooltip(&handle_health, if ok { "Connected" } else { "Disconnected" });
                    let _ = handle_health.emit("server-status", ok);
                }
            });

            Ok(())
        })
        // Keep app alive in tray when main window is closed
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    let _ = window.hide();
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Failed to run Goose In A Pond desktop app");
}

/// Inject the server URL as `window.__GIAP_SERVER_URL__` so that api.ts
/// can construct the correct base URL regardless of what hostname/port
/// pond-server is running on.
fn inject_server_url(app: &tauri::AppHandle, url: &str) {
    if let Some(win) = app.get_webview_window("main") {
        let script = format!(
            r#"window.__GIAP_SERVER_URL__ = "{}"; console.log("[GIAP] Server:", window.__GIAP_SERVER_URL__);"#,
            url
        );
        let _ = win.eval(&script);
    }
}
