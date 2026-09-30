import { LogIn, LogOut, Music, Pause, Play, SkipBack, SkipForward } from "lucide-react";
import { usePlayerState } from "./usePlayerState";
import type { PlayerAdapter } from "./types";
import "./player.css";

function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

/**
 * One service on the player page: what is playing, with its artwork, and the standard controls.
 * The controls are always there once the service can play, because the services require them (and
 * Play is also the click a browser wants before it lets a page make sound). Signing in is a button
 * here, since the sign-in window may only open from a click on this page.
 */
export function PlayerApp({ adapter }: { adapter: PlayerAdapter }) {
  const state = usePlayerState(adapter);
  const playing = state.status === "playing";

  async function run(action: () => Promise<void>) {
    try {
      await action();
    } catch {
      // The adapter already put the reason in its state.message, which is shown below.
    }
  }

  return (
    <main className="player" aria-label={`${adapter.label} player`}>
      <header className="player__head">
        <Music size={18} strokeWidth={1.8} aria-hidden="true" />
        <h1 className="player__title">{adapter.label}</h1>
      </header>

      {state.need === "authorization" && (
        <section className="player__card">
          <p className="player__text">
            Sign in to {adapter.label} with the account that has your subscription.
          </p>
          <button
            type="button"
            className="player__primary"
            onClick={() => void run(() => adapter.authorize())}
          >
            <LogIn size={16} strokeWidth={1.8} aria-hidden="true" />
            Sign in to {adapter.label}
          </button>
        </section>
      )}

      {state.need === "setup" && (
        <section className="player__card">
          <p className="player__text">
            {state.message ??
              `${adapter.label} is not set up yet. Add it in the Music extension's settings.`}
          </p>
        </section>
      )}

      {state.need === "none" && (
        <section className="player__card">
          {state.track ? (
            <div className="player__now">
              {state.track.artwork_url && (
                <img
                  className="player__art"
                  src={state.track.artwork_url}
                  alt={`Artwork for ${state.track.album || state.track.title}`}
                  width={150}
                  height={150}
                />
              )}
              <div className="player__about">
                <p className="player__track">{state.track.title}</p>
                <p className="player__meta">{state.track.artist}</p>
                {state.track.album && <p className="player__meta">{state.track.album}</p>}
                <p className="player__time">
                  {clock(state.position_ms)} / {clock(state.track.duration_ms)}
                </p>
              </div>
            </div>
          ) : (
            <p className="player__text">Nothing is playing. Ask the assistant for a song.</p>
          )}
          <div className="player__controls">
            <button
              type="button"
              className="player__key"
              aria-label="Previous"
              onClick={() => void run(() => adapter.previous())}
            >
              <SkipBack size={18} strokeWidth={1.8} aria-hidden="true" />
            </button>
            <button
              type="button"
              className="player__key"
              aria-label={playing ? "Pause" : "Play"}
              onClick={() => void run(() => (playing ? adapter.pause() : adapter.resume()))}
            >
              {playing ? (
                <Pause size={18} strokeWidth={1.8} aria-hidden="true" />
              ) : (
                <Play size={18} strokeWidth={1.8} aria-hidden="true" />
              )}
            </button>
            <button
              type="button"
              className="player__key"
              aria-label="Next"
              onClick={() => void run(() => adapter.next())}
            >
              <SkipForward size={18} strokeWidth={1.8} aria-hidden="true" />
            </button>
          </div>
          {adapter.signOut && (
            <button
              type="button"
              className="player__quiet"
              onClick={() => void run(() => adapter.signOut!())}
            >
              <LogOut size={14} strokeWidth={1.8} aria-hidden="true" />
              Sign out of {adapter.label}
            </button>
          )}
        </section>
      )}

      {state.message && state.need !== "setup" && (
        <p
          className={`player__note${state.status === "error" ? " player__note--error" : ""}`}
          role="status"
        >
          {state.message}
        </p>
      )}
    </main>
  );
}
