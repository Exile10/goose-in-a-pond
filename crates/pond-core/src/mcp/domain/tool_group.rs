//! The catalog of GIAP tool GROUPS — one entry per `giap-*` MCP extension, and the unit of
//! tool-relevance selection (Phase D): 15 comparisons instead of 59, scored against one short
//! natural-language description per group. Groups rather than tools because a tool schema is
//! indivisible in the prompt and costs ~100 tokens re-prefilled per turn on an 8K-class budget.

/// A selectable group of tools, backed by exactly one `giap-*` MCP extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolGroup {
    /// The MCP extension name, e.g. `"giap-weather"`. Tool names are
    /// `"<extension>__<tool>"`, which is how a tool is mapped back to a group.
    pub extension: &'static str,
    /// Natural-language description of what the group is FOR, phrased the way a
    /// user would ask for it. This string is what gets embedded and scored, so
    /// it should read like example requests, not like an API summary.
    pub description: &'static str,
    /// Always loaded, never scored, never removable. See [`CORE_RATIONALE`].
    pub core: bool,
}

/// Why each core group is core, kept next to the data so the decision is reviewable.
/// `giap-draft` is the confirmation/safety surface, the one extension registered unconditionally;
/// `giap-memory` is cross-cutting and no opening message predicts it; `giap-system` holds
/// `get_current_time`; `giap-toolkit` is the escape hatch that keeps narrowing reversible.
pub const CORE_RATIONALE: &str = "draft=safety, memory=cross-cutting, system=time, toolkit=escape";

/// The extension providing the discovery / enable escape hatch.
pub const TOOLKIT_EXTENSION: &str = "giap-toolkit";

/// The extension carrying the `delegate` tool (PAI-6 P5). A const because three crates spell this
/// name and a typo is silent: an extension matching no catalog entry is treated as a user-added
/// MCP server, which selection never narrows. `tests/registration_matches_the_catalog.rs` resolves
/// the known consts by name and fails on any argument it cannot resolve.
pub const ORCHESTRATOR_EXTENSION: &str = "giap-orchestrator";

/// Separator between the extension name and the tool name in a prefixed tool
/// name (`giap-weather__get_forecast`). Goose's own convention.
pub const TOOL_NAME_SEPARATOR: &str = "__";

