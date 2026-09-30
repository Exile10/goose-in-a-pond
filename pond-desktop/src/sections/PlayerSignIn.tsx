import { useCallback, useEffect, useState } from "react";
import { Check, ExternalLink } from "lucide-react";
import { api } from "../api/PondApiClient";
import { invoke, isDesktopShell } from "../shell";
import { playerView, type PlayerReply } from "./signInView";

/** The page is asked now and then, so a sign-in made there shows up here. */
const POLL_MS = 3_000;

/**
 * Where a music service is signed in and played: the music player page, which opens in the person's
 * own web browser because that is where the service documents its player running. This row opens
 * the page and says what the page last reported. The sign-in is a button on the page itself, since
 * the service's sign-in window may only open from a click there.
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
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setReply(await api.getPlayerState(service));
    } catch {
      // Not being able to ask reads the same as the page not being open.
      setReply(null);
    }
  }, [service]);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), POLL_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const view = playerView({ reply, label });
  const url = `${api.serverUrl()}/player.html?service=${encodeURIComponent(service)}`;

  async function open() {
    setError(null);
    try {
      // In the app, `window.open` would open an in-app window; the page belongs in the real browser.
      if (desktop) await invoke("open_external", { url });
      else window.open(url, "_blank", "noopener");
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <div className="secret-modal__oauth-block" data-testid={`player-sign-in-${service}`}>
      <p className="secret-modal__oauth-desc">
        {label} plays in the music player, a page that opens in your web browser on this computer.
        Sign in there.
      </p>

      {view.kind === "signed_in" && (
        <div className="secret-modal__fulfilled-indicator">
          <Check size={13} strokeWidth={2.5} />
          Signed in to {label}
        </div>
      )}

      <button
        type="button"
        className="secret-modal__oauth-btn"
        onClick={() => void open()}
        disabled={disabled}
      >
        <ExternalLink size={13} strokeWidth={1.8} />
        Open the music player
      </button>

      {view.kind !== "signed_in" && <p className="secret-modal__oauth-note">{view.note}</p>}
      {error && <p className="secret-modal__field-error">{error}</p>}
    </div>
  );
}
