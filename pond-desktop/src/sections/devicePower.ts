/** On/off from `get_device_state`'s text; `undefined`, not `false`, when it did not say.
 *  Format: `state_line` in `crates/pond-mcp-server/src/device_control.rs` (a test pins it). */
export function powerStateOf(content: string): boolean | undefined {
  const line = content.split("\n").find((l) => /^\s*power:/.test(l));
  if (line === undefined) return undefined;
  const value = line.slice(line.indexOf(":") + 1).trim().toLowerCase();
  if (value === "on") return true;
  if (value === "off") return false;
  return undefined;
}