/// Every group GIAP knows about. Registration is still gated by the
/// `ext_*_enabled` settings toggles — this catalog describes what COULD be
/// registered, and selection always intersects it with what actually was.
pub const TOOL_GROUPS: &[ToolGroup] = &[
    ToolGroup {
        extension: "giap-draft",
        description: "Pending actions awaiting the user's confirmation: save a draft of a risky \
                      or irreversible action, list what is waiting, approve it once the user \
                      agrees, or reject it if they decline.",
        core: true,
    },
    ToolGroup {
        extension: "giap-memory",
        description: "The user's long-term memories: remember a fact or preference about them, \
                      recall what they have told you before, or forget something they no longer \
                      want kept.",
        core: true,
    },
    ToolGroup {
        extension: "giap-system",
        description: "This machine and the current moment: the date, time and timezone, operating \
                      system and hostname, memory and disk usage, desktop notifications, reading \
                      and writing local files, and running shell commands.",
        core: true,
    },
    ToolGroup {
        extension: TOOLKIT_EXTENSION,
        description: "Which groups of tools are loaded for this conversation, and loading another \
                      group when a capability you need is not currently available.",
        core: true,
    },
    ToolGroup {
        extension: "giap-schedule",
        // This sentence promised "timers" for months while a one-shot was not
        // expressible: `SchedulerPort` was cron-only, and a 6-field cron has no
        // year field, so "in ten minutes" became an annual alarm or nothing at
        // all. `set_timer` is what makes the first clause true.
        description: "Reminders, alarms, timers, recurring routines and scheduled tasks: set a \
                      one-shot timer for a few minutes or hours from now, create a repeating \
                      schedule, list or inspect what is scheduled, change or pause or delete one, \
                      run one now, and review past runs. Anything about doing something later or \
                      every day at a certain time.",
        core: false,
    },
    ToolGroup {
        extension: "giap-weather",
        description: "The weather: current conditions and the forecast for the days ahead, \
                      temperature, rain, whether to take an umbrella, for here or another place.",
        core: false,
    },
    ToolGroup {
        extension: "giap-knowledge",
        description: "General reference knowledge and factual lookup: encyclopedia and Wikipedia \
                      articles, definitions of words, books and authors, short factual answers \
                      about history, science, geography, people and places, and computed answers \
                      such as arithmetic, unit and currency conversion, dates and statistics.",
        core: false,
    },
    ToolGroup {
        extension: "giap-device",
        description: "The smart-home devices in this house: which lights, plugs, sensors, \
                      thermostats and appliances are registered, which rooms they are in, whether \
                      they are online, and what state they report.",
        core: false,
    },
    ToolGroup {
        extension: "giap-device-control",
        description: "Actually operating the smart-home devices: turning a light or plug or \
                      appliance on and off, changing brightness or colour or temperature, opening \
                      or closing something, setting a device to a new state.",
        core: false,
    },
    ToolGroup {
        extension: "giap-news",
        description:
            "The news: today's headlines, top stories, and searching recent news coverage \
                      about a topic, company, country or person.",
        core: false,
    },
    ToolGroup {
        extension: "giap-finance",
        description: "Money and markets: stock and share prices, cryptocurrency prices, and \
                      currency exchange rates between two currencies.",
        core: false,
    },
    ToolGroup {
        extension: "giap-discovery",
        description: "Reference data about countries and shop products: a country's population, \
                      capital, currency, languages and region; a packaged food looked up by \
                      barcode or name, with its ingredients and nutrition; and crowdsourced \
                      prices for one. This group does NOT search the web.",
        core: false,
    },
    ToolGroup {
        extension: "giap-audit",
        description: "The privacy and activity audit trail: what this assistant has recorded and \
                      done, which data left the device, and privacy questions about what is stored \
                      and why.",
        core: false,
    },
    ToolGroup {
        extension: "giap-vision",
        description: "What the cameras have seen: recent camera events, people or motion detected \
                      at the door or in a room, and describing what is in a captured snapshot.",
        core: false,
    },
    ToolGroup {
        extension: "giap-sensors",
        description:
            "Sensor readings over time: temperature, humidity, air quality, power use and \
                      other measurements from sensors in the house, their latest values and their \
                      history.",
        core: false,
    },
    ToolGroup {
        extension: "giap-context",
        description: "The speaker's own personal context, from sources they connected: what a \
                      camera or sensor of theirs has recorded, searched by meaning or listed by \
                      recency. Read-only, and scoped to whoever is speaking.",
        core: false,
    },
    ToolGroup {
        extension: ORCHESTRATOR_EXTENSION,
        description: "Handing a piece of work to a named specialist agent that runs on its own \
                      and reports back: research a question in depth, work through a longer task \
                      under a saved role, or have a second agent do something while this \
                      conversation carries on.",
        core: false,
    },
];

/// Sort key putting the tools every turn carries before the ones it might not. Two turns share a
/// prompt prefix only up to their first difference, and tool schemas are the bulk of it: on the
/// Gemma template, chats differing in half their tools shared 70% of the preamble with those tools
/// first and 85% with them last. Core groups sort first, then by name: order must be deterministic.
pub fn prefix_sort_key(tool_name: &str) -> (u8, &str) {
    // Tool names are `<extension>__<tool>`; the extension is what maps to a
    // group. An unknown prefix (a user-added MCP server) ranks with the
    // non-core tools, which is right: nothing guarantees it is there next turn.
    let extension = tool_name.split("__").next().unwrap_or("");
    let tier = match find_group(extension) {
        Some(group) if group.core => 0,
        _ => 1,
    };
    (tier, tool_name)
}

/// Look up a group by extension name.
pub fn find_group(extension: &str) -> Option<&'static ToolGroup> {
    TOOL_GROUPS.iter().find(|g| g.extension == extension)
}

