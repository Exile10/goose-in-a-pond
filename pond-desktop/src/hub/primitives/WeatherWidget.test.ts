import { describe, it, expect } from "vitest";
import { dayPhaseFor } from "./WeatherWidget";

describe("dayPhaseFor", () => {
  const sunrise = "06:30";
  const sunset = "20:15";

  it("returns dawn near sunrise", () => {
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 6, 30))).toBe("dawn");
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 6, 50))).toBe("dawn");
  });

  it("returns dusk near sunset", () => {
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 20, 15))).toBe("dusk");
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 19, 45))).toBe("dusk");
  });

  it("returns day between the twilight windows", () => {
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 12, 0))).toBe("day");
  });

  it("returns night outside sunrise/sunset entirely", () => {
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 2, 0))).toBe("night");
    expect(dayPhaseFor(sunrise, sunset, new Date(2026, 0, 1, 23, 0))).toBe("night");
  });

  it("falls back to day when sunrise/sunset are malformed", () => {
    expect(dayPhaseFor("--:--", "--:--", new Date(2026, 0, 1, 2, 0))).toBe("day");
  });
});
