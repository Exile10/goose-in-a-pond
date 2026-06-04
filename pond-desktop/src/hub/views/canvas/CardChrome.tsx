import React from "react";
import { HubIco } from "../../primitives/HubIco";
import { dotsEl } from "../../primitives/HubIco";

interface CardChromeProps {
  /** MCP server app name, e.g. "giap-weather" */
  app: string;
  children: React.ReactNode;
}

/**
 * Shared card wrapper that renders the source-app dot + app name + kebab menu.
 * Ports `.mcp-item__chrome` from goose-hub-mcp.jsx/css.
 */
export function CardChrome({ app, children }: CardChromeProps): React.ReactElement {
  return (
    <div className="mcp-item">
      <div className="mcp-item__chrome">
        <span className="mcp-item__src">
          <span className="mcp-item__dot" aria-hidden="true" />
          {app}
        </span>
        <span
          className="mcp-item__kebab"
          role="button"
          aria-label={`${app} card options`}
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") e.preventDefault();
          }}
        >
          <HubIco d={dotsEl} size={15} color="#C4C4CC" sw={0} fill="currentColor" />
        </span>
      </div>
      {children}
    </div>
  );
}
