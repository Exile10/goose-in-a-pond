//! Toolkit MCP Server — the tool-relevance escape hatch (Phase D2).
//!
//! Provides 2 tools: `list_tool_groups`, `enable_tool_group`.
//!
//! ## Why this exists
//!
//! Phase D narrows which extension tool schemas reach the model, per session, so
//! a 59-tool surface (~5.9K prompt tokens through the Gemma chat template) fits
//! an 8K-class on-device budget. Narrowing is only safe if the model can reach a
//! capability that was not preloaded — otherwise a mis-scored session is a dead
//! end and the user just gets a worse assistant.
//!
//! This server is that hatch, and it is why the whole feature is defensible: the
//! tool surface is LAZILY LOADED, not restricted. The model still decides
//! natively whether and which tools to call; it can also decide that it needs a
//! group nobody predicted, and say so.
//!
//! Both tools are in the always-on core set, so they are present in every
//! session regardless of selection.

use pond_core::mcp::domain::tool_group::TOOLKIT_EXTENSION;
use pond_core::mcp::ports::tools::tool_selection_control::{
    ToolSelectionControl, ToolSelectionError,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, ErrorData, Implementation, InitializeResult, ProtocolVersion,
        ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    tool, tool_handler, tool_router, RoleServer, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::Arc;

// ── Parameter structs ─────────────────────────────────────────────────────────

/// Zero real parameters — the schema stays tiny on purpose, because this tool is
/// in every prompt and every character of it is re-prefilled every fresh turn.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListToolGroupsParams {
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct EnableToolGroupParams {
    /// Exact group name, e.g. "giap-schedule".
    pub group: Option<String>,
    /// Catch-all for unexpected fields the model might send.
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: std::collections::HashMap<String, serde_json::Value>,
}

/// Pull the group name out of `group`, falling back to the extras bag under the
/// aliases a small model is most likely to invent.
fn resolve_group(params: &EnableToolGroupParams) -> Option<String> {
    if let Some(g) = params.group.as_ref() {
        let g = g.trim();
        if !g.is_empty() {
            return Some(g.to_string());
        }
    }
    for key in ["name", "extension", "tool_group", "group_name"] {
        if let Some(v) = params.extra.get(key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

// ── MCP server ─────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct ToolkitMcpServer {
    control: Option<Arc<dyn ToolSelectionControl>>,
    #[allow(dead_code)] // accessed by rmcp's generated tool_handler code
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl ToolkitMcpServer {
    pub fn new(control: Option<Arc<dyn ToolSelectionControl>>) -> Self {
        Self {
            control,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "\
List the groups of tools available on this device and whether each is loaded now. \
Use when a capability you need seems to be missing.")]
    async fn list_tool_groups(
        &self,
        ctx: RequestContext<RoleServer>,
        _params: Parameters<ListToolGroupsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("list_tool_groups");
        let Some(control) = &self.control else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        };
        // `_meta`, not `current_session_id()`. See `session_meta.rs`: the global
        // is a `RwLock<String>` raced by four concurrent chat streams, so it can
        // name a different member's conversation than the one that called.
        let Some(session_id) = crate::session_from_meta(&ctx.meta) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        };
        let groups = control.group_status(&session_id).await;
        if groups.is_empty() {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded for this conversation.",
            )]));
        }

        let mut out = String::with_capacity(groups.len() * 96);
        out.push_str("Tool groups on this device:\n");
        for g in &groups {
            let state = if g.core {
                "loaded (always)"
            } else if g.loaded {
                "loaded"
            } else {
                "NOT loaded"
            };
            out.push_str(&format!(
                "- {} [{}] {} tools: {}\n",
                g.extension, state, g.tool_count, g.description
            ));
        }
        out.push_str(
            "\nTo use a group that is NOT loaded, call enable_tool_group with its exact name; \
             its tools become available straight away.",
        );
        Ok(CallToolResult::success(vec![Content::text(out)]))
    }

    #[tool(description = "\
