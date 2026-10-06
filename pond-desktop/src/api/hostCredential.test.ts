import { describe, expect, it, vi } from "vitest";
import { captureSignInLink, followSignInLinks, isSignInRequired } from "./hostCredential";
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

describe("followSignInLinks", () => {
  function tab(hash: string) {
    let listener: (() => void) | undefined;
    const target = {
      addEventListener: vi.fn((_: string, handler: () => void) => { listener = handler; }),
      location: { hash, pathname: "/", search: "" },
      history: { replaceState: vi.fn() },
      sessionStorage: { setItem: vi.fn() },
    };
    const reload = vi.fn();
    followSignInLinks(target, reload);
    return { open: (next: string) => { target.location.hash = next; listener?.(); }, reload, target };
  }

  it("signs in again from a link opened in a running tab, and reloads", () => {
    const { open, reload, target } = tab("");
    expect(target.addEventListener).toHaveBeenCalledWith("hashchange", expect.any(Function));
    open(`#host=${credential}`);
    expect(target.sessionStorage.setItem).toHaveBeenCalledWith("giap-host-credential", credential);
    expect(reload).toHaveBeenCalledTimes(1);
  });

  it("does not reload for any other change of fragment", () => {
    const { open, reload } = tab("");
    open("#section=pairing");
    open("#host=not-a-credential");
    expect(reload).not.toHaveBeenCalled();
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
