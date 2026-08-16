// ────────────────────────────────────────────────────────────
// Home — what needs me, then what's on.
//
// This screen answers two questions in that order and nothing else. The
// suggestion is the only element that asks for anything; devices, weather and
// music report. The voice entry sits last because it is how you say something
// back, and the things you might say something about are above it.
//
// What used to be here and is deliberately gone: the sticky note, the to-do
// widget, the routines row, the camera strip, the room pills, the category
// dock, the ask-Goose bar and the header clock. Each was a reasonable thing to
// want; together they made a screen you had to read rather than glance at.
// Cameras, rooms and routines all still have their own destinations, reachable
// from the rail — Home stopped being a copy of them.
// ────────────────────────────────────────────────────────────

import { useEffect } from "react";
import { Mic } from "lucide-react";
import { HubIco } from "../primitives/HubIco";
import { DeviceTile } from "../primitives/DeviceTile";
import { WeatherWidget } from "../primitives/WeatherWidget";
import { NowPlaying } from "../primitives/NowPlaying";
import { resumeNowPlayingPolling } from "../state/hubDataStore";
import { Suggestion } from "../primitives/Suggestion";
import { useHomeData } from "../state/hubDataStore";
import { formatHubDate, greetingForHour, useNow } from "../state/useNow";
import { useAppState, useAppDispatch } from "../../state/AppContext";

// Icons used only in this header.
const HX = {
  chat: "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  bell: "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0",
};

/** Devices shown at a glance. More than this is a device list, not a glance. */
const GLANCE_LIMIT = 6;

interface HomeViewProps {
  go?: (route: string) => void;
}

export function HomeView({ go }: HomeViewProps) {
  const home = useHomeData();
  const now = useNow();
  const state = useAppState();
  const dispatch = useAppDispatch();

  // The second of the two ways a stopped now-playing poll comes back: the pond
  // itself just used the music service, so whatever was refusing may not be any
  // more. Lives here rather than in the widget because the reducer is pure and
  // the widget is deliberately provider-free (its own tests mount it bare).
  const musicToolCalls = state.contextCards.filter((c) =>
    /music|spotify/i.test(c.tool),
  ).length;
  useEffect(() => {
    if (musicToolCalls > 0) resumeNowPlayingPolling();
  }, [musicToolCalls]);

  const greeting = greetingForHour(now.getHours());
  const glance = home.devices.slice(0, GLANCE_LIMIT);

  return (
    <div className="home">
      <header className="home__head">
        <div>
          <h1 className="home__greet">
            {greeting}, <span>{home.user}</span>
          </h1>
          <p className="home__sub">
            {formatHubDate(now)} · {home.weather.cond}, {home.weather.temp}°
          </p>
        </div>
        <div className="home__head-actions">
          <button
            className="home__iconbtn"
            onClick={() => go?.("chat")}
            aria-label="Open chat"
          >
            <HubIco d={HX.chat} size={20} color="var(--pp)" />
          </button>
          <button
            className="home__iconbtn"
            onClick={() => go?.("notifications")}
            aria-label="Notifications"
          >
            <HubIco d={HX.bell} size={20} color="var(--pp)" />
          </button>
        </div>
      </header>

      {/* The one thing that asks. */}
      <Suggestion sessionId={state.sessionId} />

      <div className="home__grid">
        <section className="home__devices">
          <h2 className="home__label">Devices</h2>
          {glance.length > 0 ? (
            <div className="home__tiles">
              {glance.map((d) => (
                <DeviceTile key={d.id} device={d} />
              ))}
            </div>
          ) : (
            // An empty screen is an invitation: name the next move rather than
            // report that a list is empty.
            <button className="home__empty" onClick={() => go?.("rooms")}>
              Add your first device
            </button>
          )}
        </section>

        <aside className="home__aside">
          <WeatherWidget />
          <NowPlaying variant="tile" />
        </aside>
      </div>

      {/* Last, because it is how you answer everything above it. */}
      <button
        className="home__talk"
        onClick={() => dispatch({ type: "SET_MODE", payload: "voice" })}
      >
        <Mic size={16} strokeWidth={2.2} />
        Start talking
      </button>
    </div>
  );
}
