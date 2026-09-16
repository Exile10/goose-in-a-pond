//! The tool block, exactly as it reaches the model — pinned.
//!
//! # Why this exists
//!
//! Every phase of the goose-parity work moves something that touches the tool
//! list, and the tool list IS the prompt prefix: on the Gemma template the tool
//! schemas are ~90% of a fresh turn. A prefix that moves invalidates the
//! retained KV cache and the on-disk snapshot, which costs a full re-prefill —
//! measured at ~3.7 s for an 8,192-token prompt on the Orin.
//!
//! "Any tool-list change moves the KV prefix" is a warning nobody can act on.
//! This turns it into a failing diff.
//!
//! # What it pins, and why that is the right thing
//!
//! Three properties, in the order the prefix depends on them:
//!
//! 1. **Which tools.** A tool added or removed changes the prefix outright.
//! 2. **In what order.** `prefix_sort_key` puts core groups first so two
//!    conversations differing in half their tools still share most of the
//!    preamble (measured 70% → 85% when the differing tools sort last). Order is
//!    therefore load-bearing, not cosmetic.
//! 3. **Their serialized size.** A schema that grows moves every byte after it.
//!
//! It deliberately does NOT pin the schema text itself. A description reworded
//! for the model's benefit should not fail a test about cache stability — but it
//! must show up as a size change, which it does.
//!
//! # Which byte count this is
//!
//! The RAW router form, before the provider shim minifies it. The shim strips
//! `$schema`, `title`, `format`, `minimum` and `maximum`
//! (`pond-adapters-goose/src/provider_shim.rs::minify_schema_object`), which on
//! the current surface takes **14,478 bytes down to 13,358** — about 7.7%.
//!
//! So this number is not the one the model receives, and the two must not be
//! conflated: quote 13,358 (~3,339 tok, 40.8% of the 8,192-token prompt budget)
//! when talking about prompt cost, and this one only when talking about
//! movement. It pins the raw form because `pond-mcp-server` must not depend on
//! `pond-adapters-goose` — that is the hexagonal direction, and the CI "fast
//! crates" list is what enforces it. A constant offset detects every movement
//! just as well as an absolute one.
//!
//! # Reading a failure
//!
//! A diff here is not automatically a bug. It says: the prefix moved, so the
//! next cold turn on the device pays for it. Accept it deliberately and update
//! the fixture, or find out why it moved. What it forbids is moving by accident.

use pond_core::mcp::domain::tool_group::prefix_sort_key;
use rmcp::model::Tool;

/// Every tool GIAP can offer, from the real routers rather than a source scan.
///
/// Returned unordered; callers that care about the prefix use [`ordered_tools`].
pub fn all_tools() -> Vec<(&'static str, Tool)> {
    let mut out: Vec<(&'static str, Tool)> = Vec::new();
    let mut push = |ext: &'static str, tools: Vec<Tool>| {
        for t in tools {
            out.push((ext, t));
        }
    };

    push("giap-memory", crate::memory::MemoryMcpServer::tool_defs());
    push(
        "giap-weather",
        crate::weather::WeatherMcpServer::tool_defs(),
    );
    push("giap-system", crate::system::SystemMcpServer::tool_defs());
    push(
        "giap-schedule",
        crate::schedule::ScheduleMcpServer::tool_defs(),
    );
    push("giap-device", crate::device::DeviceMcpServer::tool_defs());
    push(
        "giap-device-control",
        crate::device_control::DeviceControlMcpServer::tool_defs(),
    );
    push(
        "giap-sensors",
        crate::sensors::SensorsMcpServer::tool_defs(),
    );
    push(
        "giap-knowledge",
        crate::knowledge::KnowledgeMcpServer::tool_defs(),
    );
    push(
        "giap-toolkit",
        crate::toolkit::ToolkitMcpServer::tool_defs(),
    );
    push(
        "giap-context",
        crate::context::ContextMcpServer::tool_defs(),
    );
    push(
        "giap-orchestrator",
        crate::orchestrator::OrchestratorMcpServer::tool_defs(),
    );

    out
}

/// One entry of the pinned prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixEntry {
    /// Prefixed name, as the model sees it.
    pub name: String,
    /// Serialized JSON length of the whole tool object.
    pub bytes: usize,
}

