import React from "react";
import { HubIco } from "../primitives/HubIco";
import { CardChrome } from "./canvas/CardChrome";
import { WeatherCard }    from "./canvas/WeatherCard";
import { CalendarCard }   from "./canvas/CalendarCard";
import { MapsCard }       from "./canvas/MapsCard";
import { CryptoCard }     from "./canvas/CryptoCard";
import { SmartHomeCard }  from "./canvas/SmartHomeCard";
import { NewsCard }       from "./canvas/NewsCard";
import "./canvas-mcp.css";

// ── Card registry ──────────────────────────────────────────────

interface McpCardDef {
  app: string;
  Comp: () => React.ReactElement;
}

const MCP_CARDS: McpCardDef[] = [
  { app: "giap-weather",       Comp: WeatherCard },
  { app: "giap-calendar",      Comp: CalendarCard },
  { app: "giap-homeassistant", Comp: SmartHomeCard },
  { app: "giap-finance",       Comp: CryptoCard },
  { app: "giap-maps",          Comp: MapsCard },
  { app: "giap-news",          Comp: NewsCard },
];

const PLUS_PATH = "M12 5v14M5 12h14";

/**
 * Canvas Hub view — masonry board of MCP result cards.
 * Each card is wrapped in <CardChrome> which renders the source-app dot + kebab.
 * Phase 5: all card data is mock. SmartHomeCard is the only live-wired card
 * (room toggles sync to hubStore → Home tiles update instantly).
 */
export function CanvasHubView(): React.ReactElement {
  return (
    <div className="mcpc">
      {/* View header */}
      <header className="view-head">
        <div>
          <h1 className="view-title">Canvas</h1>
          <p className="view-sub">
            Live cards from your MCP apps. Goose drops results here — drag, pin or dismiss them.
          </p>
        </div>
        <button
          type="button"
          className="primary-btn"
          onClick={() => {
            /* Phase 7: open Add Card dialog */
          }}
          style={{ display: "flex", alignItems: "center", gap: 6 }}
        >
          <HubIco d={PLUS_PATH} size={15} color="#fff" sw={2.2} />
          Add card
        </button>
      </header>

      {/* Masonry board */}
      <div className="mcpc__board">
        {MCP_CARDS.map(({ app, Comp }) => (
          <CardChrome key={app} app={app}>
            <Comp />
          </CardChrome>
        ))}
      </div>
    </div>
  );
}
