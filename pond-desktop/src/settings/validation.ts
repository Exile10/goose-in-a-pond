// ─── Settings validation ───────────────────────────────────────────────────
//
// Client-side checks that run before `PUT /api/v1/settings`.
//
// WHY THIS EXISTS, given the server validates too: the server validates FOUR
// fields (`network_mode`, `reasoning_effort`, `agent_backend`, `matter_ws_url`)
// and accepts any type-valid value for the rest. So for most settings a bad
// value is not refused — it is stored, and then a parser downstream quietly
// falls back to a default. `quiet_hours_start = "10pm"` does not error; it
// makes `quiet_hours_cover` fail closed and the pond goes silent all day, with
// nothing anywhere saying why.
//
// These validators are therefore the ONLY place several of these mistakes are
// ever reported to a person. They are deliberately conservative: they reject
// what is definitely wrong and stay quiet otherwise, because a false rejection
// blocks a save the server would have accepted.
//
// They never REPLACE the server's checks. The four the server enforces are
// mirrored here so the message arrives before the round-trip, not instead of it.

/** `null` means valid. A string is the message shown under the control. */
export type Validator = (value: unknown) => string | null;

const isBlank = (v: unknown): boolean =>
  v == null || (typeof v === "string" && v.trim() === "");

/** Passes anything empty. Compose with `required` when a value is mandatory. */
export function optional(v: Validator): Validator {
  return (value) => (isBlank(value) ? null : v(value));
}

export function required(message = "This cannot be empty."): Validator {
  return (value) => (isBlank(value) ? message : null);
}

/** Local 24-hour clock time, `HH:MM`. */
export const hhmm: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!/^\d{1,2}:\d{2}$/.test(s)) return "Use a 24-hour time like 22:00.";
  const [h, m] = s.split(":").map(Number);
  if (h > 23) return "Hours run from 00 to 23.";
  if (m > 59) return "Minutes run from 00 to 59.";
  return null;
};

/** A URL restricted to the schemes a given field can actually open. */
export function url(schemes: string[], example: string): Validator {
  return (value) => {
    const s = String(value ?? "").trim();
    if (!schemes.some((p) => s.startsWith(p))) {
      return `Must start with ${schemes.join(" or ")} — for example ${example}`;
    }
    try {
      new URL(s);
    } catch {
      return `That is not a complete address. Try ${example}`;
    }
    return null;
  };
}

export function range(min: number, max: number, unit = ""): Validator {
  return (value) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return "Enter a number.";
    if (n < min || n > max) return `Must be between ${min} and ${max}${unit ? ` ${unit}` : ""}.`;
    return null;
  };
}

export function atLeast(min: number, unit = ""): Validator {
  return (value) => {
    const n = Number(value);
    if (!Number.isFinite(n)) return "Enter a number.";
    if (n < min) return `Must be ${min}${unit ? ` ${unit}` : ""} or more.`;
    return null;
  };
}

export const integer: Validator = (value) => {
  const n = Number(value);
  if (!Number.isFinite(n)) return "Enter a number.";
  if (!Number.isInteger(n)) return "Enter a whole number.";
  return null;
};

/** Every validator in order; the first complaint wins. */
export function all(...vs: Validator[]): Validator {
  return (value) => {
    for (const v of vs) {
      const msg = v(value);
      if (msg) return msg;
    }
    return null;
  };
}

export function oneOf(options: readonly string[]): Validator {
  return (value) =>
    options.includes(String(value)) ? null : `Choose one of ${options.join(", ")}.`;
}

/**
 * `network 14, sensor 7` — a category name and a whole number of days.
 *
 * Typed rather than picked because the category set is server-side and open;
 * the check is that each pair parses, so a typo is caught before it becomes an
 * entry nothing matches.
 */
export const retentionMap: Validator = (value) => {
  if (value == null) return null;
  if (typeof value === "object" && !Array.isArray(value)) {
    for (const [k, n] of Object.entries(value as Record<string, unknown>)) {
      if (!/^[a-z_]+$/.test(k)) return `“${k}” is not a category name.`;
      if (!Number.isInteger(Number(n)) || Number(n) < 0) {
        return `“${k}” needs a whole number of days.`;
      }
    }
    return null;
  }
  return "Write pairs like: network 14, sensor 7";
};

/**
 * Comma-separated notification categories.
 *
 * Empty is REJECTED rather than treated as "all": server-side an unrecognised
 * or blank entry matches nothing, so an empty box silently means the pond never
 * speaks — which is the opposite of what someone typing here intends.
 */
export const speechCategories: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!s) return "Name at least one category, such as alert.";
  const parts = s.split(",").map((p) => p.trim()).filter(Boolean);
  if (!parts.length) return "Name at least one category, such as alert.";
  const bad = parts.filter((p) => !/^[a-z_]+$/.test(p));
  if (bad.length) return `Not a category: ${bad.join(", ")}. Use lower-case names like alert, info.`;
  return null;
};

/** Latitude / longitude, in decimal degrees. */
export const latitude = range(-90, 90, "degrees");
export const longitude = range(-180, 180, "degrees");

/**
 * An IANA zone the running system actually recognises.
 *
 * Checked against the platform rather than a bundled list, so a zone this
 * machine can resolve is never rejected for being absent from our own table.
 */
export const ianaTimezone: Validator = (value) => {
  const s = String(value ?? "").trim();
  if (!s) return "Choose a time zone.";
  try {
    new Intl.DateTimeFormat("en", { timeZone: s });
    return null;
  } catch {
    return `“${s}” is not a time zone this device knows.`;
  }
};

/** The system's own zone, e.g. "Africa/Nairobi". Local, no network. */
export function detectTimezone(): string | null {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || null;
  } catch {
    return null;
  }
}
