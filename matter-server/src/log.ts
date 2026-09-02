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

/**
 * Which of the three forms a setup code is in.
 *
 * A QR payload is its own kind rather than a species of pairing code, because the
 * three take three different routes into matter.js -- see `commissioningOptions` in
 * `controller.ts`. Collapsing the first two is what made the QR path fail: a payload
 * classified as a pairing code was handed to a decoder that only reads the manual
 * form, and nothing in either implementation noticed.
 *
 * Also used for logging in place of the value itself, which is why the kind names are
 * safe to print and the code is not.
 */
export type SetupCodeKind = "qr_payload" | "pairing_code" | "passcode" | "unknown";

export function setupCodeKind(code: string): SetupCodeKind {
  const trimmed = code.trim();
  if (/^MT:/i.test(trimmed)) return "qr_payload";
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

/**
 * An error's whole cause chain, redacted, without the stack — stacks belong in
 * `fields`.
 *
 * The chain and not just `.message`, for the same reason the Rust side renders
 * `{:#}` rather than `Display`: matter.js wraps errors as it rethrows them, so
 * the outermost message is the layer that gave up rather than the reason. A
 * failed discovery reported itself as "discovery of node discovery failed" and
 * dropped the cause, which is the only part that tells you what to do.
 */
export function describeError(error: unknown): string {
  const parts: string[] = [];
  const seen = new Set<unknown>();
  let current: unknown = error;

  // Bounded as well as cycle-guarded: a chain long enough to matter is a bug of
  // its own, and a log line the width of a screen helps nobody.
  while (current !== undefined && current !== null && parts.length < 8) {
    if (seen.has(current)) break;
    seen.add(current);

    const message = current instanceof Error ? current.message : String(current);
    // matter.js sometimes rethrows with the cause's text already embedded;
    // repeating it would pad the line without adding anything.
    if (message.length > 0 && !parts.includes(message)) parts.push(message);

    current = current instanceof Error ? (current as { cause?: unknown }).cause : undefined;
  }

  return redactSetupCode(parts.join(": ")) || "an error with no message";
}
