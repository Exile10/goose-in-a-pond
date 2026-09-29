// Whether this window can play protected audio at all, asked of the browser rather than assumed
// from the Electron build: stock Electron has no key systems, castlabs' has Widevine once its
// module has downloaded, and the difference is worth saying in words.

/**
 * Widevine modules the streaming services are known to refuse, and why. Saying so up front beats
 * letting a song start and die a few seconds in, which is what these do. A module that Google or
 * castlabs later fixes has a different version, so it is not caught here.
 */
export const KNOWN_BAD_MODULES: Readonly<Record<string, string>> = {
  "4.10.3050.0":
    "it revokes licenses and stops playback on Apple Music and Spotify (castlabs/electron-releases#237), and a fixed build, 4.10.3050.1, has not reached this app's updater",
};

/**
 * A sentence for a person, or null when Widevine is available. `what` is what could not play, and
 * `version` is the module the shell loaded, when it knows (the browser does not say).
 */
export async function widevineProblem(
  what = "protected music",
  version?: string,
): Promise<string | null> {
  const bad = version ? KNOWN_BAD_MODULES[version] : undefined;
  if (bad) {
    return `The Widevine module in this app (${version}) is one ${what} refuses: ${bad}. ${what} cannot play here until that changes.`;
  }
  if (typeof navigator.requestMediaKeySystemAccess !== "function") {
    return `This window cannot play ${what}: it has no DRM support.`;
  }
  try {
    await navigator.requestMediaKeySystemAccess("com.widevine.alpha", [
      {
        initDataTypes: ["cenc"],
        audioCapabilities: [{ contentType: 'audio/mp4; codecs="mp4a.40.2"' }],
      },
    ]);
    return null;
  } catch {
    return `This build of Goose In A Pond has no Widevine module, so it cannot play ${what}. If the module is still downloading, try again in a minute.`;
  }
}
