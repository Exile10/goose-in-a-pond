/**
 * Structured logging to stderr, as NDJSON.
 *
 * The same shape the Matter controller uses (`matter-server/src/log.ts`), and for
 * the same reason: a line without a level cannot be told from any other, so
 * whatever reads this has to flatten everything to one, and an extension whose
 * failures arrive at the same level as its chatter is most of the way back to
 * being silent.
 *
 * stdout is the MCP JSON-RPC channel and MUST stay clean — a stray line there is
 * not noise, it is a protocol violation that breaks the extension. Everything
 * here goes to stderr.
 */

export type Level = "error" | "warn" | "info" | "debug";

export interface LogRecord {
  level: Level;
  /** Discriminator, matching the `kind` convention on GIAP's `giap::trace` events. */
  kind: string;
  message: string;
  fields?: Record<string, unknown>;
}

/**
 * Strip OAuth credentials out of anything on its way to a log line.
 *
 * A Spotify access token is a bearer credential for the user's account, and the
 * refresh path carries GIAP's own internal token as well. Both would otherwise
 * travel in exactly the places worth logging: request headers, error bodies, and
 * the refresh response. Deliberately over-eager — a vaguer log line costs
 * nothing next to a working credential on disk.
 */
export function redactSecrets(text: string): string {
  return (
    text
      // `Bearer <token>` in a header or an echoed request.
      .replace(/Bearer\s+[A-Za-z0-9._~+/=-]{8,}/gi, "Bearer [redacted]")
      // `"access_token": "..."` and its siblings in a JSON body.
      .replace(
        /("(?:access|refresh|id)_token"\s*:\s*")[^"]+(")/gi,
        (_m, open: string, close: string) => `${open}[redacted]${close}`,
      )
      .replace(/("client_secret"\s*:\s*")[^"]+(")/gi,
        (_m, open: string, close: string) => `${open}[redacted]${close}`)
      // Spotify access tokens are long opaque strings starting `BQ`; they show
      // up bare in error text often enough to be worth their own rule.
      .replace(/\bBQ[A-Za-z0-9._-]{20,}/g, "[redacted:token]")
  );
}

function emit(level: Level, kind: string, message: string, fields?: Record<string, unknown>): void {
  const record: LogRecord = { level, kind, message: redactSecrets(message) };
  if (fields && Object.keys(fields).length > 0) {
    record.fields = JSON.parse(redactSecrets(JSON.stringify(fields))) as Record<string, unknown>;
  }
  // One line, whatever happens: a record that cannot serialise must not take the
  // process down, and must not emit a half-line that corrupts the reader.
  let line: string;
  try {
    line = JSON.stringify(record);
  } catch {
    line = JSON.stringify({ level, kind, message: "log record was not serialisable" });
  }
  process.stderr.write(`${line}\n`);
}

export const log = {
  error: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("error", kind, message, fields),
  warn: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("warn", kind, message, fields),
  info: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("info", kind, message, fields),
  debug: (kind: string, message: string, fields?: Record<string, unknown>) =>
    emit("debug", kind, message, fields),
};

/**
 * An error's whole cause chain, redacted, without the stack.
 *
 * The chain and not just `.message`: `fetch` and the JSON layer under it wrap
 * errors as they rethrow, so the outermost message is the layer that gave up
 * rather than the reason.
 */
export function describeError(error: unknown): string {
  const parts: string[] = [];
  const seen = new Set<unknown>();
  let current: unknown = error;

  while (current !== undefined && current !== null && parts.length < 8) {
    if (seen.has(current)) break;
    seen.add(current);
    const message = current instanceof Error ? current.message : String(current);
    if (message.length > 0 && !parts.includes(message)) parts.push(message);
    current = current instanceof Error ? (current as { cause?: unknown }).cause : undefined;
  }

  return redactSecrets(parts.join(": ")) || "an error with no message";
}
