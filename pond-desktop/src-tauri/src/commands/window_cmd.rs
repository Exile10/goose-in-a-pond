use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn show_canvas(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("canvas")
        .ok_or("Canvas window not found")?;
    win.show().map_err(|e| e.to_string())?;
    win.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn hide_canvas(app: AppHandle) -> Result<(), String> {
    let win = app
        .get_webview_window("canvas")
        .ok_or("Canvas window not found")?;
    win.hide().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn toggle_canvas(app: AppHandle) -> Result<bool, String> {
    let win = app
        .get_webview_window("canvas")
        .ok_or("Canvas window not found")?;

    if win.is_visible().map_err(|e| e.to_string())? {
        win.hide().map_err(|e| e.to_string())?;
        Ok(false)
    } else {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        Ok(true)
    }
}

#[tauri::command]
pub async fn canvas_visible(app: AppHandle) -> Result<bool, String> {
    let win = app
        .get_webview_window("canvas")
        .ok_or("Canvas window not found")?;
    win.is_visible().map_err(|e| e.to_string())
}

/// Snap canvas overlay to a screen edge.
/// position: "right" | "left" | "center"
#[tauri::command]
pub async fn position_canvas(app: AppHandle, position: String) -> Result<(), String> {
    let win = app
        .get_webview_window("canvas")
        .ok_or("Canvas window not found")?;

    let monitor = win
        .current_monitor()
        .map_err(|e| e.to_string())?
        .ok_or("Cannot determine monitor")?;

    let screen_size = monitor.size();
    let win_size = win.outer_size().map_err(|e| e.to_string())?;
    let scale = monitor.scale_factor();

    let (x, y) = match position.as_str() {
        "left" => (
            20.0,
            (screen_size.height as f64 / scale - win_size.height as f64 / scale) / 2.0,
        ),
        "center" => (
            (screen_size.width as f64 / scale - win_size.width as f64 / scale) / 2.0,
            (screen_size.height as f64 / scale - win_size.height as f64 / scale) / 2.0,
        ),
        _ => (
            // right (default)
            screen_size.width as f64 / scale - win_size.width as f64 / scale - 20.0,
            (screen_size.height as f64 / scale - win_size.height as f64 / scale) / 2.0,
        ),
    };

    win.set_position(tauri::LogicalPosition::new(x, y))
        .map_err(|e| e.to_string())?;
    Ok(())
}