Load a group of tools that is not currently available, by its exact name (e.g. \
\"giap-schedule\"). Its tools can be called immediately afterwards.")]
    async fn enable_tool_group(
        &self,
        ctx: RequestContext<RoleServer>,
        params: Parameters<EnableToolGroupParams>,
    ) -> Result<CallToolResult, ErrorData> {
        crate::set_current_tool("enable_tool_group");
        let Some(control) = &self.control else {
            return Ok(CallToolResult::success(vec![Content::text(
                "All available tools are already loaded — there is nothing to enable.",
            )]));
        };
        let Some(group) = resolve_group(&params.0) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Which group? Call list_tool_groups to see the exact names, then pass one as \
                 'group'.",
            )]));
        };
        // Widening is authorisation, so it reads the one channel that cannot be
        // raced. An unattributable call widens nothing rather than widening
        // whichever conversation happened to start a turn most recently.
        let Some(session_id) = crate::session_from_meta(&ctx.meta) else {
            return Ok(CallToolResult::success(vec![Content::text(
                "Tool groups cannot be changed from here — every group already available \
                 to you is loaded.",
            )]));
        };
        match control.enable_group(&session_id, &group).await {
            Ok(loaded) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "tool_group_enabled",
                    session_id = %session_id,
                    group = %group,
                    loaded = ?loaded,
                );
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Loaded '{group}'. Its tools are available now — go ahead and call the one you \
                     need. Loaded groups: {}.",
                    loaded.join(", ")
                ))]))
            }
            // The group IS loaded; its tools just are not callable yet. The
            // catch-all below says "Could not load" and points at
            // list_tool_groups, both of which would be wrong here and would send
            // the model back round a loop it has already completed.
            Err(ToolSelectionError::NotReady(_)) => {
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Loaded '{group}', but its tools only become callable on your next turn. \
                     Do not call one yet — answer with what you have, or use a tool you \
                     already had."
                ))]))
            }
            // A failure is reported as tool SUCCESS carrying the explanation: an
            // MCP error makes small models retry the same bad call, whereas a
            // readable message with the valid names lets them self-correct.
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Could not load '{group}': {e}. Call list_tool_groups for the exact names."
            ))])),
        }
    }
}

#[tool_handler]
impl ServerHandler for ToolkitMcpServer {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new(
                TOOLKIT_EXTENSION,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(
                "GIAP Toolkit MCP server — discover and load tool groups.\n\n\
                 To keep the prompt small on-device, only some groups of tools are loaded for a \
                 conversation. If a capability you need is missing, call list_tool_groups to see \
                 what exists, then enable_tool_group to load it. Nothing is permanently \
                 unavailable.",
            )
    }
}

// ── Static deps + spawn function for Goose builtin registry ──────────────

use std::sync::OnceLock;
use tokio::io::DuplexStream;

struct ToolkitDeps {
    control: Option<Arc<dyn ToolSelectionControl>>,
}

static TOOLKIT_DEPS: OnceLock<ToolkitDeps> = OnceLock::new();

/// Install the tool-selection control handle. Call once at startup, AFTER the
/// agent adapter exists (it is the implementor). Without it both tools degrade
/// to "everything is already loaded", which is the truthful answer when no
/// narrowing is in effect.
pub fn init_toolkit_deps(control: Option<Arc<dyn ToolSelectionControl>>) {
    let _ = TOOLKIT_DEPS.set(ToolkitDeps { control });
}

