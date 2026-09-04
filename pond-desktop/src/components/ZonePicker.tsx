// ─── One time-zone picker, everywhere ────────────────────────────────────────
//
// There were three <select>s over three different hand-maintained lists — 16
// zones in Settings, 18 in Schedules, 13 in the wizard — so the same household
// was offered a different world depending on which screen it was standing in
// front of, and a zone picked in one could be unofferable in another.
//
// This renders the server's IANA catalogue (597 zones), with the offset beside
// each name because that is how somebody confirms they picked the right one of
// two similar names.

import { useEffect, useState } from "react";
import { allZones, deviceZone } from "../lib/place";
import type { ZoneChoice } from "../api/types";

interface Props {
  value: string;
  onChange: (zone: string) => void;
  className?: string;
  "aria-label"?: string;
  id?: string;
}

export function ZonePicker({ value, onChange, className, id, ...rest }: Props) {
  const [zones, setZones] = useState<ZoneChoice[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    // `allZones` falls back to this webview's own catalogue and never rejects,
    // so there is no error branch to render here.
    allZones().then((z) => {
      if (!cancelled) setZones(z);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const current = value || deviceZone();
  const list = zones ?? [];
  // A stored zone the catalogue does not list is kept and shown rather than
  // silently replaced by whatever happens to be first — opening a picker must
  // never change the value behind it.
  const missing = current && !list.some((z) => z.zone === current);

  return (
    <select
      id={id}
      className={className}
      value={current}
      onChange={(e) => onChange(e.target.value)}
      aria-label={rest["aria-label"]}
    >
      {missing && <option value={current}>{current}</option>}
      {list.map((z) => (
        <option key={z.zone} value={z.zone}>
          {z.place ? `${z.zone} — ${z.place} (${z.offset})` : `${z.zone} (${z.offset})`}
        </option>
      ))}
    </select>
  );
}
