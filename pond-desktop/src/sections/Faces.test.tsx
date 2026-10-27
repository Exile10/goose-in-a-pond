import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import "../i18n";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listProfiles: vi.fn(async () => ({
      profiles: [{ id: "amina", display_name: "Amina", avatar_emoji: "" }],
    })),
    listFaceEnrollments: vi.fn(),
    registerFace: vi.fn(),
    identifyFaceBurst: vi.fn(),
    deleteUserBiometrics: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { Faces } from "./Faces";

describe("Faces while the Pond's face models are not ready", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("says the models are downloading, with progress, and that no restart is needed", async () => {
    vi.mocked(api.listFaceEnrollments).mockRejectedValue(
      new ApiError(503, "Face recognition is still downloading its models.", "face_models_downloading", {
        code: "face_models_downloading",
        progress: { file: "adaface_ir101.onnx", downloaded_bytes: 50 * 1_048_576, total_bytes: 261 * 1_048_576 },
      }),
    );
    render(<Faces />);
    expect(await screen.findByText(/downloading its face models/)).toBeTruthy();
    expect(screen.getByText(/adaface_ir101\.onnx: 50 of 261 MB/)).toBeTruthy();
    expect(screen.getByText(/no need to restart/)).toBeTruthy();
    expect(screen.queryByText(/rebuild the server/)).toBeNull();
  });

  it("passes on why recognition could not start", async () => {
    vi.mocked(api.listFaceEnrollments).mockRejectedValue(
      new ApiError(
        503,
        "Face recognition is unavailable. No face embedding model could be downloaded.",
        "face_recognition_unavailable",
        { code: "face_recognition_unavailable", reason: "No face embedding model could be downloaded." },
      ),
    );
    render(<Faces />);
    expect(await screen.findByText(/could not start on this Pond/)).toBeTruthy();
    expect(
      screen.getByText("Face recognition could not start on this Pond. No face embedding model could be downloaded."),
    ).toBeTruthy();
    expect(screen.queryByText(/rebuild the server/)).toBeNull();
  });

  it("still tells a build without face support to rebuild", async () => {
    vi.mocked(api.listFaceEnrollments).mockRejectedValue(
      new ApiError(503, "Face recognition is not configured on this server"),
    );
    render(<Faces />);
    expect(await screen.findByText(/rebuild the server with face support/)).toBeTruthy();
  });
});
