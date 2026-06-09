import { useState } from "react";
import { HubIco } from "./HubIco";
import { HP_PATHS } from "./icons";
import { pauseEl } from "./HubIco";
import { useHomeData } from "../state/hubDataStore";

type NowPlayingVariant = "bar" | "tile";

interface NowPlayingProps {
  variant?: NowPlayingVariant;
}

export function NowPlaying({ variant = "bar" }: NowPlayingProps) {
  const np = useHomeData().nowPlaying;
  const [playing, setPlaying] = useState(true);

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
        <button aria-label="Previous">
          <HubIco d={HP_PATHS.skipB} size={16} color="#64748B" />
        </button>
        <button
          className="np__play"
          onClick={() => setPlaying((p) => !p)}
          aria-label={playing ? "Pause" : "Play"}
        >
          <HubIco d={playing ? pauseEl : HP_PATHS.play} size={16} color="#fff" />
        </button>
        <button aria-label="Next">
          <HubIco d={HP_PATHS.skipF} size={16} color="#64748B" />
        </button>
      </div>
    </div>
  );
}