/// Extension names of the always-on core groups.
/// Groups an unidentified speaker must never be given (PAI-1 P5): a guest can still ask for
/// `recall_memories` or `forget_memory`, and neither tool knows about sessions. `select_groups`
/// puts `core` groups back, so subtract this list AFTER selection; a denylist, so new groups pass.
pub fn groups_denied_to_guests() -> &'static [&'static str] {
    &[
        // Reads and deletes the household's long-term memory.
        "giap-memory",
        // Approves and rejects staged actions -- and `approve_draft` performs
        // no ownership check of its own.
        "giap-draft",
        // What data left the device, and when. A visitor's business it is not.
        "giap-audit",
        // Who has been seen on camera, and when.
        "giap-vision",
        // Sensor history: when the house was empty, when somebody came home.
        "giap-sensors",
        // PAI-8. A member's own connected sources. Invariant 2 is that a Guest sees no context
        // items, and the tool layer enforces that by scope; this entry stops the tool being
        // OFFERED, which on a small model saves a turn spent discovering the refusal.
        "giap-context",
        // PAI-6 P5, and the only entry here not about reading personal data. `delegate` starts an
        // autonomous multi-turn agent under `GooseMode::Auto` on the household's own hardware,
        // which on a Jetson is the single GPU the household's next turn needs. The child inherits
        // the guest's scope, so what is withheld is the device, not the memory.
        ORCHESTRATOR_EXTENSION,
    ]
}

/// Groups a SUBAGENT must never be given, whatever role asked and however wide its parent was.
/// PAI-6 P3: a subagent has no identity, so the draft gate answers `REASON_UNRESOLVED_ACTOR` and
/// the default `PolicyMode::Audit` proceeds, and mandatory `GooseMode::Auto` has no approval path.
/// The tool set is the only boundary, so subtract this from the DERIVED set or core groups return.
pub fn groups_denied_to_subagents() -> &'static [&'static str] {
    &[
        // `approve_draft`/`reject_draft` DECIDE, and the gate that would check
        // who decided cannot resolve a subagent (see above).
        "giap-draft",
        // `enable_tool_group` WIDENS an allow-set, and a child has nothing to widen: its whole
        // grant is published up front by `narrow_child_groups` and bounded by the parent's
        // entitlement. Offering a 2-4B model a tool whose every call is refused is not harmless.
        TOOLKIT_EXTENSION,
        // Actuates the house. There is no approval path for a subagent, and the
        // one it would otherwise take -- staging a draft -- is denied above.
        "giap-device-control",
        // `write_file`, `run_shell_command` and `send_notification`. Writes and
        // executes, with no approval path.
        "giap-system",
        // Schedules future work that will run with the household's authority,
        // long after the delegation that created it has ended.
        "giap-schedule",
        // PAI-6 P5. Depth already refuses a subagent's `delegate` with `DepthExceeded`; this
        // entry stops the tool being OFFERED, because a 2-4B model handed a tool whose every
        // call is refused spends its turn budget discovering that.
        ORCHESTRATOR_EXTENSION,
    ]
}

pub fn core_group_names() -> Vec<&'static str> {
    TOOL_GROUPS
        .iter()
        .filter(|g| g.core)
        .map(|g| g.extension)
        .collect()
}

/// The group a prefixed tool name belongs to (`giap-weather__get_forecast` →
/// `giap-weather`). `None` for an unprefixed name.
pub fn group_of_tool(tool_name: &str) -> Option<&str> {
    tool_name
        .find(TOOL_NAME_SEPARATOR)
        .map(|sep| &tool_name[..sep])
}

