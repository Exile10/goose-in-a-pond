//! `GET /api/v1/extensions` reports an extension's status from whether it runs, not from its
//! tools: one that runs with no tools on purpose (music on a Pond that is not a Mac) must not be
//! listed as a failure, and one that truly failed still is.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use pond_core::mcp::ports::extension_manager::{
    AddExtensionRequest, ExtensionInfo, ExtensionManagerPort, ToolInfo,
};
use pond_core::mcp::ports::mcp_server::{McpServerConfig, McpServerRepository};
use serde_json::Value;
use tower::ServiceExt;

const MUSIC_REASON: &str =
    "No music service can be controlled by the assistant on this computer: Apple Music needs a Mac.";
const BROKEN_ERROR: &str = "Failed to add extension: the command exited with status 1";

/// What the extension manager reports: music running tool-less, `broken` failed to start.
struct Manager;

#[async_trait::async_trait]
impl ExtensionManagerPort for Manager {
    async fn list_extensions(&self) -> anyhow::Result<Vec<ExtensionInfo>> {
        let info = |name: &str, status: &str| ExtensionInfo {
            name: name.into(),
            kind: "builtin".into(),
            description: String::new(),
            tools: vec![],
            enabled: true,
            status: status.into(),
            last_error: None,
            status_reason: None,
        };
        Ok(vec![
            ExtensionInfo {
                status_reason: Some(MUSIC_REASON.into()),
                ..info("music", "no_tools")
            },
            ExtensionInfo {
                last_error: Some(BROKEN_ERROR.into()),
                ..info("broken", "error")
            },
        ])
    }
    async fn add_extension(&self, _: AddExtensionRequest) -> anyhow::Result<ExtensionInfo> {
        anyhow::bail!("not used")
    }
    async fn remove_extension(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn list_tools(&self) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    async fn list_tools_detailed(&self) -> anyhow::Result<Vec<ToolInfo>> {
        Ok(vec![])
    }
    async fn set_enabled(&self, _: &str, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The saved rows: the two the manager reports, one it has never heard of, one switched off.
struct Rows;

fn row(name: &str, enabled: bool) -> McpServerConfig {
    McpServerConfig {
        id: format!("{name}-id"),
        name: name.into(),
        kind: "stdio".into(),
        description: String::new(),
        command: Some("npx".into()),
        args: vec![],
        env: std::collections::HashMap::new(),
        uri: None,
        enabled,
        created_at: "2026-10-19T00:00:00Z".into(),
    }
}

#[async_trait::async_trait]
impl McpServerRepository for Rows {
    async fn list(&self) -> anyhow::Result<Vec<McpServerConfig>> {
        Ok(vec![
            row("music", true),
            row("broken", true),
            row("missing", true),
            row("off", false),
        ])
    }
    async fn save(&self, _: &McpServerConfig) -> anyhow::Result<()> {
        Ok(())
    }
    async fn delete(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn set_enabled(&self, _: &str, _: bool) -> anyhow::Result<()> {
        Ok(())
    }
}

/// `GET /api/v1/extensions`, in the order the route lists them.
async fn listed() -> Vec<Value> {
    let pond_api::test_support::TestState {
        state, dir: _dir, ..
    } = pond_api::test_support::app_state().await;
    let handshake = Arc::new(pond_infra::mock_handshake::MockHandshake::new());
    handshake.add_valid_token("desktop-token".to_string()).await;
    let state = Arc::new(pond_api::AppState {
        handshake,
        mcp_server_repo: Some(Arc::new(Rows)),
        extension_manager: Some(Arc::new(Manager)),
        ..state
    });
    let router =
        pond_api::build_router(state, std::path::PathBuf::from("pond-desktop/dist")).layer(
            MockConnectInfo(SocketAddr::from(([192, 168, 1, 44], 40_000))),
        );
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/v1/extensions")
                .header("Authorization", "Bearer desktop-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{body}");
    body["extensions"]
        .as_array()
        .expect("an extensions array")
        .clone()
}

/// The single entry named `name`; a second one would mean a row was listed twice.
fn entry<'a>(exts: &'a [Value], name: &str) -> &'a Value {
    let mut matches = exts.iter().filter(|e| e["name"] == name);
    let found = matches
        .next()
        .unwrap_or_else(|| panic!("{name} is not listed"));
    assert!(matches.next().is_none(), "{name} is listed twice");
    found
}

#[tokio::test]
async fn a_running_extension_without_tools_is_not_an_error() {
    let exts = listed().await;
    let music = entry(&exts, "music");
    assert_eq!(music["status"], "no_tools", "{music}");
    assert!(music["last_error"].is_null(), "{music}");
    assert_eq!(music["status_reason"], MUSIC_REASON);
}

#[tokio::test]
async fn a_failed_start_is_still_an_error_with_its_own_reason() {
    let exts = listed().await;
    let broken = entry(&exts, "broken");
    assert_eq!(broken["status"], "error");
    assert_eq!(broken["last_error"], BROKEN_ERROR);

    // A saved, enabled row the manager knows nothing of did not start: still a failure.
    let missing = entry(&exts, "missing");
    assert_eq!(missing["status"], "error");
    assert_eq!(missing["last_error"], "Extension failed to load");

    let off = entry(&exts, "off");
    assert_eq!(off["status"], "disabled");
    assert!(off["last_error"].is_null());
}
