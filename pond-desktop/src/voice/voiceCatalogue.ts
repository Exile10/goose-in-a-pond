/**
 * Display metadata for Kokoro voices, derived from the voice id.
 *
 * Kokoro names every voice `<lang><gender>_<name>` — `af_heart` is American
 * Female "Heart", `bm_george` is British Male "George". That means the picker
 * does not need a hand-maintained table of 50-odd voices that drifts every time
 * the model repo adds one: the id already says everything the UI shows.
 *
 * An unrecognised prefix degrades to the raw id rather than being hidden, so a
 * newly shipped voice is still selectable before anyone updates this file.
 */

/** Quality tiers, in the order the UI should offer them. */
export const VOICE_QUALITY_TIERS = [
  {
    value: "q8",
    label: "Balanced",
    detail: "92 MB — the default. Fits comfortably beside the language model.",
    sizeMb: 92,
  },
  {
    value: "q8f16",
    label: "Compact",
    detail: "86 MB — smallest. Best on constrained hardware.",
    sizeMb: 86,
  },
  {
    value: "q4f16",
    label: "Small",
    detail: "155 MB — mid-size, four-bit weights.",
    sizeMb: 155,
  },
  {
    value: "fp16",
    label: "High",
    detail: "163 MB — half precision, closer to reference quality.",
    sizeMb: 163,
  },
  {
    value: "fp32",
    label: "Reference",
    detail: "326 MB — full precision. Slowest, and heavy on memory.",
    sizeMb: 326,
  },
] as const;

export type VoiceQuality = (typeof VOICE_QUALITY_TIERS)[number]["value"];

/** The tier shipped by default. */
export const DEFAULT_QUALITY: VoiceQuality = "q8";

/**
 * The tier first-run setup picks: the smallest one.
 *
 * Onboarding is the worst moment to spend 92 MB — the household is waiting to
 * hear the thing speak for the first time, on whatever connection they have.
 * Compact is 86 MB and sounds close enough to judge a voice by; the Models
 * page then says plainly whether a bigger tier fits this device, which is a
 * decision worth making once the pond is actually running.
 */
export const ONBOARDING_QUALITY: VoiceQuality = "q8f16";
/** The voice shipped by default — Kokoro's own reference voice. */
export const DEFAULT_VOICE = "af_heart";

/** Pace bounds, matching the adapter's clamp. Stored as a multiplier. */
export const MIN_PACE = 0.5;
export const MAX_PACE = 2.0;
export const DEFAULT_PACE = 1.0;

const LANGUAGES: Record<string, string> = {
  a: "American English",
  b: "British English",
  e: "Spanish",
  f: "French",
  h: "Hindi",
  i: "Italian",
  j: "Japanese",
  p: "Portuguese",
  z: "Mandarin",
};

const GENDERS: Record<string, string> = { f: "Female", m: "Male" };

export interface VoiceInfo {
  /** The id the backend uses, e.g. `af_heart`. */
  id: string;
  /** Display name, e.g. "Heart". */
  name: string;
  /** e.g. "American English", or null when the prefix is unknown. */
  language: string | null;
  /** "Female" | "Male", or null when unknown. */
  gender: string | null;
  /** Grouping label for the picker, e.g. "American English · Female". */
  group: string;
}

