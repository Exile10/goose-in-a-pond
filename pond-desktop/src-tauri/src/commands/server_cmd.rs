use crate::process::ServerProcess;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub async fn get_server_url(server: State<'_, ServerProcess>) -> Result<String, String> {
    Ok(server.get_url())
}

#[tauri::command]
pub async fn set_server_url(url: String, server: State<'_, ServerProcess>) -> Result<(), String> {
    server.set_url(url);
    Ok(())
}

#[tauri::command]
pub async fn server_health(server: State<'_, ServerProcess>) -> Result<bool, String> {
    let url = server.get_url();
    Ok(server.health_check(&url).await)
}

/// Called by the frontend StartupScreen to ensure pond-server is running
/// before the UI is shown. Emits `server-starting` before attempting startup.
#[tauri::command]
pub async fn ensure_server_running(
    server: State<'_, ServerProcess>,
    app: AppHandle,
) -> Result<String, String> {
    let _ = app.emit("server-starting", ());
    let resource_dir = app
        .path()
        .resource_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    server.ensure_running(&resource_dir).await
}
