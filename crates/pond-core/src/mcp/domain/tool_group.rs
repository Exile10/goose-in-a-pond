//! The catalog of GIAP tool GROUPS — one entry per `giap-*` MCP extension.
//!
//! A group is the unit of tool-relevance selection (Phase D). Selection scores
//! the session's opening context against one short natural-language description
//! per group, not per tool: 15 comparisons instead of 59, and the descriptions
//! read like the sentences a user would actually say.
//!
//! ## Why groups and not tools
//!
//! Tool schemas are indivisible in the prompt — a model that can see
//! `create_schedule` but not `list_schedules` is worse off than one that sees
//! neither, because it will invent the missing call. Extensions are already the
//! cohesive unit (one MCP server, one capability area), so they are the unit
//! that goes in or stays out.
//!
//! ## Cost
//!
//! Every tool schema costs roughly 100 tokens through the Gemma chat template,
//! re-prefilled on every fresh turn. The full 59-tool surface is ~5.9K prompt
//! tokens against an 8K-class on-device budget, which is why this exists.

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

/// Why each core group is core — kept next to the data so the decision is
/// reviewable rather than folklore.
///
/// - `giap-draft`: the confirmation/safety surface. It is the one extension
///   registered unconditionally (not behind an `ext_*_enabled` toggle), and a
///   model that can start a risky action but cannot route it through a draft is
///   strictly less safe. Never selectable away.
/// - `giap-memory`: cross-cutting. "remember that…" / "what did I say about…"
///   can attach to ANY topic, so no opening message reliably predicts it, and
///   losing it degrades the assistant's core promise.
/// - `giap-system`: contains `get_current_time`. Time is asked constantly, in
///   passing, mid-conversation, and is not predictable from the first message of
///   a session.
/// - `giap-toolkit`: the escape hatch itself. Removing it would make narrowing
///   irreversible within a session, which is the one thing that would make this
///   feature unsafe.
pub const CORE_RATIONALE: &str = "draft=safety, memory=cross-cutting, system=time, toolkit=escape";

/// The extension providing the discovery / enable escape hatch.
pub const TOOLKIT_EXTENSION: &str = "giap-toolkit";

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
        description: "Reminders, alarms, timers, recurring routines and scheduled tasks: create a \
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
                      articles, definitions of words, books and authors, and short factual answers \
                      about history, science, geography, people and places.",
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
        description: "Looking things up about places and countries: facts about a country, nearby \
                      places and points of interest, postcodes and locations, and general web \
                      search when nothing else fits.",
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
];

/// Look up a group by extension name.
pub fn find_group(extension: &str) -> Option<&'static ToolGroup> {
    TOOL_GROUPS.iter().find(|g| g.extension == extension)
}

/// Extension names of the always-on core groups.
/// Groups an unidentified speaker must never be given, whatever the scorer says.
///
/// PAI-1 P5. Suppressing memory *injection* for a guest is only half a boundary:
/// the model can be asked to call `recall_memories` and read the household's
/// memory directly, or `forget_memory` and destroy it. Neither tool has any
/// notion of a session, so the only place to stop it is before the guest's
/// session is given the group at all.
///
/// `giap-memory` and `giap-draft` are both `core`, so `select_groups` will
/// always put them back -- the caller has to subtract this list *after*
/// selection, not filter the candidates going in.
///
/// Deliberately a denylist, not an allowlist. A new group is far more likely to
/// be neutral (weather, news, a unit converter) than personal, and a new
/// *personal* group is exactly the kind of change whose author should have to
/// think about this list. An allowlist would silently deny every new group to
/// guests and be discovered as a bug report.
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
    ]
}

/// Groups a SUBAGENT must never be given, whatever role asked for it and
/// however wide its parent was.
///
/// PAI-6 P3. This is the same move PAI-1 P5 made for guests, for the same
/// reason: **when the thing you want to check has no identity, move the check
/// to the layer that hands it out.** A subagent has no identity two separate
/// controls need:
///
/// - Its tool calls carry the CHILD's engine session id in `agent-session-id`,
///   and nothing writes that id into `engine_session_map`. So
///   `RepoDraftAuthority::actor_for_engine_session` returns `None`,
///   `is_draft_decision_permitted` answers `REASON_UNRESOLVED_ACTOR`, and under
///   the DEFAULT `PolicyMode::Audit` that **proceeds and logs**. A subagent
///   could approve any staged action on a default install.
/// - It runs under `GooseMode::Auto`, which is mandatory rather than chosen:
///   any approval-requiring mode hangs forever on the child's
///   `confirmation_rx`, because nothing forwards an ActionRequired message to a
///   parent. So a subagent cannot be gated by approval at all, and its tool set
///   is its only boundary.
///
/// Withholding is therefore the enforcement, not a substitute for it.
/// Deliberately a denylist for the same reason as the guest one, and
/// deliberately applied to the derived set rather than to the role's request:
/// `giap-draft`, `giap-system` and `giap-toolkit` are all `core`, so anything
/// that re-runs selection would put them straight back.
///
/// The cost is real and is accepted: a subagent has no clock, because
/// `get_current_time` lives in `giap-system` next to `write_file`. A role that
/// needs the date should be given it in its instructions.
pub fn groups_denied_to_subagents() -> &'static [&'static str] {
    &[
        // `approve_draft`/`reject_draft` DECIDE, and the gate that would check
        // who decided cannot resolve a subagent (see above).
        "giap-draft",
        // `enable_tool_group` WIDENS an allow-set keyed by the process-global
        // `current_session_id()`, which a child does not own. A child holding
        // this could widen its own narrowing -- or its parent's. This is the
        // most direct breach of invariant 1 available anywhere in the tree.
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

    /// The three the list exists for, named individually so removing one is a
    /// deliberate edit with a failing test rather than a quiet deletion.
    ///
    /// Each is here for a mechanism, not a vibe: `giap-draft` because the draft
    /// gate cannot resolve a subagent actor and audit mode proceeds;
    /// `giap-toolkit` because `enable_tool_group` widens an allow-set keyed by
    /// the process-global session id; `giap-device-control` because a subagent
    /// is forced to `GooseMode::Auto` and has no approval path left once draft
    /// is gone.
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
