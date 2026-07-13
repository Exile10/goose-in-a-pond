// ────────────────────────────────────────────────────────────
// useMemoryStatus — fetches the device LLM memory budget once.
//
// Wraps GET /api/v1/models/memory-status. Degrades gracefully: on any error
// (endpoint absent, server offline, Mac/dev) it returns null and never throws,
// so the memory-fit guard simply renders no verdict rather than crashing.
// ────────────────────────────────────────────────────────────

import { useState, useEffect } from "react";
import { api } from "./PondApiClient";
import type { ModelMemoryStatus } from "./types";

export function useMemoryStatus(): ModelMemoryStatus | null {
  const [status, setStatus] = useState<ModelMemoryStatus | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getMemoryStatus()
      .then((s) => {
        if (!cancelled) setStatus(s);
      })
      .catch(() => {
        /* non-fatal — no budget means no verdict (graceful degradation) */
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return status;
}
