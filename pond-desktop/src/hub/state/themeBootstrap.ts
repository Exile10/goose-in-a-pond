/**
 * themeBootstrap.ts
 * Import this at the very top of main.tsx (before React renders) so dark
 * mode is applied to <html> before the first paint, preventing FOUC.
 *
 * The side-effect is in themeStore.ts module load — importing themeStore
 * here is sufficient; we don't need to call anything explicitly.
 */
import "./themeStore";
