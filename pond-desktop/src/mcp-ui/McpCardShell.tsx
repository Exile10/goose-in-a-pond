import type { ReactNode } from "react";
import { Chip } from "@heroui/react";
import { X } from "lucide-react";

interface McpCardShellProps {
  tool: string;
  label: string;
  showChrome?: boolean;
  onClose?: () => void;
  children: ReactNode;
  actions?: ReactNode;
}

export function McpCardShell({
  tool,
  label,
  showChrome = true,
  onClose,
  children,
  actions,
}: McpCardShellProps) {
  const bareTool = tool.includes("__") ? tool.split("__")[1] : tool;

  return (
    <div className="mcp-card">
      {showChrome && (
        <div className="mcp-card__chrome">
          <div className="mcp-card__chrome-left">
            <Chip size="sm" variant="soft" color="accent">
              {label || bareTool}
            </Chip>
          </div>
          {onClose && (
            <button
              className="mcp-card__close"
              onClick={onClose}
              aria-label="Close card"
            >
              <X size={14} />
            </button>
          )}
        </div>
      )}
      <div className="mcp-card__body">{children}</div>
      {actions && <div className="mcp-card__actions">{actions}</div>}
    </div>
  );
}