/** Title-case a voice's name segment: `van_dyke` → "Van Dyke". */
function titleCase(segment: string): string {
  return segment
    .split(/[_-]/)
    .filter(Boolean)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** Parse one Kokoro voice id into what the UI shows. */
export function describeVoice(id: string): VoiceInfo {
  const match = /^([a-z])([fm])_(.+)$/.exec(id);
  if (!match) {
    // Unknown shape — still selectable, just ungrouped.
    return { id, name: titleCase(id), language: null, gender: null, group: "Other" };
  }
  const [, langKey, genderKey, rest] = match;
  const language = LANGUAGES[langKey] ?? null;
  const gender = GENDERS[genderKey] ?? null;
  if (!language || !gender) {
    return { id, name: titleCase(rest), language, gender, group: "Other" };
  }
  return { id, name: titleCase(rest), language, gender, group: `${language} · ${gender}` };
}

/**
 * Group voices for a `<select>`, preserving a sensible order: English first
 * (the pond speaks English), then everything else alphabetically, with "Other"
 * last so unrecognised ids never bury the real choices.
 */
export function groupVoices(ids: string[]): { group: string; voices: VoiceInfo[] }[] {
  const groups = new Map<string, VoiceInfo[]>();
  for (const info of ids.map(describeVoice)) {
    const list = groups.get(info.group) ?? [];
    list.push(info);
    groups.set(info.group, list);
  }

  const rank = (g: string) => {
    if (g === "Other") return 3;
    if (g.startsWith("American English")) return 0;
    if (g.startsWith("British English")) return 1;
    return 2;
  };

  return [...groups.entries()]
    .map(([group, voices]) => ({
      group,
      voices: voices.sort((a, b) => a.name.localeCompare(b.name)),
    }))
    .sort((a, b) => rank(a.group) - rank(b.group) || a.group.localeCompare(b.group));
}

/**
 * The catalogue title for a voice: `af_heart` → `Af_Heart`.
 *
 * Keeps the accent/gender prefix visible — which is information, not noise, in
 * a list of fifty voices — while reading as a name rather than a filename. The
 * picker uses the shorter `describeVoice().name` ("Heart") because it groups by
 * accent already, so the prefix would be said twice.
 *
 * Derived rather than stored: `ModelRecord.name` is the id the engine resolves
 * `<name>.bin` from and must stay lowercase, so the title cannot simply be the
 * name field.
 */
export function voiceTitle(id: string): string {
  return id
    .split("_")
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join("_");
}


/**
 * Overall grades, verbatim from Kokoro's own `VOICES.md`.
 *
 * The grade estimates the quality *and quantity* of a voice's training data,
 * and it is the single most useful thing the model page knows that a name and
 * an accent cannot tell you — `af_heart` at **A** and `am_adam` at **F+** are
 * the same size download and sound nothing alike in fidelity.
 *
 * Most pickers hide this. Showing it is the difference between choosing a
 * voice and guessing at one.
 *
 * English only: those are the voices the catalogue seeds. An unlisted voice
 * simply has no grade shown rather than a fabricated one.
 */
const VOICE_GRADES: Record<string, string> = {
  // American English — female
  af_heart: "A", af_bella: "A-", af_nicole: "B-", af_aoede: "C+", af_kore: "C+",
  af_sarah: "C+", af_alloy: "C", af_nova: "C", af_sky: "C-", af_jessica: "D",
  af_river: "D",
  // American English — male
  am_fenrir: "C+", am_michael: "C+", am_puck: "C+", am_echo: "D", am_eric: "D",
  am_liam: "D", am_onyx: "D", am_santa: "D-", am_adam: "F+",
  // British English — female
  bf_emma: "B-", bf_isabella: "C", bf_alice: "D", bf_lily: "D",
  // British English — male
  bm_fable: "C", bm_george: "C", bm_lewis: "D+", bm_daniel: "D",
};

/** Notes Kokoro's own table records as traits, for the few voices that carry one. */
const VOICE_NOTES: Record<string, string> = {
  af_heart: "Reference voice",
  af_bella: "Most training data",
  af_nicole: "Close-mic",
};

/** Kokoro's published grade for a voice, or null when it publishes none. */
export function gradeFor(id: string): string | null {
  return VOICE_GRADES[id] ?? null;
}

/** The trait Kokoro's table records, if any. */
export function noteFor(id: string): string | null {
  return VOICE_NOTES[id] ?? null;
}

/**
 * Rank a grade for sorting: A before F, so the picker leads with the voices
 * worth hearing first. Ungraded voices sort last rather than pretending to a
 * middle position they were never given.
 */
export function gradeRank(id: string): number {
  const g = VOICE_GRADES[id];
  if (!g) return 99;
  const letter = "ABCDF".indexOf(g[0]);
  const modifier = g[1] === "+" ? -0.3 : g[1] === "-" ? 0.3 : 0;
  return letter + modifier;
}

/**
 * What the pond says while you are choosing how it sounds.
 *
 * A voice is a thing you live with, and one sentence on repeat tells you very
 * little about it — you stop hearing it by the third play. These are ordinary
 * lines this assistant actually says, varied in length, rhythm and ending, so
 * a few plays cover statement, number, time and a soft close. Every one is
 * true to what the product does; none of them is a pangram or a demo phrase.
 */
export const PREVIEW_STATEMENTS: string[] = [
  "Your four o'clock moved to Thursday. I've left the morning open.",
  "It's sixty-eight inside, and clear until about four.",
  "The front door locked itself at eleven, same as it always does.",
  "I've turned the patio lights down to forty percent.",
  "You asked me to mention the water filter. It's been three months.",
  "Nothing needs you right now.",
  "I live here on your shelf, I think on my own, and nothing you say to me leaves this room.",
];

/**
 * The next statement to speak, given how many have already played.
 *
 * Cycles rather than randomising: comparing two voices is only fair if they
 * say the same thing, and a random line makes every comparison a new one.
 */
export function statementAt(playCount: number): string {
  return PREVIEW_STATEMENTS[playCount % PREVIEW_STATEMENTS.length];
}

/** Human label for a pace multiplier, for the slider readout. */
export function paceLabel(pace: number): string {
  if (pace < 0.7) return "Much slower";
  if (pace < 0.9) return "Slower";
  if (pace <= 1.1) return "Natural";
  if (pace <= 1.35) return "Faster";
  return "Much faster";
}

/** Clamp a pace value to what the engine accepts. */
export function clampPace(pace: number): number {
  if (!Number.isFinite(pace)) return DEFAULT_PACE;
  return Math.min(MAX_PACE, Math.max(MIN_PACE, pace));
}

/**
 * Roughly what a tier costs resident: the weights plus ONNX Runtime's arena.
 *
 * The arena is not a fixed number, so this is deliberately generous — the
 * point is to stop the UI recommending a tier that will fight the language
 * model for memory, and being optimistic there is the expensive mistake.
 */
export function tierCostMb(value: string): number {
  return Math.round(describeQuality(value).sizeMb * 1.6);
}

/**
 * The best tier that fits in `availableMb`, or the default when nothing is
 * known about the device.
 *
 * Tiers are declared smallest-benefit-first, so this walks them by size rather
 * than by list order.
 */
export function recommendedQuality(availableMb: number | null): VoiceQuality {
  if (availableMb == null || availableMb <= 0) return DEFAULT_QUALITY;
  const affordable = [...VOICE_QUALITY_TIERS]
    .filter((t) => tierCostMb(t.value) < availableMb)
    .sort((a, b) => b.sizeMb - a.sizeMb)[0];
  return (affordable?.value ?? DEFAULT_QUALITY) as VoiceQuality;
}

/**
 * One honest sentence about whether to move off the default.
 *
 * A tier is not a quality slider you should max out: precision mainly shows up
 * as fewer artefacts on long sentences, and it is paid for in memory the
 * language model also wants. So the advice names the trade rather than
 * recommending "higher is better", and it says nothing at all when the current
 * tier is already the right one.
 */
export function qualityAdvice(current: string, availableMb: number | null): string {
  const now = describeQuality(current);
  const best = describeQuality(recommendedQuality(availableMb));

  if (availableMb == null || availableMb <= 0) {
    return `${now.label} is the default and fits nearly anything. Higher tiers smooth out long sentences but cost memory the language model also wants.`;
  }
  if (tierCostMb(now.value) >= availableMb) {
    return `${now.label} wants about ${tierCostMb(now.value)} MB and this device has ${Math.round(availableMb)} MB free. Expect slower replies — ${best.label} is the one that fits.`;
  }
  if (best.sizeMb > now.sizeMb) {
    return `${best.label} would also fit here (${best.sizeMb} MB). It smooths out long sentences; ${now.label} is fine for short ones.`;
  }
  return `${now.label} is the best fit for this device — ${Math.round(availableMb)} MB free after the language model.`;
}

/** The tier's descriptor, falling back to the default rather than undefined. */
export function describeQuality(value: string | undefined) {
  return (
    VOICE_QUALITY_TIERS.find((t) => t.value === value) ??
    VOICE_QUALITY_TIERS.find((t) => t.value === DEFAULT_QUALITY)!
  );
}
