/**
 * Structured logging to stderr, as NDJSON.
 *
 * The Rust side pipes this process's stderr and relays each line into `tracing`. Lines
 * that parse as one of these records are re-emitted at the level they name; anything
 * else (a Node stack trace, matter.js's own output) is relayed at debug. That is why
 * this is NDJSON and not prose: without a level on the wire the relay has to flatten
 * every line to one level, and a controller that cannot say "this was an error" is
 * most of the way back to being silent.
 *
 * stdout is left alone. Nothing writes to it, so a future NDJSON channel on stdout
 * stays available.
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
 * Setup codes are secrets: a Matter pairing code grants fabric access, and a leaked
 * one in a log file is a working credential for anyone who reads it.
 *
 * Applied to every string that could have come near a code — op parameters, and any
 * error message raised while commissioning, because matter.js and the underlying CHIP
 * errors echo what they were given. Deliberately over-eager: redacting a digit run
 * that happened not to be a code costs a slightly vaguer log line, while missing one
 * writes a credential to disk. GIAP's Rust side applies the same rule on its side of
 * the socket, so neither implementation depends on the other having done it.
 */
export function redactSetupCode(text: string): string {
  return (
    text
      // QR payloads: `MT:` followed by base-38. Longest form first, so the digit
      // rules below cannot nibble a piece out of one and leave the rest readable.
      .replace(/MT:[A-Z0-9.$%*+\-./:]+/gi, "[redacted:setup-code]")
      // Manual pairing codes are 11 or 21 digits; passcodes are 8. Bounded on both
      // sides so an ordinary identifier of a different length is left alone.
      .replace(/(?<!\d)(\d{21}|\d{11}|\d{8})(?!\d)/g, "[redacted:setup-code]")
  );
}

/** Which kind of code this is, for logging in place of the value itself. */
export function setupCodeKind(code: string): "pairing_code" | "passcode" | "unknown" {
  const trimmed = code.trim();
  if (/^MT:/i.test(trimmed)) return "pairing_code";
  const digits = trimmed.replace(/[\s-]/g, "");
  if (/^\d{11}$/.test(digits) || /^\d{21}$/.test(digits)) return "pairing_code";
  if (/^\d{8}$/.test(digits)) return "passcode";
  return "unknown";
}

/** Hook used by the server to also fan log records out to connected clients. */
type Sink = (record: LogRecord) => void;

let extraSink: Sink | undefined;

export function onLog(sink: Sink | undefined): void {
  extraSink = sink;
}

function emit(level: Level, kind: string, message: string, fields?: Record<string, unknown>): void {
  const record: LogRecord = { level, kind, message: redactSetupCode(message) };
  if (fields && Object.keys(fields).length > 0) {
    record.fields = fields;
  }
  // One line, whatever happens: a record that cannot serialise must not take the
  // process down, and must not emit a half-line that corrupts the reader's framing.
  let line: string;
  try {
    line = JSON.stringify(record);
  } catch {
    line = JSON.stringify({ level, kind, message: "log record was not serialisable" });
  }
  process.stderr.write(`${line}\n`);
  extraSink?.(record);
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

/** An error's message, redacted, without the stack — stacks belong in `fields`. */
export function describeError(error: unknown): string {
  if (error instanceof Error) return redactSetupCode(error.message);
  return redactSetupCode(String(error));
}
