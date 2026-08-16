// ────────────────────────────────────────────────────────────
// Home — what needs me, then what's on.
//
// The sections-shell half of the Home redesign; `hub/views/Home.tsx` is the
// hub half and they are deliberately the same screen. Both surfaces are kept
// (they are blended on purpose), so a design that landed on only one of them
// would leave the household looking at two different Homes depending on how
// they got there.
//
// The component is still exported as `Dashboard` and still lives on the
// `dashboard` section id: that id is persisted in localStorage as
// `giap-section`, so renaming it would strand anyone whose app reopens on the
// screen they left. What people see is "Home"; what the router remembers is
// unchanged.
//
// Deliberately gone: the ask-Goose bar, room pills, camera strip, routines
// row, sticky note, to-do widget, category dock, header clock, and the model
// roles panel. Each had a reason to exist; together they made a screen you
// read rather than glanced at. Every one of them still has its own destination
// in the sidebar — Home stopped being a copy of all of them.
// ────────────────────────────────────────────────────────────

import { useEffect } from "react";
import { Mic } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import { useHomeData } from "../hub/state/hubDataStore";
import { formatHubDate, greetingForHour, useNow } from "../hub/state/useNow";
import { DeviceTile } from "../hub/primitives/DeviceTile";
import { WeatherWidget } from "../hub/primitives/WeatherWidget";
import { NowPlaying } from "../hub/primitives/NowPlaying";
import { resumeNowPlayingPolling } from "../hub/state/hubDataStore";
import { Suggestion } from "../hub/primitives/Suggestion";
import { HubIco } from "../hub/primitives/HubIco";

const HX = {
  chat: "M21 12a8 8 0 0 1-11.5 7.2L4 21l1.8-5.4A8 8 0 1 1 21 12z",
  bell: "M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9M10.3 21a1.94 1.94 0 0 0 3.4 0",
};

/** Devices shown at a glance. More than this is a device list, not a glance. */
const GLANCE_LIMIT = 6;

export function Dashboard() {
  const state = useAppState();
  const dispatch = useAppDispatch();
  const home = useHomeData();
  const now = useNow();

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
            onClick={() => dispatch({ type: "SET_SECTION", payload: "chat" })}
            aria-label="Open chat"
          >
            <HubIco d={HX.chat} size={20} color="var(--pp)" />
          </button>
          <button
            className="home__iconbtn"
            onClick={() => dispatch({ type: "SET_SECTION", payload: "notifications" })}
            aria-label="Notifications"
          >
            <HubIco d={HX.bell} size={20} color="var(--pp)" />
          </button>
        </div>
      </header>

      {/* The one thing on this screen that asks. */}
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
            <button
              className="home__empty"
              onClick={() => dispatch({ type: "SET_SECTION", payload: "devices" })}
            >
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
