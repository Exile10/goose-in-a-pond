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
