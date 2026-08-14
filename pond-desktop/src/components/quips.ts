/**
 * Quips — the short lines the pond greets you with, and the ones it shows
 * while it is busy.
 *
 * Shared rather than living in the chat screen, because the same voice should
 * turn up anywhere the assistant has a moment to fill: an empty conversation,
 * a working indicator, an empty list.
 *
 * Rules that keep this from ageing badly:
 *
 * - Never more than one clause. These sit next to a heading, not instead of it.
 * - No exclamation marks and no jokes about being an AI. The pond lives in
 *   someone's house; it should sound like a housemate, not a mascot.
 * - Nothing that claims capability ("Ready for anything"). It sets an
 *   expectation the model then has to meet.
 * - A name is optional everywhere. It comes from the `user_name` setting at
 *   runtime and is never written into this file; where these docs need to show
 *   one, they use the placeholder "user". A blank name falls through to the
 *   anonymous set, so every path reads without it.
 */

/** Morning / afternoon / evening / night, from the local clock. */
export function timeOfDay(now: Date = new Date()): "morning" | "afternoon" | "evening" | "night" {
  const h = now.getHours();
  // The small hours are handled FIRST. Ordering this after the morning test
  // let 04:00 fall through to `h < 17` and greet you with "Good afternoon".
  if (h < 5) return "night";
  if (h < 12) return "morning";
  if (h < 17) return "afternoon";
  if (h < 22) return "evening";
  return "night";
}

const GREETINGS_WITH_NAME: Record<ReturnType<typeof timeOfDay>, string[]> = {
  morning: [
    "Good morning, {name}",
    "{name} returns",
    "Morning, {name}",
    "Early start, {name}",
  ],
  afternoon: [
    "Good afternoon, {name}",
    "{name} returns",
    "Afternoon, {name}",
    "Back again, {name}",
  ],
  evening: [
    "Good evening, {name}",
    "{name} returns",
    "Evening, {name}",
    "Winding down, {name}?",
  ],
  night: [
    "Still up, {name}?",
    "{name} returns",
    "Late one, {name}",
    "Good evening, {name}",
  ],
};

const GREETINGS_ANONYMOUS: string[] = [
  "What can I help with?",
  "Where would you like to start?",
  "The pond is listening",
  "Ask me anything",
];

/** Shown under the greeting. Quiet, factual, and true of this product. */
const SUBTITLES: string[] = [
  "Everything you type stays on this device.",
  "Running on-device — nothing leaves your home.",
  "No cloud, no account, no telemetry you did not switch on.",
];

/** Shown while a turn is running. Present tense, never a promise. */
export const WORKING_QUIPS: string[] = [
  "Thinking",
  "Working on it",
  "Reading the pond",
  "Checking what I know",
  "Putting that together",
  "Following the thread",
];

/**
 * Pick deterministically from `list` using `seed`.
 *
 * Deliberately not `Math.random()`: React may render a component more than
 * once for the same state, and a random pick would change the greeting on a
 * re-render while you were reading it. Passing a seed that only changes when
 * the conversation does keeps the line stable for as long as it is on screen.
 */
export function pick<T>(list: readonly T[], seed: number): T {
  const i = Math.abs(Math.trunc(seed)) % list.length;
  return list[i];
}

/**
 * The greeting for an empty conversation.
 *
 * @param name  Whatever `user_name` holds — passed in, never assumed. An empty
 *              value uses the anonymous set rather than inventing a
 *              placeholder, because "Good morning, user" is worse than a line
 *              that simply does not need a name.
 * @param seed  Stable per conversation — pass something that changes when a
 *              new chat starts, not on every render.
 */
export function greeting(name: string | undefined, seed: number, now: Date = new Date()): string {
  const trimmed = (name ?? "").trim();
  if (!trimmed) return pick(GREETINGS_ANONYMOUS, seed);
  // Whatever is stored is what the household chose to be called. Second-guessing
  // it here — an earlier version skipped a particular default string — is not
  // this function's business.
  return pick(GREETINGS_WITH_NAME[timeOfDay(now)], seed).replace("{name}", trimmed);
}

/** The line under the greeting. */
export function subtitle(seed: number): string {
  return pick(SUBTITLES, seed);
}

/** A line for the working indicator. */
export function workingQuip(seed: number): string {
  return pick(WORKING_QUIPS, seed);
}
