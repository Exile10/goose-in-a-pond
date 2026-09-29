// Whether this window can play protected audio at all, asked of the browser rather than assumed
// from the Electron build: stock Electron has no key systems, castlabs' has Widevine once its
// module has downloaded, and the difference is worth saying in words.

/** A sentence for a person, or null when Widevine is available. */
export async function widevineProblem(): Promise<string | null> {
  if (typeof navigator.requestMediaKeySystemAccess !== "function") {
    return "This window cannot play protected music: it has no DRM support.";
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
    return "This build of Goose In A Pond has no Widevine module, so it cannot play Apple Music. If the module is still downloading, try again in a minute.";
  }
}
