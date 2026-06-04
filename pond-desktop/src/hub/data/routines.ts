// ─── Routines Data ─────────────────────────────────────────────
// Ported from goose-hub-settings.jsx ROUTINE_DETAIL constant.
// Routines are on-demand scenes/macros that execute immediately on tap.
// They are NOT time-triggered schedules — see Phase 6 notes in scratchpad.md.

import { sunEl, filmEl, focusEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import type React from "react";

export type RoutineId = "morning" | "night" | "movie" | "away" | "focus";

export interface RoutineDetail {
  id: RoutineId;
  name: string;
  /**
   * SVG icon content — either a path string (d attribute) or a ReactNode for
   * multi-path compound icons. HubIco handles both via typeof check.
   */
  iconPath: string | React.ReactNode;
  /** Primary accent color (for Run button border + filled state) */
  color: string;
  /** Gradient CSS string (for icon square background) */
  bg: string;
  /** Chip labels describing what the routine does */
  does: string[];
  /** Human-readable time/trigger meta */
  time: string;
}

export const ROUTINES: RoutineDetail[] = [
  {
    id: "morning",
    name: "Good Morning",
    iconPath: sunEl,
    color: "#F59E0B",
    bg: "linear-gradient(150deg,#FCD34D,#F59E0B)",
    does: ["Lights to 60%", "Heat to 70°", "Brew coffee", "Read briefing"],
    time: "7:00 AM · weekdays",
  },
  {
    id: "night",
    name: "Good Night",
    iconPath: HP_PATHS.moon,
    color: "#6366F1",
    bg: "linear-gradient(150deg,#818CF8,#4F46E5)",
    does: ["Lock all doors", "Lights off", "Heat to 66°", "Arm security"],
    time: "11:00 PM · daily",
  },
  {
    id: "movie",
    name: "Movie Time",
    iconPath: filmEl,
    color: "#7C3AED",
    bg: "linear-gradient(150deg,#A78BFA,#7C3AED)",
    does: ["Dim to 20%", "Close blinds", "TV on", "Mute notifications"],
    time: "On demand",
  },
  {
    id: "away",
    name: "Away",
    iconPath: HP_PATHS.away,
    color: "#0D9488",
    bg: "linear-gradient(150deg,#2DD4BF,#0D9488)",
    does: ["Lock up", "Eco climate", "Cameras armed", "Lights off"],
    time: "When everyone leaves",
  },
  {
    id: "focus",
    name: "Focus",
    iconPath: focusEl,
    color: "#EC4899",
    bg: "linear-gradient(150deg,#F472B6,#DB2777)",
    does: ["Do not disturb", "Desk lamp on", "Lo-fi playlist", "Heat to 71°"],
    time: "On demand",
  },
];
