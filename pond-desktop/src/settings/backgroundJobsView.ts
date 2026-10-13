import type { LaneJobStatus } from "../api/types";

// What BackgroundJobs says about each job, in household words, kept out of the component so it
// can be tested.

/** Last run in household words. `null` means never: the symptom this screen exists for. */
export function describeLastRun(secs: number | null): string {
  if (secs === null) return "Hasn't run yet";
  if (secs < 90) return "Ran just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `Ran ${mins} minutes ago`;
  const hours = Math.round(mins / 60);
  if (hours < 48) return `Ran ${hours} ${hours === 1 ? "hour" : "hours"} ago`;
  return `Ran ${Math.round(hours / 24)} days ago`;
}

/** What a job waits for, in household words; an unknown reason passes through, never "Ready". */
export function describeWait(job: LaneJobStatus): string {
  if (!job.present) return "Not running on this pond";
  if (!job.registered) return "Starting up";
  if (job.would_run_next) return "Next to run";
  switch (job.blocked_by) {
    case null:
      return "Ready";
    case "disabled":
      return "Turned off in settings";
    case "no_activity_since_start":
      return "Waiting for you to say something first";
    case "still_active":
      return "Waiting for the house to be quiet";
    case "interval_floor":
      return "Ran recently, waiting its turn again";
    default:
      return `Waiting: ${job.blocked_by}`;
  }
}

/** How long the running job has held the slot, as a clause; "" for the first few seconds. */
export function describeElapsed(secs: number | null | undefined): string {
  if (secs === null || secs === undefined || secs < 3) return "";
  if (secs < 60) return `, for ${secs} seconds`;
  const mins = Math.round(secs / 60);
  return `, for ${mins} ${mins === 1 ? "minute" : "minutes"}`;
}

/** History since startup in one clause; it alone tells a starved job from one about to run. */
export function describeHistory(job: LaneJobStatus): string {
  const parts: string[] = [];
  if (job.granted) parts.push(`ran ${job.granted}×`);
  // Names a defect, not a state: eligible but never getting its turn.
  if (job.lost_to_total) {
    const most = job.lost_to_most;
    parts.push(
      most
        ? `waited behind ${most.job.replace(/_/g, " ")} ${job.lost_to_total}×`
        : `waited its turn ${job.lost_to_total}×`,
    );
  }
  if (job.slot_busy) parts.push(`found the pond busy ${job.slot_busy}×`);
  return parts.length ? ` Since starting: ${parts.join(", ")}.` : "";
}
