import { useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { pauseEl } from "./HubIco";
import { useHomeData, controlNowPlaying } from "../state/hubDataStore";

type NowPlayingVariant = "bar" | "tile";

interface NowPlayingProps {
  variant?: NowPlayingVariant;
}

export function NowPlaying({ variant = "bar" }: NowPlayingProps) {
  const np = useHomeData().nowPlaying;
  // Cosmetic-only toggle used while Spotify isn't connected, so the demo
  // widget still feels interactive rather than dead.
  const [demoPlaying, setDemoPlaying] = useState(true);
  const playing = np.connected ? np.playing : demoPlaying;

  function handlePlayPause() {
    if (np.connected) void controlNowPlaying(playing ? "pause" : "play");
    else setDemoPlaying((p) => !p);
  }

  function handleSkip(action: "next" | "previous") {
    if (np.connected) void controlNowPlaying(action);
  }

  return (
    <div className={`np np--${variant}`}>
      <div
        className="np__art"
        style={{
          background: `linear-gradient(135deg,hsl(${np.hue},60%,58%),hsl(${np.hue + 40},55%,42%))`,
        }}
      >
        <HubIco
          d={HP_PATHS.music}
          size={variant === "tile" ? 26 : 18}
          color="rgba(255,255,255,.9)"
        />
      </div>
      <div className="np__info">
        <span className="np__track">{np.track}</span>
        <span className="np__artist">{np.artist}</span>
        {variant === "tile" && (
          <div className="np__bar">
            <span style={{ width: `${np.elapsed * 100}%` }} />
          </div>
        )}
      </div>
      <div className="np__ctrls">
        <button aria-label="Previous" onClick={() => handleSkip("previous")}>
          <HubIco d={HP_PATHS.skipB} size={16} color="#64748B" />
        </button>
        <button
          className="np__play"
          onClick={handlePlayPause}
          aria-label={playing ? "Pause" : "Play"}
        >
          <HubIco d={playing ? pauseEl : HP_PATHS.play} size={16} color="#fff" />
        </button>
        <button aria-label="Next" onClick={() => handleSkip("next")}>
          <HubIco d={HP_PATHS.skipF} size={16} color="#64748B" />
        </button>
      </div>
    </div>
  );
}
