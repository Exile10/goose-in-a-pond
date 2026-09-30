import { describe, expect, it, vi } from "vitest";
import { captureSignInLink, isSignInRequired } from "./hostCredential";
import { ApiError } from "./types";

const credential = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJ-_01234";

function capture(hash: string) {
  const history = { replaceState: vi.fn() };
  const storage = { setItem: vi.fn() };
  const kept = captureSignInLink({ hash, pathname: "/", search: "?x=1" }, history, storage);
  return { kept, history, storage };
}

describe("captureSignInLink", () => {
  it("keeps the credential for the tab and clears it from the address bar", () => {
    expect(credential).toHaveLength(43);
    const { kept, history, storage } = capture(`#host=${credential}`);
    expect(kept).toBe(true);
    expect(storage.setItem).toHaveBeenCalledWith("giap-host-credential", credential);
    expect(history.replaceState).toHaveBeenCalledWith(null, "", "/?x=1");
  });

  it("leaves the rest of the fragment in place", () => {
    const { history } = capture(`#section=pairing&host=${credential}`);
    expect(history.replaceState).toHaveBeenCalledWith(null, "", "/?x=1#section=pairing");
  });

  it("clears but does not keep a malformed credential", () => {
    const { kept, history, storage } = capture("#host=not-a-credential");
    expect(kept).toBe(false);
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(history.replaceState).toHaveBeenCalled();
  });

  it("does nothing without a sign-in link", () => {
    const { kept, history, storage } = capture("#section=pairing");
    expect(kept).toBe(false);
    expect(storage.setItem).not.toHaveBeenCalled();
    expect(history.replaceState).not.toHaveBeenCalled();
  });
});

describe("isSignInRequired", () => {
  it("recognises only the host credential refusal", () => {
    expect(isSignInRequired(new ApiError(403, "host_credential_required"))).toBe(true);
    expect(isSignInRequired(new ApiError(403, "origin_not_allowed"))).toBe(false);
    expect(isSignInRequired(new ApiError(401, "host_credential_required"))).toBe(false);
    expect(isSignInRequired(new Error("host_credential_required"))).toBe(false);
  });
});