/// Whether `extension` is a GIAP builtin the catalog knows about. Anything else
/// (a user-added external MCP server) is never narrowed by selection: the user
/// added it deliberately and GIAP has no description to score it against.
pub fn is_catalog_extension(extension: &str) -> bool {
    find_group(extension).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_no_duplicate_extensions() {
        let mut names: Vec<&str> = TOOL_GROUPS.iter().map(|g| g.extension).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate extension in TOOL_GROUPS");
    }

    /// The four core groups are load-bearing for safety, continuity, and the
    /// escape hatch. A change here should be deliberate, so pin it.
    #[test]
    fn core_groups_are_exactly_the_documented_four() {
        let mut core = core_group_names();
        core.sort_unstable();
        assert_eq!(
            core,
            vec!["giap-draft", "giap-memory", "giap-system", "giap-toolkit"]
        );
    }

    #[test]
    fn descriptions_are_substantive_enough_to_embed() {
        for g in TOOL_GROUPS {
            assert!(
                g.description.len() > 60,
                "{} has a description too short to score meaningfully",
                g.extension
            );
        }
    }

    #[test]
    fn tool_names_map_back_to_their_group() {
        assert_eq!(
            group_of_tool("giap-weather__get_forecast"),
            Some("giap-weather")
        );
        assert_eq!(group_of_tool("platform__manage_schedule"), Some("platform"));
        assert_eq!(group_of_tool("unprefixed"), None);
    }

    #[test]
    fn only_catalog_extensions_are_recognised() {
        assert!(is_catalog_extension("giap-vision"));
        assert!(!is_catalog_extension("some-user-mcp-server"));
    }
}

#[cfg(test)]
mod prefix_order_tests {
    use super::*;

    fn ordered(mut names: Vec<&str>) -> Vec<&str> {
        names.sort_by_key(|n| prefix_sort_key(n));
        names
    }

    /// The property the on-disk KV snapshot depends on: the tools every turn
    /// carries come first, so they form a prefix two turns can share even when
    /// the rest of their selection differs.
    #[test]
    fn core_tools_come_before_the_ones_a_turn_might_not_have() {
        let got = ordered(vec![
            "giap-weather__get_current_weather",
            "giap-draft__list_drafts",
            "giap-news__headlines",
            "giap-toolkit__enable_tool_group",
        ]);
        let first_two: Vec<&str> = got.iter().take(2).copied().collect();
        assert_eq!(
            first_two,
            vec!["giap-draft__list_drafts", "giap-toolkit__enable_tool_group"],
            "core groups must lead, or a turn that drops weather truncates the shared \
             prefix at the first tool"
        );
    }

    /// Two turns whose selections differ must still agree for the whole core
    /// block. This is the measurement that motivated the change, as a property.
    #[test]
    fn two_different_selections_agree_for_their_whole_core_block() {
        let a = ordered(vec![
            "giap-weather__get_current_weather",
            "giap-draft__list_drafts",
            "giap-toolkit__enable_tool_group",
        ]);
        let b = ordered(vec![
            "giap-news__headlines",
            "giap-draft__list_drafts",
            "giap-toolkit__enable_tool_group",
        ]);
        let shared = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        assert_eq!(
            shared, 2,
            "the two core tools must be a common prefix of both selections; got {a:?} vs {b:?}"
        );
    }

    /// Deterministic within a tier. A set rendered in a different order on two
    /// turns shares nothing, whatever the tiering does.
    #[test]
    fn the_order_is_stable_whatever_order_the_selection_arrives_in() {
        let forward = ordered(vec![
            "giap-draft__list_drafts",
            "giap-weather__get_current_weather",
            "giap-news__headlines",
        ]);
        let backward = ordered(vec![
            "giap-news__headlines",
            "giap-weather__get_current_weather",
            "giap-draft__list_drafts",
        ]);
        assert_eq!(forward, backward);
    }

    /// A user-added MCP server ranks with the removable tools. Nothing promises
    /// it is there next turn, so leading with it would truncate the prefix for
    /// every turn that lacks it.
    #[test]
    fn an_unknown_extension_does_not_lead() {
        let got = ordered(vec![
            "some-user-server__do_thing",
            "giap-draft__list_drafts",
        ]);
        assert_eq!(got.first(), Some(&"giap-draft__list_drafts"));
    }
}

#[cfg(test)]
mod guest_denylist_tests {
    use super::*;

    #[test]
    fn every_denied_group_actually_exists() {
        for name in groups_denied_to_guests() {
            assert!(
                TOOL_GROUPS.iter().any(|g| g.extension == *name),
                "denylist names a group that does not exist: {name} -- a typo here \
                 silently grants a guest the access it was meant to deny"
            );
        }
    }

