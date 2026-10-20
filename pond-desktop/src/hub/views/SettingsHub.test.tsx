import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("../../state/useAppContext", () => ({ useAppDispatch: () => vi.fn() }));

import { SettingsHubView } from "./SettingsHub";
import { SETTINGS } from "../data/settingsConfig";

afterEach(cleanup);

describe("the Hub's Settings list", () => {
  it("lists Connections, opened on its own route and not on Accounts' route", () => {
    const go = vi.fn();
    render(<SettingsHubView go={go} />);
    fireEvent.click(screen.getByRole("button", { name: "Connections" }));
    expect(go).toHaveBeenCalledWith("network");

    fireEvent.click(screen.getByRole("button", { name: "Accounts" }));
    expect(go).toHaveBeenLastCalledWith("connections");
  });

  it("describes Connections by what it covers", () => {
    const row = SETTINGS.flatMap((g) => g.rows).find((r) => r.id === "network");
    expect(row?.label).toBe("Connections");
    expect(row?.sub).toBe("Wi-Fi, cable, Bluetooth and pairing");
  });

  it("gives every row its own route", () => {
    const ids = SETTINGS.flatMap((g) => g.rows).map((r) => r.id);
    expect(new Set(ids).size).toBe(ids.length);
  });
});
