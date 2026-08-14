/**
 * McpAppHost — Renders an MCP App in a sandboxed iframe.
 *
 * Implements the host side of the MCP Apps protocol (io.modelcontextprotocol/ui).
 * Communication with the app uses JSON-RPC 2.0 over postMessage.
 *
 * Protocol spec: https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx
 */

import { useEffect, useRef, useCallback } from "react";
import { X } from "lucide-react";

// ── Types ──────────────────────────────────────────────────────

export interface McpAppHostProps {
  /** Self-contained HTML content of the MCP App */
  html: string;
  /** Tool result to push on mount (from the tool_call that triggered this app) */
  toolResult?: McpToolResult;
  /** Tool input arguments (pushed via ui/notifications/tool-input) */
  toolInput?: Record<string, unknown>;
  /** Tool name that triggered this app */
  toolName?: string;
  /** Light or dark theme */
  theme?: "light" | "dark";
  /** Callback when the app calls a server tool */
  onToolCall?: (name: string, args: Record<string, unknown>) => Promise<McpToolResult>;
  /** Callback when the app updates the model context */
  onUpdateContext?: (content: unknown) => void;
  /** Callback when the app wants to open a URL */
  onOpenUrl?: (url: string) => void;
  /** Callback when the app sends a message to the chat */
  onMessage?: (text: string) => void;
  /** Close/dismiss the app card */
  onClose?: () => void;
  /** Container width */
  width?: number;
  /** Container height (auto if not set) */
  height?: number;
}

export interface McpToolResult {
  content: Array<{ type: string; text?: string; blob?: string }>;
}

interface JsonRpcRequest {
  jsonrpc: "2.0";
  id?: number | string;
  method: string;
  params?: Record<string, unknown>;
}

// ── Constants ──────────────────────────────────────────────────

const PROTOCOL_VERSION = "2026-01-26";
const EXTENSION_ID = "io.modelcontextprotocol/ui";

/**
 * targetOrigin for every message sent INTO the app frame.
 *
 * The frame is sandboxed without `allow-same-origin`, so its origin is opaque
 * and serialises to the literal string `"null"` — that is what a targetOrigin
 * must match. `"*"` would work too, and is what this used to be, but it also
 * means "deliver to whatever origin this frame has now", which is exactly the
 * check worth keeping if the frame ever navigates or the sandbox is loosened.
 *
 * The inbound direction cannot use origin at all: `event.origin` is also
 * `"null"` for an opaque-origin frame, and every such frame reports the same
 * thing, so it distinguishes nothing. `handleMessage` therefore checks
 * `event.source === iframeRef.current.contentWindow` — window identity, which
 * is unforgeable from inside the frame.
 */
const APP_FRAME_ORIGIN = "null";

// ── Component ──────────────────────────────────────────────────

