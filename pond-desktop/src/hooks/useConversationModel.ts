// Whether the pond has a conversation model chosen. With none it refuses a turn (409 `no_model`)
// and picks nothing in its place, so the screens offer the suggestions instead.

import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api/PondApiClient";
import { onUsed } from "../state/downloadAndUse";

export type ConversationModelState = "unknown" | "chosen" | "none";

/** The pond's own rule: another pond (`mesh`) and the offline echo (`mock`) need no model name;
 *  every other provider does, and an empty one is no choice at all. */
export function conversationModelChosen(provider: string, model: string): boolean {
  switch (provider.trim()) {
    case "":
      return false;
    case "mesh":
    case "mock":
      return true;
    default:
      return model.trim() !== "";
  }
}

export interface UseConversationModel {
  /** `unknown` until the settings are read, and when they cannot be: never a reason to block. */
  state: ConversationModelState;
  refresh: () => void;
  /** The pond refused a turn for want of a model, whatever the last read said. */
  markNone: () => void;
}

export function useConversationModel(): UseConversationModel {
  const [state, setState] = useState<ConversationModelState>("unknown");

  const refresh = useCallback(() => {
    let read: Promise<{ chat_provider?: string; chat_model?: string }>;
    try {
      read = api.getSettings();
    } catch {
      return;
    }
    read
      .then((s) => {
        // A reply that names neither field is no evidence either way.
        if (typeof s?.chat_provider !== "string" || typeof s?.chat_model !== "string") return;
        setState(conversationModelChosen(s.chat_provider, s.chat_model) ? "chosen" : "none");
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    refresh();
    return onUsed(() => setState("chosen"));
  }, [refresh]);

  const markNone = useCallback(() => setState("none"), []);

  return useMemo(() => ({ state, refresh, markNone }), [state, refresh, markNone]);
}
