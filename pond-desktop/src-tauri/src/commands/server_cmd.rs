use crate::process::ServerProcess;
use tauri::State;

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
