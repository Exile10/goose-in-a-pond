use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Default global hotkey: Cmd/Ctrl + Shift + G
pub const DEFAULT_HOTKEY: &str = "CmdOrCtrl+Shift+G";
/// Dedicated voice summon hotkey: Cmd/Ctrl + Shift + V
pub const DEFAULT_SUMMON_HOTKEY: &str = "CmdOrCtrl+Shift+V";

/// Tauri event names shared by every trigger (hotkey, menu bar, tray) that
/// can bring up Canvas / start a voice summon, so they can't drift apart.
pub const CANVAS_TOGGLE_EVENT: &str = "canvas-toggle";
pub const DESKTOP_SUMMON_EVENT: &str = "desktop-summon";

/// Managed state that tracks the currently registered canvas hotkey string.
/// Needed so `re_register_hotkey` can unregister the *current* hotkey rather
/// than always falling back to the compile-time default.
pub struct HotkeyState {
    pub current: Mutex<String>,
}

impl HotkeyState {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(DEFAULT_HOTKEY.to_string()),
        }
    }
}

/// Register the canvas hotkey: brings the main window forward, switches it
/// to the Canvas section, and starts a voice summon turn.
pub fn register_canvas_hotkey(app: &AppHandle) -> Result<(), String> {
    let shortcut = parse_shortcut(DEFAULT_HOTKEY)?;

    app.global_shortcut()
        .on_shortcut(shortcut, move |app_handle, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                handle_hotkey(app_handle);
            }
        })
        .map_err(|e| format!("Failed to register hotkey: {e}"))?;

    tracing::info!("Canvas hotkey registered: {}", DEFAULT_HOTKEY);
    Ok(())
}

/// Register a dedicated global hotkey that triggers a desktop voice summon turn.
pub fn register_summon_hotkey(app: &AppHandle) -> Result<(), String> {
    let shortcut = parse_shortcut(DEFAULT_SUMMON_HOTKEY)?;

    app.global_shortcut()
        .on_shortcut(shortcut, move |app_handle, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                let _ = app_handle.emit(DESKTOP_SUMMON_EVENT, ());
            }
        })
        .map_err(|e| format!("Failed to register summon hotkey: {e}"))?;

    tracing::info!("Summon hotkey registered: {}", DEFAULT_SUMMON_HOTKEY);
    Ok(())
}

/// Re-register the canvas hotkey with a new key combination (called from Settings).
///
/// Reads the currently registered hotkey from `HotkeyState`, unregisters it,
/// registers the new one, and updates the stored value.
pub fn re_register_hotkey(app: &AppHandle, hotkey: &str, state: &HotkeyState) -> Result<(), String> {
    // Unregister whichever hotkey is currently active
    {
        let current = state.current.lock().unwrap();
        if let Ok(old) = parse_shortcut(&current) {
            let _ = app.global_shortcut().unregister(old);
        }
    }

    let shortcut = parse_shortcut(hotkey)?;
    app.global_shortcut()
        .on_shortcut(shortcut, move |app_handle, _shortcut, event| {
            if event.state == ShortcutState::Pressed {
                handle_hotkey(app_handle);
            }
        })
        .map_err(|e| format!("Failed to register hotkey '{hotkey}': {e}"))?;

    // Persist the newly registered hotkey
    *state.current.lock().unwrap() = hotkey.to_string();
    tracing::info!("Canvas hotkey updated to: {hotkey}");
    Ok(())
}

fn handle_hotkey(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.set_focus();
    }
    let _ = app.emit(CANVAS_TOGGLE_EVENT, ());
    let _ = app.emit(DESKTOP_SUMMON_EVENT, ());
}

/// Parse a human-readable shortcut string like "CmdOrCtrl+Shift+G" into a Shortcut.
fn parse_shortcut(s: &str) -> Result<Shortcut, String> {
    let mut modifiers = Modifiers::empty();
    let mut code = None;

    for part in s.split('+') {
        match part.trim() {
            "CmdOrCtrl" | "CommandOrControl" => {
                modifiers |= if cfg!(target_os = "macos") {
                    Modifiers::META
                } else {
                    Modifiers::CONTROL
                };
            }
            "Ctrl" | "Control" => modifiers |= Modifiers::CONTROL,
            "Cmd" | "Command" | "Meta" => modifiers |= Modifiers::META,
            "Shift" => modifiers |= Modifiers::SHIFT,
            "Alt" | "Option" => modifiers |= Modifiers::ALT,
            key => {
                code = Some(key_to_code(key).ok_or_else(|| format!("Unknown key: {key}"))?);
            }
        }
    }

    let code = code.ok_or_else(|| format!("No key specified in shortcut: {s}"))?;
    Ok(Shortcut::new(Some(modifiers), code))
}

fn key_to_code(key: &str) -> Option<Code> {
    match key.to_uppercase().as_str() {
        "A" => Some(Code::KeyA),
        "B" => Some(Code::KeyB),
        "C" => Some(Code::KeyC),
        "D" => Some(Code::KeyD),
        "E" => Some(Code::KeyE),
        "F" => Some(Code::KeyF),
        "G" => Some(Code::KeyG),
        "H" => Some(Code::KeyH),
        "I" => Some(Code::KeyI),
        "J" => Some(Code::KeyJ),
        "K" => Some(Code::KeyK),
        "L" => Some(Code::KeyL),
        "M" => Some(Code::KeyM),
        "N" => Some(Code::KeyN),
        "O" => Some(Code::KeyO),
        "P" => Some(Code::KeyP),
        "Q" => Some(Code::KeyQ),
        "R" => Some(Code::KeyR),
        "S" => Some(Code::KeyS),
        "T" => Some(Code::KeyT),
        "U" => Some(Code::KeyU),
        "V" => Some(Code::KeyV),
        "W" => Some(Code::KeyW),
        "X" => Some(Code::KeyX),
        "Y" => Some(Code::KeyY),
        "Z" => Some(Code::KeyZ),
        "SPACE" => Some(Code::Space),
        "ESCAPE" | "ESC" => Some(Code::Escape),
        "ENTER" | "RETURN" => Some(Code::Enter),
        _ => None,
    }
}