/// Spawn function compatible with Goose's `SpawnServerFn` type.
pub fn spawn_toolkit_server(reader: DuplexStream, writer: DuplexStream) {
    // Unlike the other servers this does NOT expect(): the toolkit extension is
    // registered before the adapter that implements the port is built, so a
    // missing handle is a legitimate transient state, not a bug.
    let control = TOOLKIT_DEPS.get().and_then(|d| d.control.clone());
    let server = ToolkitMcpServer::new(control);
    crate::serve_builtin("giap-toolkit", server, reader, writer);
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Widening a session's tool surface is authorisation, so it may only ever
    /// read the per-call channel.
    ///
    /// `crate::current_session_id()` is a process-global `RwLock<String>` written
    /// once per turn, with `Semaphore::new(4)` concurrent chat streams racing it.
    /// Both tools here used to read it, so one member's `enable_tool_group` could
    /// widen a different member's allow-set — and `session_meta.rs` had already
    /// written down why that is not acceptable: *"Correct authorisation on a
    /// misattributed session is not correct."*
    ///
    /// A source scan because the alternative needs a live `RequestContext`, which
    /// rmcp does not offer a constructor for. Comments are stripped so the note
    /// explaining the ban does not itself trip it — without that, this guard
    /// would have failed on the day it landed for reasons unrelated to the code.
    #[test]
    fn neither_tool_reads_the_process_global_session() {
        let src = include_str!("toolkit.rs");
        let production = src.split("mod tests").next().unwrap_or(src);
        let code: String = production
            .lines()
            .map(|l| match l.find("//") {
                Some(i) => &l[..i],
                None => l,
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Vacuity control: the scan must be able to see the call it permits, or
        // a rename would make this silently pass forever.
        assert!(
            code.contains("session_from_meta("),
            "neither tool resolves a session from _meta — this guard is scanning \
             the wrong thing"
        );
        assert!(
            !code.contains("current_session_id("),
            "toolkit.rs reads the process-global session id. Four chat streams \
             race it, so this widens whichever conversation started a turn most \
             recently rather than the one that called."
        );
    }

    #[test]
    fn group_comes_from_the_declared_param() {
        let params = EnableToolGroupParams {
            group: Some("giap-schedule".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_group(&params), Some("giap-schedule".to_string()));
    }

    /// Small on-device models routinely rename the argument. Recovering costs
    /// nothing and saves a whole round trip.
    #[test]
    fn group_is_recovered_from_common_aliases() {
        for alias in ["name", "extension", "tool_group", "group_name"] {
            let mut extra = std::collections::HashMap::new();
            extra.insert(
                alias.to_string(),
                serde_json::Value::String("giap-vision".to_string()),
            );
            let params = EnableToolGroupParams { group: None, extra };
            assert_eq!(
                resolve_group(&params),
                Some("giap-vision".to_string()),
                "alias '{alias}' was not recovered"
            );
        }
    }

    #[test]
    fn blank_and_absent_groups_resolve_to_none() {
        assert_eq!(resolve_group(&EnableToolGroupParams::default()), None);
        let params = EnableToolGroupParams {
            group: Some("   ".to_string()),
            extra: Default::default(),
        };
        assert_eq!(resolve_group(&params), None);
    }

    /// Under `tool_selection_mode = "minimal"` these two tools are the ENTIRE
    /// tool surface, and the reason that mode exists is a 4%-of-prompt-budget
    /// ceiling. So the ceiling has to be a test, not a claim in a doc comment.
    ///
    /// The budget is `LOCAL_PROMPT_CLAMP` = 8,192 tokens (the prompt-side clamp
    /// every local provider gets, whatever its n_ctx), 4% of which is 327
    /// tokens. Tool JSON tokenizes at very close to 4 chars/token on the Gemma
    /// template — the 61-tool payload measured 30,463 chars against 7,633
    /// counted prompt tokens, 0.2% off — so the ceiling in characters is 1,310.
    ///
    /// Measured on the real serialized schemas rather than the source text,
    /// because what costs tokens is what `list_all()` hands the model: adding
    /// one optional field to `EnableToolGroupParams` is a one-line change that
    /// would quietly move this number.
    #[test]
    fn the_hatch_fits_four_percent_of_the_prompt_budget() {
        const PROMPT_BUDGET_TOKENS: usize = 8_192;
        const CHARS_PER_TOKEN: usize = 4;
        let ceiling = PROMPT_BUDGET_TOKENS * 4 / 100 * CHARS_PER_TOKEN;

        let tools = ToolkitMcpServer::tool_router().list_all();
        let chars: usize = tools
            .iter()
            .map(|t| serde_json::to_string(t).map(|s| s.len()).unwrap_or(0))
            .sum();

        assert_eq!(tools.len(), 2, "the hatch is two tools: {tools:?}");
        assert!(
            chars <= ceiling,
            "the minimal tool surface is {chars} chars (~{} tok, {:.1}% of the \
             {PROMPT_BUDGET_TOKENS}-token prompt budget) and the ceiling is \
             {ceiling} chars (327 tok, 4.0%). \"minimal\" no longer delivers \
             what it is named for.",
            chars / CHARS_PER_TOKEN,
            100.0 * (chars as f32 / CHARS_PER_TOKEN as f32) / PROMPT_BUDGET_TOKENS as f32,
        );
    }
}
