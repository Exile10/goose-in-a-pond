import type { ChatEvent, SubagentStatus } from "../api/types";

/** Folds `subagent_progress` frames into a turn's runs. The only fold (`applySubagentProgress`),
 *  keyed by `task_id` because a role can repeat; `SubagentTree` draws the result. */

export interface SubagentRun {
  taskId: string;
  role: string;
  status: SubagentStatus;
  /** Tool names, in the order the child called them. */
  tools: string[];
  /** The pond's own reason, when a run failed. Never the child's text. */
  detail?: string;
}

/** Folds a `subagent_progress` frame into a turn's runs; other events return `runs` as-is. */
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

  // `tool` frames only append to `tools`. Other statuses overwrite `detail` (never merge): it is
  // why a run ended, and nothing else clears a stale one.
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
