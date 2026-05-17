import type { ComponentType } from "react";

/** Standard props every MCP-UI card renderer receives */
export interface McpCardProps {
  data: Record<string, unknown>;
  toolName: string;
  onClose?: () => void;
  variant?: "compact" | "normal" | "large";
}

/** Registration entry for an MCP-UI card renderer */
export interface McpCardRegistration {
  /** Unique key for this card type */
  key: string;
  /** Display label */
  label: string;
  /** Lucide icon name for chrome bar */
  icon: string;
  /** Pattern to match tool names (string for includes-match, RegExp for regex) */
  toolPattern: string | RegExp;
  /** The React component */
  component: ComponentType<McpCardProps>;
  /** Extract structured data from raw tool result string */
  parseResult?: (raw: string) => Record<string, unknown>;
  /** Mock data for demo/suggest mode */
  mockData?: Record<string, unknown>;
  /** Mock tool name for demo triggers */
  mockTool?: string;
}

const _registry: McpCardRegistration[] = [];

export function registerMcpCard(entry: McpCardRegistration): void {
  // Avoid duplicates
  const idx = _registry.findIndex((r) => r.key === entry.key);
  if (idx >= 0) _registry[idx] = entry;
  else _registry.push(entry);
}

export function findCardRenderer(toolName: string): McpCardRegistration | null {
  const bare = toolName.includes("__") ? toolName.split("__")[1]! : toolName;
  for (const reg of _registry) {
    if (typeof reg.toolPattern === "string") {
      if (bare.includes(reg.toolPattern) || toolName.includes(reg.toolPattern)) return reg;
    } else {
      if (reg.toolPattern.test(bare) || reg.toolPattern.test(toolName)) return reg;
    }
  }
  return null;
}

/** Find a card renderer by explicit hint key (exact match on registration key). */
export function findCardByHint(hint: string): McpCardRegistration | null {
  return _registry.find((r) => r.key === hint) ?? null;
}

export function getAllRegistrations(): readonly McpCardRegistration[] {
  return _registry;
}
