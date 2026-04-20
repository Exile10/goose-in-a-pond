use tauri::{AppHandle, State};
use tauri_plugin_autostart::ManagerExt;

/// Enable auto-launch at login.
#[tauri::command]
pub fn enable_autostart(app: AppHandle) -> Result<(), String> {
    app.autolaunch().enable().map_err(|e| e.to_string())
}

/// Disable auto-launch at login.
#[tauri::command]
pub fn disable_autostart(app: AppHandle) -> Result<(), String> {
    app.autolaunch().disable().map_err(|e| e.to_string())
}

/// Return `true` if auto-launch is currently registered.
#[tauri::command]
pub fn is_autostart_enabled(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

/// Open macOS System Settings → Privacy → Microphone so the user can grant access.
#[tauri::command]
pub fn open_privacy_mic() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Re-register the global canvas hotkey with a new key combination.
/// The string format is the same as the default: e.g. "CmdOrCtrl+Shift+G".
#[tauri::command]
pub fn set_hotkey(
    app: AppHandle,
    hotkey: String,
    hotkey_state: State<'_, crate::hotkey::HotkeyState>,
) -> Result<(), String> {
    crate::hotkey::re_register_hotkey(&app, &hotkey, &hotkey_state)
}
