import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen, within } from "@testing-library/react";
import type { MarketplaceExtension, SecretRequirement } from "../api/types";

const getPlayerState = vi.hoisted(() => vi.fn());
vi.mock("../shell", () => ({ isDesktopShell: () => true, invoke: vi.fn() }));
vi.mock("../api/PondApiClient", async (importOriginal) => {
  const original = await importOriginal<typeof import("../api/PondApiClient")>();
  return { ...original, api: { ...original.api, getPlayerState } };
});

import { SecretConfigModal } from "./Extensions";

const secret = (over: Partial<SecretRequirement> & Pick<SecretRequirement, "key">): SecretRequirement => ({
  display_name: over.key,
  description: "",
  required: false,
  kind: "generic",
  ...over,
});

/** The music entry as the registry describes it. */
const music: MarketplaceExtension = {
  id: "music",
  name: "Music",
  description: "Play music",
  kind: "stdio",
  args: [],
  category: "entertainment",
  featured: true,
  tools: [],
  required_secrets: [
    secret({ key: "SPOTIFY_ACCESS_TOKEN", display_name: "Spotify", kind: "oauth_flow" }),
    secret({ key: "MUSIC_SERVICE", display_name: "Preferred service", advanced: true }),
    secret({ key: "APPLE_MUSIC_TEAM_ID", display_name: "Apple Music Team ID", advanced: true, host_only: true }),
    secret({ key: "APPLE_MUSIC_KEY_ID", display_name: "Apple Music Key ID", advanced: true, host_only: true }),
    secret({
      key: "APPLE_MUSIC_PRIVATE_KEY",
      display_name: "Apple Music private key",
      kind: "api_key",
      advanced: true,
      host_only: true,
    }),
  ],
} as unknown as MarketplaceExtension;

const github = {
  id: "github",
  name: "GitHub",
  description: "",
  kind: "stdio",
  args: [],
  category: "dev",
  featured: false,
  tools: [],
  required_secrets: [
    secret({ key: "GITHUB_TOKEN", display_name: "GitHub token", kind: "api_key", required: true }),
  ],
} as unknown as MarketplaceExtension;

const props = { mode: "edit" as const, onClose: () => undefined, onComplete: async () => undefined };

beforeEach(() => {
  getPlayerState.mockReset();
  getPlayerState.mockResolvedValue({ attached: true, state: { need: "authorization" } });
});
afterEach(cleanup);

describe("the Music extension's settings", () => {
  it("leads with a sign-in for each service, and Apple Music's is one button", async () => {
    render(<SecretConfigModal ext={music} {...props} />);
    expect(await screen.findByRole("button", { name: /sign in to apple music/i })).toBeTruthy();
    expect(screen.getByRole("button", { name: /sign in with spotify/i })).toBeTruthy();
  });

  it("keeps the key fields and the service picker under Developer settings, closed", () => {
    render(<SecretConfigModal ext={music} {...props} />);
    const summary = screen.getByText("Developer settings");
    const panel = summary.closest("details") as HTMLDetailsElement;
    expect(panel).toBeTruthy();
    expect(panel.open).toBe(false);

    for (const label of [
      "Preferred service",
      "Apple Music Team ID",
      "Apple Music Key ID",
      "Apple Music private key",
    ]) {
      expect(within(panel).getByText(label), `${label} lives under Developer settings`).toBeTruthy();
    }
  });

  it("shows none of them outside that panel", () => {
    const { container } = render(<SecretConfigModal ext={music} {...props} />);
    const panel = container.querySelector("details") as HTMLElement;
    for (const label of ["Preferred service", "Apple Music Team ID", "Apple Music private key"]) {
      const shown = screen.getAllByText(label);
      expect(shown.every((el) => panel.contains(el)), `${label} is only inside the panel`).toBe(true);
    }
  });

  it("says how many developer settings are already saved, so a custom key is not hidden from view", () => {
    render(
      <SecretConfigModal
        ext={music}
        {...props}
        fulfilledMap={{ APPLE_MUSIC_TEAM_ID: true, APPLE_MUSIC_KEY_ID: true }}
      />,
    );
    expect(screen.getByText("2 saved")).toBeTruthy();
  });

  it("does not claim anything is saved when nothing is", () => {
    render(<SecretConfigModal ext={music} {...props} />);
    expect(screen.queryByText(/saved$/)).toBeNull();
  });
});

describe("an extension with no advanced fields", () => {
  it("looks as it always did: its field, no Developer settings, no Apple sign-in", () => {
    render(<SecretConfigModal ext={github} {...props} />);
    expect(screen.getByText("GitHub token")).toBeTruthy();
    expect(screen.queryByText("Developer settings")).toBeNull();
    expect(screen.queryByRole("button", { name: /apple music/i })).toBeNull();
    expect(getPlayerState).not.toHaveBeenCalled();
  });
});
