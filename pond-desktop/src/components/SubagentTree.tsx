import { CornerDownRight, Wrench } from "lucide-react";
import type { SubagentStatus } from "../api/types";
import type { SubagentRun } from "./subagentRuns";

/** Draws a turn's subagent runs as a tree, one branch per `task_id` (a role can repeat). It renders
 *  WHILE streaming, unlike other message notes; `subagentRuns.ts` folds the frames it draws. */

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
