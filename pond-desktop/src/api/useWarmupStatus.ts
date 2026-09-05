// ────────────────────────────────────────────────────────────
// useWarmupStatus — live view of the boot/model-change prefix warm-up.
//
// Polls GET /api/v1/warmup while the state is "warming" (the model is loading
// and the prompt prefix precompiling) and stops on any terminal state. On any
// error it settles to null and never throws — a pond that cannot report
// warm-up is a pond that chats exactly as before, so the banner just absents
// itself (graceful degradation, same contract as useMemoryStatus).
// ────────────────────────────────────────────────────────────

import { useState, useEffect } from "react";
import { api } from "./PondApiClient";
import type { WarmupStatus } from "./types";

const POLL_MS = 700;

export function useWarmupStatus(): WarmupStatus | null {
  const [status, setStatus] = useState<WarmupStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const tick = () => {
      // Partial api mocks (tests) and older clients may lack the method; a
      // synchronous throw here must degrade the same as a failed request.
      let call: Promise<import("./types").WarmupStatus>;
      try {
        call = api.getWarmupStatus();
      } catch {
        setStatus(null);
        return;
      }
      call
        .then((s) => {
          if (cancelled) return;
          // request<T> can hand back undefined or an HTML shell on a broken
          // route — guard before trusting the shape.
          if (!s || typeof s.state !== "string") {
            setStatus(null);
            return;
          }
          setStatus(s);
          if (s.state === "warming") timer = setTimeout(tick, POLL_MS);
        })
        .catch(() => {
          if (!cancelled) setStatus(null);
        });
    };
    tick();

    return () => {
      cancelled = true;
      if (timer) clearTimeout(timer);
    };
  }, []);

  return status;
}
