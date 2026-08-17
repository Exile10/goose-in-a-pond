import { describe, expect, it } from "vitest";
import {
  DESKTOP_MODES,
  DESKTOP_SECTIONS,
  getDesktopInitials,
  getDesktopSectionLabel,
  normalizeDesktopMode,
  normalizeGuiSection,
} from "./desktopState";

describe("desktopState", () => {
  it("normalizes the desktop mode with a gui fallback", () => {
    expect(normalizeDesktopMode("gui")).toBe("gui");
    expect(normalizeDesktopMode("voice")).toBe("voice");
    // "canvas" was a real mode before Canvas was consolidated into a GuiSection —
    // a stale persisted value should now self-heal back to "gui".
    expect(normalizeDesktopMode("canvas")).toBe("gui");
    expect(normalizeDesktopMode("unexpected")).toBe("gui");
    expect(normalizeDesktopMode(null)).toBe("gui");
    expect(normalizeDesktopMode(undefined)).toBe("gui");
    expect(DESKTOP_MODES).toEqual(["gui", "voice"]);
  });

  it("normalizes the gui section with a dashboard fallback", () => {
    expect(normalizeGuiSection("dashboard")).toBe("dashboard");
    expect(normalizeGuiSection("chat")).toBe("chat");
    expect(normalizeGuiSection("models")).toBe("models");
    expect(normalizeGuiSection("prompts")).toBe("prompts");
    expect(normalizeGuiSection("settings")).toBe("settings");
    expect(normalizeGuiSection("devices")).toBe("devices");
    expect(normalizeGuiSection("pairing")).toBe("pairing");
    expect(normalizeGuiSection("schedules")).toBe("schedules");
    expect(normalizeGuiSection("memory")).toBe("memory");
    expect(normalizeGuiSection("skills")).toBe("skills");
    // Unknown sections fall back to "dashboard"
    expect(normalizeGuiSection("unexpected")).toBe("dashboard");
    expect(normalizeGuiSection(null)).toBe("dashboard");
    expect(normalizeGuiSection(undefined)).toBe("dashboard");
  });

  it("DESKTOP_SECTIONS is a flat ordered list of all sidebar items", () => {
    // 14 = dashboard, chat, devices, mesh, pairing, schedules, memory,
    // connections, skills, logs, models, prompts, settings, extensions.
    // (canvas, faces, hub are routable but hidden from the sidebar, so they are
    // in GUI_SECTIONS and not here — see HIDDEN_SECTIONS in desktopState.ts)
    expect(DESKTOP_SECTIONS).toHaveLength(14);
    expect(DESKTOP_SECTIONS[0]).toEqual({ section: "dashboard", label: "Home" });
    expect(DESKTOP_SECTIONS[1]).toEqual({ section: "chat", label: "Chat" });
    const sectionKeys = DESKTOP_SECTIONS.map((s) => s.section);
    expect(sectionKeys).toContain("devices");
    expect(sectionKeys).toContain("mesh");
    expect(sectionKeys).toContain("pairing");
    expect(sectionKeys).toContain("schedules");
    expect(sectionKeys).toContain("memory");
    expect(sectionKeys).toContain("connections");
    expect(sectionKeys).toContain("skills");
    expect(sectionKeys).toContain("extensions");
    expect(sectionKeys).toContain("models");
    expect(sectionKeys).toContain("prompts");
    expect(sectionKeys).toContain("settings");
    expect(sectionKeys).toContain("logs");
  });

  it("hidden sections are still valid for normalizeGuiSection", () => {
    expect(normalizeGuiSection("faces")).toBe("faces");
    expect(normalizeGuiSection("canvas")).toBe("canvas");
  });

  it("derives consistent desktop section labels", () => {
    // The route id stays `dashboard` — it is persisted as `giap-section`, so
    // renaming it would strand anyone whose app reopens on this screen. Only
    // the label people read changed.
    expect(getDesktopSectionLabel("dashboard")).toBe("Home");
    expect(getDesktopSectionLabel("chat")).toBe("Chat");
    expect(getDesktopSectionLabel("settings")).toBe("Settings");
    expect(getDesktopSectionLabel("models")).toBe("Models");
  });

  it("derives initials from assistant name", () => {
    expect(getDesktopInitials("Goose In A Pond")).toBe("GI");
    expect(getDesktopInitials("Pond")).toBe("P");
    expect(getDesktopInitials("A B")).toBe("AB");
    expect(getDesktopInitials("")).toBe("?");
  });
});
