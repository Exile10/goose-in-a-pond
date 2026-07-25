import { vi } from "vitest";

// HeroUI uses framer-motion internally; happy-dom doesn't support its browser APIs.
vi.mock("framer-motion", () => ({
  motion: new Proxy({}, { get: (_t: object, tag: string) => tag }),
  AnimatePresence: ({ children }: { children: unknown }) => children,
  useReducedMotion: () => false,
  useAnimation: () => ({ start: vi.fn(), stop: vi.fn() }),
  useMotionValue: (v: unknown) => ({ get: () => v, set: vi.fn() }),
}));

// HeroUI Tabs (react-aria-components) uses the Web Animations API.
// happy-dom does not implement getAnimations() — polyfill it globally.
if (typeof Element !== "undefined" && !Element.prototype.getAnimations) {
  Element.prototype.getAnimations = () => [];
}

// happy-dom 20 implements Storage via prototype accessors, which vitest's
// global copying flattens into a bare object — localStorage.setItem ends up
// undefined in tests. Replace it with a real in-memory Storage so client code
// (token persistence, per-instance client id) behaves as in the browser.
if (typeof globalThis.localStorage?.setItem !== "function") {
  const store = new Map<string, string>();
  const memoryStorage: Storage = {
    get length() {
      return store.size;
    },
    getItem: (k: string) => (store.has(k) ? (store.get(k) as string) : null),
    setItem: (k: string, v: string) => {
      store.set(k, String(v));
    },
    removeItem: (k: string) => {
      store.delete(k);
    },
    clear: () => {
      store.clear();
    },
    key: (i: number) => [...store.keys()][i] ?? null,
  };
  Object.defineProperty(globalThis, "localStorage", {
    value: memoryStorage,
    configurable: true,
  });
  if (typeof window !== "undefined") {
    Object.defineProperty(window, "localStorage", {
      value: memoryStorage,
      configurable: true,
    });
  }
}
