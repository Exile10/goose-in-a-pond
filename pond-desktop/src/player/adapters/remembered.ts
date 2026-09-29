// Whether a person has signed in to a service in this window before, kept in the window's own storage.
// A service that has been signed in to starts at launch (to restore the session); one that has not
// sleeps until someone presses Sign in, so nothing leaves the pond for a household that never uses it.

export interface Remembered {
  remembered(): boolean;
  remember(): void;
  forget(): void;
}

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function windowStorage(): Store | undefined {
  try {
    return typeof localStorage === "undefined" ? undefined : localStorage;
  } catch {
    // Storage can be blocked outright; the service then simply never counts as signed in before.
    return undefined;
  }
}

/** A yes/no kept under `key`. Storage that is missing or throws reads as "no" and never breaks a sign-in. */
export function rememberedIn(key: string, store: Store | undefined = windowStorage()): Remembered {
  return {
    remembered() {
      try {
        return store?.getItem(key) === "1";
      } catch {
        return false;
      }
    },
    remember() {
      try {
        store?.setItem(key, "1");
      } catch {
        /* the next launch will sleep again, which costs one click */
      }
    },
    forget() {
      try {
        store?.removeItem(key);
      } catch {
        /* nothing to undo */
      }
    },
  };
}
