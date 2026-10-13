// What each background job is waiting for, shared by both settings surfaces. Run now only wakes
// the job's loop (it still queues for the one inference slot), hence "Asked", not "Done".

import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../api/PondApiClient";
import type { LaneStatus } from "../api/types";
import { describeLastRun, describeWait, describeElapsed, describeHistory } from "./backgroundJobsView";
import "./background-jobs.css";

/** Matches Logs.tsx; the lane's gates move on 60s to 15-minute cadences, so faster buys nothing. */
const POLL_MS = 5_000;

/** Per-row transient state. One row can be asked while another is idle. */
type RowState = "idle" | "asking" | "asked" | "nothing" | "failed";

export interface BackgroundJobsProps {
  /** Renders as a plain block instead of a titled card. The hub panel has its own heading. */
  bare?: boolean;
}

export function BackgroundJobs({ bare = false }: BackgroundJobsProps) {
  const [status, setStatus] = useState<LaneStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [rows, setRows] = useState<Record<string, RowState>>({});

  // One request at a time, or a stalled pond stacks a tick every 5s until the 30s client abort.
  const inFlight = useRef(false);

  // A `silent` (polled) read that fails keeps the last good answer rather than blanking the panel.
  const load = useCallback(async (silent = false) => {
    if (inFlight.current) return;
    inFlight.current = true;
    try {
      const next = await api.laneStatus();
      // `request<T>` may return undefined or a parsed index.html (server starting, or a proxy).
      if (!next || typeof next.lane !== "boolean") return;
      setStatus(next);
      setError(null);
    } catch (e) {
      if (silent) return;
      // A silent empty panel would read as "no background jobs", so a failed load must say so.
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
    }
  }, []);

  useEffect(() => {
    void load();

    // Skip ticks while hidden (a kitchen panel stays open for days); catch up when shown.
    const tick = () => {
      if (document.visibilityState !== "visible") return;
      void load(true);
    };
    const id = setInterval(tick, POLL_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load(true);
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearInterval(id);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [load]);

  const run = useCallback(
    async (job: string) => {
      setRows((r) => ({ ...r, [job]: "asking" }));
      try {
        const result = await api.runLaneJob(job);
        setRows((r) => ({ ...r, [job]: result.woken ? "asked" : "nothing" }));
      } catch (e) {
        setRows((r) => ({ ...r, [job]: "failed" }));
        setError(e instanceof Error ? e.message : String(e));
        return;
      }
      // Re-read rather than patch from the response: it only says the job was woken.
      await load();
    },
    [load],
  );

  const body = (
    <div className="bgjobs">
      {/* `status`, not `alert`. A background-jobs read that failed is worth
          saying and is not worth interrupting for -- and this panel shares a
          page with the settings error banner, which IS an alert. Two alerts on
          one screen is how the urgent one stops being urgent. */}
      {error !== null && (
        <p className="bgjobs__error" role="status">
          Could not read the background jobs. {error}
        </p>
      )}

      {status !== null && !status.lane && (
        <p className="bgjobs__note">
          This pond is not running the background jobs — nothing here schedules them.
        </p>
      )}

      {status?.lane && (
        <>
          {/* The watcher. `slot_busy` alone could only ever say something was
              running; this says what, and for how long — which is the
              difference between "the pond is busy" and "the memory engine is
              reading your conversations", and the second is what tells somebody
              whether to wait or to go and turn something off.

              `aria-live="polite"` so a screen reader hears a job start and
              finish without being interrupted mid-sentence by a five-second
              tick. */}
          <p className="bgjobs__now" data-running={status.running ? "" : undefined} aria-live="polite">
            {status.running
              ? `Running now: ${status.running_title ?? status.running}${describeElapsed(status.running_for_secs)}`
              : "Nothing is running. Jobs take turns, one at a time."}
          </p>

          <ul className="bgjobs__list">
            {status.jobs.map((job) => (
              <li key={job.job} className="bgjobs__row" data-present={job.present || undefined}>
                <div className="bgjobs__text">
                  <span className="bgjobs__title">{job.title}</span>
                  {/* Two sentences, not a metadata strip joined by a middle
                      dot. They are independent facts -- what it is waiting for,
                      and when it last managed to run -- and the second is the
                      one that makes the first mean something: "waiting for the
                      house to be quiet" reads very differently under "ran 5
                      minutes ago" than under "hasn't run yet". */}
                  <span className="bgjobs__wait">
                    {describeWait(job)}. {describeLastRun(job.since_last_run_secs)}.
                    {describeHistory(job)}
                  </span>
                </div>
                <div className="bgjobs__act">
                  <button
                    type="button"
                    className="bgjobs__run"
                    // Disabled, not hidden, with no loop here: the row still shows the job exists.
                    disabled={!job.present || rows[job.job] === "asking"}
                    onClick={() => void run(job.job)}
                  >
                    {rows[job.job] === "asking" ? "Asking…" : "Run now"}
                  </button>
                  {rows[job.job] === "asked" && (
                    <span className="bgjobs__said" role="status">
                      Asked — it runs at its next turn
                    </span>
                  )}
                  {rows[job.job] === "nothing" && (
                    <span className="bgjobs__said" role="status">
                      Nothing here to run
                    </span>
                  )}
                  {rows[job.job] === "failed" && (
                    <span className="bgjobs__said" role="status">
                      That didn't send
                    </span>
                  )}
                </div>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );

  if (bare) return body;
  return (
    <section className="bgjobs__card">
      <h3 className="bgjobs__heading">Background jobs</h3>
      <p className="bgjobs__sub">
        What the pond does while nobody is talking to it. They share one slot, so they
        take turns.
      </p>
      {body}
    </section>
  );
}