/// The tool block in prompt order, with each entry's serialized size.
///
/// `groups` is the set whose tools are offered; passing every group yields the
/// `"all"` surface. Ordering matches what the provider shim applies before the
/// payload goes out (`prefix_sort_key`, then name).
pub fn ordered_tools(groups: &[&str]) -> Vec<PrefixEntry> {
    let mut kept: Vec<(String, usize)> = all_tools()
        .into_iter()
        .filter(|(ext, _)| groups.contains(ext))
        .map(|(ext, t)| {
            let name = format!("{ext}__{}", t.name);
            let bytes = serde_json::to_string(&t).map(|s| s.len()).unwrap_or(0);
            (name, bytes)
        })
        .collect();

    kept.sort_by(|a, b| prefix_sort_key(&a.0).cmp(&prefix_sort_key(&b.0)));

    kept.into_iter()
        .map(|(name, bytes)| PrefixEntry { name, bytes })
        .collect()
}

/// Total serialized bytes of a tool block — the number the prompt budget spends.
pub fn total_bytes(entries: &[PrefixEntry]) -> usize {
    entries.iter().map(|e| e.bytes).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nine groups registered by default. `giap-context` and
    /// `giap-orchestrator` exist but are off, so they are not in the prefix.
    const DEFAULT_GROUPS: &[&str] = &[
        "giap-memory",
        "giap-weather",
        "giap-system",
        "giap-schedule",
        "giap-device",
        "giap-device-control",
        "giap-sensors",
        "giap-knowledge",
        "giap-toolkit",
    ];

    /// **The fixture.** Names in prompt order, with serialized sizes.
    ///
    /// Regenerate deliberately, never reflexively — see the module docs. A
    /// change here is a statement that the next cold turn on the Orin pays for
    /// a moved prefix.
    #[test]
    fn the_default_tool_prefix_is_unchanged() {
        let entries = ordered_tools(DEFAULT_GROUPS);
        let rendered: Vec<String> = entries
            .iter()
            .map(|e| format!("{} {}", e.bytes, e.name))
            .collect();

        // Core groups first (`prefix_sort_key` tier 0), then by name. Two
        // conversations that differ only in non-core groups share everything
        // above the first tier-1 entry.
        let expected_head = [
            "giap-memory__forget_memory",
            "giap-memory__recall_memories",
            "giap-memory__save_memory",
            "giap-system__get_current_time",
            "giap-system__get_system_info",
            "giap-system__send_notification",
            "giap-toolkit__enable_tool_group",
            "giap-toolkit__list_tool_groups",
        ];
        let head: Vec<&str> = entries
            .iter()
            .take(expected_head.len())
            .map(|e| e.name.as_str())
            .collect();
        assert_eq!(
            head,
            expected_head,
            "the core block moved. Two conversations share the preamble only up \
             to their first difference, so anything that reorders this costs \
             every warm turn its KV prefix.\nfull order:\n{}",
            rendered.join("\n")
        );

        // The whole block, as one number. Tighter than a count and looser than
        // pinning schema text: a reworded description is allowed, a bigger one
        // is not silent.
        let total = total_bytes(&entries);
        assert_eq!(
            total,
            14_478,
            "the default tool block is now {total} raw bytes, was 14,478. That is \
             the PRE-minification form (see the module docs); the shim ships \
             about 7.7% less. If the change is deliberate, update this number and \
             say why in the commit.\nfull order:\n{}",
            rendered.join("\n")
        );

        assert_eq!(entries.len(), 27, "tool count moved");
    }

    /// The property the ordering exists for, asserted rather than assumed:
    /// dropping a non-core group must not disturb the core block.
    #[test]
    fn dropping_a_non_core_group_leaves_the_core_block_byte_identical() {
        let full = ordered_tools(DEFAULT_GROUPS);
        let narrowed: Vec<&str> = DEFAULT_GROUPS
            .iter()
            .copied()
            .filter(|g| *g != "giap-weather")
            .collect();
        let narrow = ordered_tools(&narrowed);

        let shared = narrow
            .iter()
            .zip(full.iter())
            .take_while(|(a, b)| a == b)
            .count();

        // Everything before the first non-core entry must survive unchanged.
        let core_len = full
            .iter()
            .take_while(|e| prefix_sort_key(&e.name).0 == 0)
            .count();
        assert!(core_len > 0, "no core tools — the sort key stopped working");
        assert!(
            shared >= core_len,
            "narrowing moved the prefix at entry {shared}, inside the core block \
             of {core_len}. Core-first ordering is what makes a narrowed \
             conversation reuse a wide one's KV cache"
        );
    }

    /// Vacuity control: the enumeration must actually reach the routers.
    #[test]
    fn the_oracle_reads_real_tools_not_an_empty_list() {
        let all = all_tools();
        assert!(
            all.len() >= 27,
            "only {} tools enumerated — a router stopped being reachable and \
             every assertion above would pass by being empty",
            all.len()
        );
        assert!(
            all.iter().all(|(_, t)| !t.name.is_empty()),
            "a tool came back unnamed"
        );
    }
}
