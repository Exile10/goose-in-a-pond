import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Music } from "lucide-react";
import { api } from "../api/PondApiClient";
import { invoke, isDesktopShell } from "../shell";
import { signInView, type PlayerReply } from "./signInView";

/** While idle the player is asked now and then, so a sign-in made elsewhere shows up. */
const IDLE_POLL_MS = 3_000;
/** While a sign-in is under way it is watched closely, since the person is waiting on it. */
const PENDING_POLL_MS = 1_500;
/** Generous: they may be typing a password and a code. */
const PENDING_TIMEOUT_MS = 3 * 60 * 1000;

/**
 * One button that signs in to a music service through the player window. The person clicks here,
 * Apple's own sign-in opens, and this row turns to "Signed in" when the player reports it; there is
 * no second click in another window and no key to paste. Whether the button can work at all is
 * decided by `signInView`, so it is only offered when it can.
 */
export function PlayerSignIn({
  service,
  label,
  disabled = false,
}: {
  service: string;
  label: string;
  disabled?: boolean;
}) {
  const desktop = isDesktopShell();
  const [reply, setReply] = useState<PlayerReply | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const startedAt = useRef(0);

  const refresh = useCallback(async () => {
    if (!desktop) return;
    try {
      setReply(await api.getPlayerState(service));
    } catch {
      // Not being able to ask reads the same as the player not being there yet.
      setReply(null);
    }
  }, [desktop, service]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (pending && Date.now() - startedAt.current > PENDING_TIMEOUT_MS) {
        setPending(false);
        setError(`${label} did not finish signing in. Please try again.`);
      }
      void refresh();
    }, pending ? PENDING_POLL_MS : IDLE_POLL_MS);
    return () => clearInterval(timer);
  }, [refresh, pending, label]);

  const view = signInView({ desktop, reply, pending, label });

  // The wait is over as soon as the player says so, not when the timer would have run out.
  useEffect(() => {
    if (view.kind === "signed_in" && pending) setPending(false);
  }, [view.kind, pending]);

  async function signIn() {
    setError(null);
    try {
      const result = await invoke("player_authorize", { service });
      if (!result.started) {
        setError(result.message ?? `${label} could not start signing in.`);
        return;
      }
      startedAt.current = Date.now();
      setPending(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <div className="secret-modal__oauth-block" data-testid={`player-sign-in-${service}`}>
      <p className="secret-modal__oauth-desc">
        Sign in with the Apple ID that has your {label} subscription.
      </p>

      {view.kind === "ready" && (
        <button
          type="button"
          className="secret-modal__oauth-btn"
          onClick={() => void signIn()}
          disabled={disabled}
        >
          <Music size={13} strokeWidth={1.8} />
          Sign in to {label}
        </button>
      )}

      {view.kind === "signing_in" && (
        <div className="secret-modal__oauth-polling">
          <div className="secret-modal__oauth-spinner" />
          <span>Waiting for {label}'s sign-in window…</span>
        </div>
      )}

      {view.kind === "signed_in" && (
        <div className="secret-modal__fulfilled-indicator">
          <Check size={13} strokeWidth={2.5} />
          Signed in to {label}
        </div>
      )}

      {(view.kind === "starting" || view.kind === "unavailable" || view.kind === "not_in_shell") && (
        <p className="secret-modal__oauth-note">{view.note}</p>
      )}
      {(view.kind === "ready" || view.kind === "signing_in") && view.note && (
        <p className="secret-modal__oauth-note">{view.note}</p>
      )}

      {error && <p className="secret-modal__field-error">{error}</p>}
    </div>
  );
}