    /// The denylist is only useful because it removes groups `select_groups`
    /// puts back. If none of them were core, the list would be doing nothing
    /// the scorer was not already doing.
    #[test]
    fn the_denylist_covers_groups_that_are_otherwise_unremovable() {
        let core = core_group_names();
        let denied_core: Vec<_> = groups_denied_to_guests()
            .iter()
            .filter(|n| core.contains(n))
            .collect();
        assert!(
            !denied_core.is_empty(),
            "no denied group is core, so subtracting after selection is pointless"
        );
        assert!(
            denied_core.contains(&&"giap-memory"),
            "giap-memory is core and reads the household's memory; it must be denied"
        );
    }

    /// A guest is meant to stay useful -- weather, time, knowledge, the lights.
    /// Denying everything would be a boundary nobody keeps switched on.
    #[test]
    fn a_guest_keeps_the_neutral_groups() {
        let denied = groups_denied_to_guests();
        for neutral in [
            "giap-weather",
            "giap-knowledge",
            "giap-device-control",
            "giap-toolkit",
            "giap-system",
        ] {
            assert!(
                !denied.contains(&neutral),
                "{neutral} carries no personal data and a guest should keep it"
            );
        }
    }

    /// Same typo hazard as the guest list, and the same consequence: a name
    /// that matches no group removes nothing.
    #[test]
    fn every_subagent_denied_group_actually_exists() {
        for name in groups_denied_to_subagents() {
            assert!(
                TOOL_GROUPS.iter().any(|g| g.extension == *name),
                "the subagent denylist names a group that does not exist: {name} -- a typo \
                 here silently hands a subagent the access it was meant to withhold"
            );
        }
    }

    /// The three the list exists for, named individually so removing one fails a test rather than
    /// passing quietly. `giap-draft` because the draft gate cannot resolve a subagent actor and
    /// audit mode proceeds; `giap-toolkit` because `enable_tool_group` widens an allow-set;
    /// `giap-device-control` because a subagent is forced to `GooseMode::Auto` with no approval.
    #[test]
    fn the_subagent_denylist_covers_deciding_widening_and_actuating() {
        let denied = groups_denied_to_subagents();
        for required in ["giap-draft", TOOLKIT_EXTENSION, "giap-device-control"] {
            assert!(
                denied.contains(&required),
                "{required} must be withheld from subagents; see the doc comment for the \
                 mechanism each one breaks"
            );
        }
    }

    /// PAI-6 P5. The delegation surface is withheld from both a guest and a subagent for two
    /// different mechanisms, and neither list backstops the other, so both are named here.
    /// Removing either entry breaks no other test: depth already refuses a subagent's `delegate`,
    /// and only the handler's own scope check refuses a guest's.
    #[test]
    fn neither_a_guest_nor_a_subagent_is_offered_the_delegation_tool() {
        assert!(
            groups_denied_to_subagents().contains(&ORCHESTRATOR_EXTENSION),
            "{ORCHESTRATOR_EXTENSION} must be withheld from subagents: depth already refuses the \
             call, so offering the tool only spends a small model's turn budget discovering that"
        );
        assert!(
            groups_denied_to_guests().contains(&ORCHESTRATOR_EXTENSION),
            "{ORCHESTRATOR_EXTENSION} must be withheld from guests: delegating starts minutes of \
             unattended agent work on the household's own GPU, which an unidentified speaker has \
             no business commanding"
        );
    }

    /// Vacuity control. If the list grew to cover every group, a subagent would
    /// be useless and the narrowing would be indistinguishable from "no
    /// subagents". The research surface the workstream exists for must survive.
    #[test]
    fn a_subagent_keeps_the_read_only_research_groups() {
        let denied = groups_denied_to_subagents();
        for kept in [
            "giap-weather",
            "giap-knowledge",
            "giap-news",
            "giap-finance",
            "giap-device",
            "giap-memory",
        ] {
            assert!(
                !denied.contains(&kept),
                "{kept} reads rather than decides; denying it leaves subagents unable to do \
                 the one job they were built for"
            );
        }
    }
}
