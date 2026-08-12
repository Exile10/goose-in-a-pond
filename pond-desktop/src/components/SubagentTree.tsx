import { CornerDownRight, Wrench } from "lucide-react";
import type { ChatEvent, SubagentStatus } from "../api/types";

/**
 * PAI-6 P6. The client half of subagent progress.
 *
 * A delegating turn used to be a spinner and nothing else: the parent is parked
 * inside one `delegate` tool call for the whole of a child's run — minutes, on a
 * Jetson — and until this phase nothing reached the browser until the tool
 * result did. The server now streams a `subagent_progress` frame per lifecycle
 * transition and per tool the child calls, and this is what draws them.
 *
 * Three decisions worth keeping:
 *
 * 1. **The fold lives here, not in a chat surface.** Both surfaces render this
 *    tree, and a fold copied into each is the drift this repository keeps
 *    paying for. `applySubagentProgress` is the only place a frame becomes
 *    state.
 * 2. **Grouped by `task_id`, not by role.** One turn may delegate the same role
 *    twice; grouping by name would merge two runs into one node whose status
 *    flickers between them.
 * 3. **It renders WHILE streaming.** Every other note on a message
 *    (`turn_limit`, the context-pressure line) is gated on `!streaming`,
 *    because it is about a finished turn. This one is the opposite: a tree
 *    nobody sees until the turn ends is the spinner again.
 */

export interface SubagentRun {
  taskId: string;
  role: string;
  status: SubagentStatus;
  /** Tool names, in the order the child called them. */
  tools: string[];
  /** The pond's own reason, when a run failed. Never the child's text. */
  detail?: string;
}

/**
 * Fold one `subagent_progress` frame into the runs of a turn.
 *
 * Returns the same array when the frame is not one (or carries no run to
 * attach to), so a caller can hand it every event without branching twice.
 */
export function applySubagentProgress(
  runs: SubagentRun[],
  ev: ChatEvent,
): SubagentRun[] {
  if (ev.type !== "subagent_progress" || !ev.task_id || !ev.status) return runs;

  const taskId = ev.task_id;
  const at = runs.findIndex((run) => run.taskId === taskId);
  const existing: SubagentRun = runs[at] ?? {
    taskId,
    role: ev.role ?? "subagent",
    status: ev.status,
    tools: [],
  };

  // A `tool` frame says what the child is DOING; every other status says where
  // the run has got to. Only the second kind touches `detail`, and it
  // overwrites rather than merging: `detail` is the reason a run ended, and
  // carrying a previous one forward would caption a finished run with the
  // sentence from an earlier failure. The render below has no second gate for
  // that -- clearing here is the only mechanism, so it is the one thing tested.
  const next: SubagentRun =
    ev.status === "tool"
      ? {
          ...existing,
          tools: ev.detail ? [...existing.tools, ev.detail] : existing.tools,
        }
      : { ...existing, status: ev.status, detail: ev.detail };

  if (at < 0) return [...runs, next];
  const out = [...runs];
  out[at] = next;
  return out;
}

/** Plain language for a status, and a miss shows up as the raw value. */
const STATUS_TEXT: Record<SubagentStatus, string> = {
  queued: "waiting its turn",
  running: "working",
  tool: "working",
  completed: "done",
  cancelled: "stopped",
  turn_budget_exhausted: "ran out of steps",
  failed: "could not finish",
};

/** The last segment of an `extension__tool` name, which is what a person reads. */
function bareToolName(name: string): string {
  const bare = name.includes("__") ? name.split("__").pop()! : name;
  return bare.replace(/_/g, " ");
}

export function SubagentTree({ runs }: { runs: SubagentRun[] }) {
  if (runs.length === 0) return null;
  return (
    <div className="subagent-tree" role="list" aria-label="Delegated tasks">
      {runs.map((run) => (
        <div className="subagent-tree__run" role="listitem" key={run.taskId}>
          <span className="subagent-tree__head">
            <CornerDownRight size={11} aria-hidden />
            <span className="subagent-tree__role">{run.role}</span>
            <span className="subagent-tree__status">
              {STATUS_TEXT[run.status] ?? run.status}
            </span>
          </span>
          {run.tools.length > 0 && (
            <span className="subagent-tree__tools">
              {run.tools.map((tool, i) => (
                <span className="subagent-tree__tool" key={`${tool}-${i}`}>
                  <Wrench size={10} aria-hidden />
                  {bareToolName(tool)}
                </span>
              ))}
            </span>
          )}
          {run.detail && (
            <span className="subagent-tree__detail">{run.detail}</span>
          )}
        </div>
      ))}
    </div>
  );
}