export function McpAppHost({
  html,
  toolResult,
  toolInput,
  toolName,
  theme = "light",
  onToolCall,
  onUpdateContext,
  onOpenUrl,
  onMessage,
  onClose,
  width,
  height,
}: McpAppHostProps) {
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const initializedRef = useRef(false);

  // Send a JSON-RPC response to the iframe
  const sendResponse = useCallback((id: number | string, result: unknown) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", id, result },
      APP_FRAME_ORIGIN,
    );
  }, []);

  // Send a JSON-RPC error to the iframe
  const sendError = useCallback((id: number | string, code: number, message: string) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", id, error: { code, message } },
      APP_FRAME_ORIGIN,
    );
  }, []);

  // Send a JSON-RPC notification (no id) to the iframe
  const sendNotification = useCallback((method: string, params?: unknown) => {
    iframeRef.current?.contentWindow?.postMessage(
      { jsonrpc: "2.0", method, params },
      APP_FRAME_ORIGIN,
    );
  }, []);

  // Handle incoming JSON-RPC messages from the iframe
  const handleMessage = useCallback(
    async (event: MessageEvent) => {
      // Only accept messages from our iframe
      if (!iframeRef.current || event.source !== iframeRef.current.contentWindow) return;

      const msg = event.data as JsonRpcRequest;
      if (!msg || msg.jsonrpc !== "2.0" || !msg.method) return;

      switch (msg.method) {
        // ── Initialization handshake ──
        case "ui/initialize": {
          const hostContext = {
            theme,
            locale: navigator.language,
            timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone,
            platform: "desktop" as const,
            displayMode: "inline" as const,
            availableDisplayModes: ["inline", "fullscreen"],
            ...(toolName ? { toolInfo: { tool: { name: toolName } } } : {}),
            styles: {
              variables: {
                "--primary": "#8C4BFF",
                "--bg": theme === "dark" ? "#1C1C1C" : "#FFFFFF",
                "--fg": theme === "dark" ? "#FFFFFF" : "#1C1C1C",
                "--border": theme === "dark" ? "#3D3D3D" : "#EDEDED",
                "--radius": "10px",
              },
            },
            ...(width || height ? {
              containerDimensions: { width, height },
            } : {}),
          };

          sendResponse(msg.id!, {
            protocolVersion: PROTOCOL_VERSION,
            capabilities: {
              extensions: {
                [EXTENSION_ID]: {
                  mimeTypes: ["text/html;profile=mcp-app"],
                },
              },
            },
            hostContext,
          });

          initializedRef.current = true;

          // Push tool input and result after initialization
          if (toolInput) {
            sendNotification("ui/notifications/tool-input", { arguments: toolInput });
          }
          if (toolResult) {
            sendNotification("ui/notifications/tool-result", toolResult);
          }
          break;
        }

        // ── Tool call from app ──
        case "tools/call": {
          if (!onToolCall || !msg.params) {
            sendError(msg.id!, -32601, "Tool calls not supported");
            break;
          }
          try {
            const result = await onToolCall(
              msg.params.name as string,
              (msg.params.arguments as Record<string, unknown>) ?? {},
            );
            sendResponse(msg.id!, result);
          } catch (err) {
            sendError(msg.id!, -32000, String(err));
          }
          break;
        }

        // ── Open external URL ──
        case "ui/open-link": {
          const url = msg.params?.url as string;
          if (url && onOpenUrl) {
            onOpenUrl(url);
          } else if (url) {
            window.open(url, "_blank", "noopener,noreferrer");
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Update model context ──
        case "ui/update-model-context": {
          if (onUpdateContext && msg.params) {
            onUpdateContext(msg.params);
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Send message to chat ──
        case "ui/message": {
          const text = (msg.params?.content as { text?: string })?.text;
          if (text && onMessage) {
            onMessage(text);
          }
          if (msg.id) sendResponse(msg.id, {});
          break;
        }

        // ── Display mode request ──
        case "ui/request-display-mode": {
          // For now, acknowledge but stay inline
          if (msg.id) sendResponse(msg.id, { mode: "inline" });
          break;
        }

        // ── Logging from app ──
        case "ui/log": {
          const level = msg.params?.level ?? "info";
          const message = msg.params?.message ?? "";
          if (level === "error") console.error(`[MCP App] ${message}`);
          else console.log(`[MCP App] ${message}`);
          break;
        }

        default: {
          if (msg.id) {
            sendError(msg.id, -32601, `Method not found: ${msg.method}`);
          }
          break;
        }
      }
    },
    [theme, toolName, toolInput, toolResult, width, height, onToolCall, onUpdateContext, onOpenUrl, onMessage, sendResponse, sendError, sendNotification],
  );

  // Listen for postMessage events
  useEffect(() => {
    window.addEventListener("message", handleMessage);
    return () => window.removeEventListener("message", handleMessage);
  }, [handleMessage]);

  // Send teardown on unmount
  useEffect(() => {
    return () => {
      if (initializedRef.current) {
        iframeRef.current?.contentWindow?.postMessage(
          { jsonrpc: "2.0", method: "ui/resource-teardown", params: { reason: "unmount" } },
          APP_FRAME_ORIGIN,
        );
      }
    };
  }, []);

  // Push updated tool results when they change after initialization
  useEffect(() => {
    if (initializedRef.current && toolResult) {
      sendNotification("ui/notifications/tool-result", toolResult);
    }
  }, [toolResult, sendNotification]);

  return (
    <div className="mcp-app-host">
      <div className="mcp-app-host__chrome">
        <span className="mcp-app-host__label">
          {toolName ?? "MCP App"}
        </span>
        {onClose && (
          <button className="mcp-app-host__close" onClick={onClose} aria-label="Close app">
            <X size={14} />
          </button>
        )}
      </div>
      <iframe
        ref={iframeRef}
        srcDoc={html}
        /*
         * `allow-scripts` ONLY. Never `allow-same-origin` here.
         *
         * A `srcdoc` iframe inherits the embedder's origin, and
         * `allow-same-origin` hands that origin back to the guest — which means
         * `parent.document`, `parent.localStorage` (where `PondApiClient` keeps
         * `giap-session-token` and the refresh token), and the ability to strip
         * this very `sandbox` attribute off the parent DOM. `html` comes from
         * `api.getMcpResource(...)`, so it is whatever an MCP server chose to
         * serve: attacker-controlled the moment any third-party server ships a
         * `ui://` resource.
         *
         * Omitting it gives the frame an OPAQUE origin, which also satisfies
         * MCP Apps (SEP-1865) "the Host and the Sandbox MUST have different
         * origins" — the spec's two-layer form puts the guest on its own real
         * origin and only then re-grants `allow-same-origin`. Until GIAP serves
         * guest documents from a separate origin, opaque is the different
         * origin, and the postMessage bridge below is the only channel in or
         * out. That is the intended shape anyway.
         */
        sandbox="allow-scripts"
        className="mcp-app-host__frame"
        style={{
          width: width ?? "100%",
          height: height ?? 400,
          border: "none",
        }}
        title={toolName ?? "MCP App"}
      />
    </div>
  );
}
