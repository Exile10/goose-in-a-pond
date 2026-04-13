use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

/// Handle to an optionally-spawned pond-server child process.
/// If the user already has pond-server running, we connect to it without spawning.
pub struct ServerProcess {
    child: Mutex<Option<Child>>,
    pub url: Mutex<String>,
}

impl ServerProcess {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            url: Mutex::new("http://127.0.0.1:4000".to_string()),
        }
    }

    /// Try to connect to a running pond-server; if none is found, spawn the
    /// bundled binary from the app's resource directory.
    pub async fn connect_or_spawn(
        &self,
        resource_dir: &std::path::Path,
    ) -> Result<String, String> {
        let url = self.url.lock().unwrap().clone();

        // 1. Probe a running server
        if self.health_check(&url).await {
            tracing::info!("Connected to existing pond-server at {}", url);
            return Ok(url);
        }

        // 2. Look for bundled binary
        let binary_name = if cfg!(windows) {
            "pond-server.exe"
        } else {
            "pond-server"
        };
        let binary_path = resource_dir.join(binary_name);

        if !binary_path.exists() {
            return Err(format!(
                "No pond-server running at {} and no bundled binary found at {}",
                url,
                binary_path.display()
            ));
        }

        tracing::info!("Spawning pond-server from {}", binary_path.display());
        let child = Command::new(&binary_path)
            .arg("serve")
            .arg("--port")
            .arg("4000")
            .spawn()
            .map_err(|e| format!("Failed to spawn pond-server: {e}"))?;

        *self.child.lock().unwrap() = Some(child);

        // Wait for the server to be ready (up to 10s)
        for _ in 0..20 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if self.health_check(&url).await {
                tracing::info!("Spawned pond-server is ready at {}", url);
                return Ok(url);
            }
        }

        Err("Spawned pond-server did not become healthy within 10s".to_string())
    }

    pub async fn health_check(&self, url: &str) -> bool {
        let endpoint = format!("{}/api/v1/health", url);
        reqwest::Client::new()
            .get(&endpoint)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    /// Kill the spawned child process on shutdown.
    #[allow(dead_code)]
    pub fn shutdown(&self) {
        if let Ok(mut lock) = self.child.lock() {
            if let Some(mut child) = lock.take() {
                let _ = child.kill();
                tracing::info!("pond-server child process terminated");
            }
        }
    }

    pub fn get_url(&self) -> String {
        self.url.lock().unwrap().clone()
    }

    pub fn set_url(&self, url: String) {
        *self.url.lock().unwrap() = url;
    }
}

impl Default for ServerProcess {
    fn default() -> Self {
        Self::new()
    }
}
