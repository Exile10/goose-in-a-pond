import { Fragment, useCallback, useEffect, useMemo, useState } from "react";
import { Switch, Button } from "@heroui/react";
import { Search, Crosshair, AlertCircle } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ModelEntry, Settings } from "../api/types";
import { diffSettings, foldServerState } from "../sections/Settings";
import { ErrorBanner, SkeletonList } from "../components/shared";
import {
  CATALOGUE, TIER_NOTE, allEntries, inertCount,
  type CatalogueCategory, type Consumer, type Entry, type OptionSource, type Subcategory,
} from "./catalogue";
import { detectTimezone } from "./validation";
import "../styles/settings-catalogue.css";

// ─── Marks ────────────────────────────────────────────────────────────────
// The signature of this page: every control says whether anything reads it.

const MARK_TITLE: Record<Consumer, string> = {
  live: "Connected — the pond reads this and acts on it",
  app: "App only — this app honours it, the pond never sees it",
  none: "Not connected — nothing reads this yet",
};

function Mark({ consumer }: { consumer: Consumer }) {
  return <span className={`scat__dot scat__dot--${consumer}`} title={MARK_TITLE[consumer]} />;
}

// ─── Options from the model registry ──────────────────────────────────────

/**
 * Provider filters copied from `sections/Models.tsx`, which is this app's
 * existing authority on which `provider` value belongs to which role. Kept as
 * one table so the two cannot drift apart silently.
 */
const PROVIDERS: Record<Exclude<OptionSource, "llm-providers">, (m: ModelEntry) => boolean> = {
  "llm-models": (m) => ["gguf", "llamafile", "ollama"].includes(m.provider),
  "whisper-models": (m) => m.provider === "whisper",
  "tts-voices": (m) => ["tts", "tts_piper", "tts_http"].includes(m.provider),
  "embedding-models": (m) => m.provider === "embedding" || m.category === "embedding",
};

interface Option { value: string; label: string }

function optionsFor(source: OptionSource, models: ModelEntry[]): Option[] {
  if (source === "llm-providers") {
    const seen = [...new Set(models.filter(PROVIDERS["llm-models"]).map((m) => m.provider))];
    return seen.sort().map((p) => ({ value: p, label: p }));
  }
  return models
    .filter(PROVIDERS[source])
    // A model the device has not downloaded cannot be selected into service,
    // so offering it would be offering a failure. `downloaded` is optional in
    // the registry, and absent means "not tracked" rather than "missing".
    .filter((m) => m.downloaded !== false)
    .map((m) => ({
      value: source === "tts-voices" ? (m.filename ?? m.name) : m.name,
      label: m.display_name ?? m.name,
    }))
    .sort((a, b) => a.label.localeCompare(b.label));
}

// ─── Value helpers ────────────────────────────────────────────────────────

/**
 * Render a stored value into a text box.
 *
 * `retention_events_by_category` is the one map-valued setting, shown as
 * `network 14, sensor 7` rather than raw JSON — nobody editing retention should
 * have to type braces. `parseText` is its inverse.
 */
function textValue(v: unknown): string {
  if (v == null) return "";
  if (Array.isArray(v)) return v.join(", ");
  if (typeof v === "object") {
    return Object.entries(v as Record<string, number>).map(([k, n]) => `${k} ${n}`).join(", ");
  }
  return String(v);
}

const NULLABLE_TEXT = new Set(["custom_system_prompt", "searxng_url", "voice_kws_whisper_url"]);

/** Inverse of `textValue`. Returns the shape the server expects for this key. */
function parseText(key: string, raw: string): unknown {
  if (key === "voice_wake_word_transcriptions") {
    return raw.split(",").map((s) => s.trim()).filter(Boolean);
  }
  if (key === "retention_events_by_category") {
    const out: Record<string, number> = {};
    for (const part of raw.split(",")) {
      const [name, days] = part.trim().split(/\s+/);
      const n = Number(days);
      if (name && Number.isFinite(n)) out[name] = n;
    }
    return out;
  }
  // An emptied box means "unset", not the empty string — both of these are
  // `Option<String>` server-side.
  if (raw === "" && NULLABLE_TEXT.has(key)) return null;
  return raw;
}

// ─── The sentence ─────────────────────────────────────────────────────────

interface Clause { text: string; category: string }

/** What the pond may currently do, as one sentence, before you touch anything. */
function postureClauses(s: Partial<Settings>): { clauses: Clause[]; reach: Clause | null; tail: string } {
  const clauses: Clause[] = [
    { text: (s.mic_enabled ?? true) ? "listens" : "hears nothing", category: "privacy" },
    { text: (s.vision_enabled ?? false) ? "watches" : "watches nothing", category: "vision" },
    {
      text: (s.memory_extraction_enabled ?? true) ? "remembers what you tell it" : "forgets everything",
      category: "memory",
    },
    {
      text: (s.unprompted_speech_enabled ?? false) ? "speaks on its own" : "speaks only when spoken to",
      category: "automation",
    },
  ];
  const mode = s.network_mode ?? "open";
  if (mode === "offline") return { clauses, reach: null, tail: "Nothing leaves this house." };
  if (mode === "allowlist") return { clauses, reach: null, tail: "It reaches only the hosts you allow." };
  return { clauses, reach: { text: "any host on the internet", category: "privacy" }, tail: "" };
}

// ─── Row ──────────────────────────────────────────────────────────────────

function EntryRow({
  entry, value, error, options, onChange, extra,
}: {
  entry: Entry;
  value: unknown;
  error: string | null;
  options: Option[] | null;
  onChange: (key: keyof Settings, v: unknown) => void;
  extra?: React.ReactNode;
}) {
  // A control nothing reads is not offered. Leaving it operable would let
  // someone spend a decision on a value that changes nothing — the exact
  // defect this page exists to surface.
  const inert = entry.consumer === "none";
  const { control } = entry;
  const errId = error ? `scat-err-${entry.key}` : undefined;
  const described = [errId].filter(Boolean).join(" ") || undefined;

  return (
    <div className="scat__row" data-invalid={error ? "true" : undefined}>
      <div className="scat__rowMain">
        <div className="scat__rowLabel">
          <Mark consumer={entry.consumer} />
          <span>{entry.label}</span>
          {entry.proposed && <span className="scat__new">New</span>}
        </div>
        <div className="scat__key">{entry.key}</div>

        {entry.note && (
          <p className={`scat__note scat__note--${entry.consumer}`}>
            <Mark consumer={entry.consumer} />
            <span>{entry.note}</span>
          </p>
        )}

        {control.kind === "radio" && (
          <div className="scat__radios" role="radiogroup" aria-label={entry.label}>
            {control.options.map((o) => (
              <label className="scat__radio" key={o.value}>
                <input
                  type="radio"
                  name={`scat-${entry.key}`}
                  value={o.value}
                  disabled={inert}
                  checked={String(value ?? "") === o.value}
                  onChange={() => onChange(entry.key, o.value)}
                />
                <span className="scat__radioBody">
                  <span className="scat__radioLabel">{o.label}</span>
                  {o.hint && <span className="scat__radioHint">{o.hint}</span>}
                </span>
              </label>
            ))}
          </div>
        )}

        {error && (
          <p className="scat__error" id={errId} role="alert">
            <AlertCircle size={13} aria-hidden="true" />
            <span>{error}</span>
          </p>
        )}
      </div>

      {control.kind !== "radio" && (
        <div className="scat__ctl">
          {control.kind === "toggle" && (
            <span className={inert ? "scat__swWrap scat__swWrap--inert" : "scat__swWrap"}>
              <Switch
                aria-label={entry.label}
                isSelected={Boolean(value)}
                isDisabled={inert}
                onChange={(v: boolean) => onChange(entry.key, v)}
              >
                <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
              </Switch>
            </span>
          )}

          {control.kind === "select" && (
            <select
              className="native-select scat__field"
              aria-label={entry.label}
              aria-describedby={described}
              disabled={inert}
              value={String(value ?? control.options[0])}
              onChange={(e) => onChange(entry.key, e.target.value)}
            >
              {control.options.map((o) => <option key={o} value={o}>{o}</option>)}
            </select>
          )}

          {control.kind === "lookup" && (
            options === null ? (
              // The registry has not answered yet, or could not be reached.
              // Free text rather than an empty picker: an empty dropdown offers
              // nothing and hides the value that is already set.
              <input
                type="text"
                className="native-input scat__field scat__field--text"
                aria-label={entry.label}
                aria-describedby={described}
                disabled={inert}
                placeholder={control.placeholder}
                value={textValue(value)}
                onChange={(e) => onChange(entry.key, e.target.value)}
              />
            ) : (
              <select
                className="native-select scat__field scat__field--text"
                aria-label={entry.label}
                aria-describedby={described}
                disabled={inert}
                value={String(value ?? "")}
                onChange={(e) => onChange(entry.key, e.target.value)}
              >
                <option value="">{control.placeholder ?? "Not set"}</option>
                {/* A stored value the registry does not list is kept and shown,
                    so opening this page can never silently drop a model that is
                    configured but not currently installed. */}
                {Boolean(value) && !options.some((o) => o.value === String(value)) && (
                  <option value={String(value)}>{String(value)} — not installed</option>
                )}
                {options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
              </select>
            )
          )}

          {control.kind === "number" && (
            <span className="scat__num">
              <input
                type="number"
                className="native-input scat__field scat__field--num"
                aria-label={entry.label}
                aria-describedby={described}
                aria-invalid={error ? true : undefined}
                disabled={inert}
                step={control.step}
                min={control.min}
                max={control.max}
                value={value == null ? "" : String(value)}
                // Emptying a number box means "unset", which is not 0 — sending
                // 0 for a blank latitude would move the home to the Gulf of
                // Guinea without anyone asking for it.
                onChange={(e) => onChange(entry.key, e.target.value === "" ? null : Number(e.target.value))}
              />
              {control.unit && <span className="scat__unit">{control.unit}</span>}
            </span>
          )}

          {control.kind === "text" && (
            <input
              type="text"
              className="native-input scat__field scat__field--text"
              aria-label={entry.label}
              aria-describedby={described}
              aria-invalid={error ? true : undefined}
              disabled={inert}
              placeholder={control.placeholder}
              value={textValue(value)}
              onChange={(e) => onChange(entry.key, parseText(entry.key, e.target.value))}
            />
          )}

          {extra}
        </div>
      )}
    </div>
  );
}

// ─── Page ─────────────────────────────────────────────────────────────────

export function SettingsCatalogueView({ onBack }: { onBack?: () => void } = {}) {
  const [settings, setSettings] = useState<Partial<Settings>>({});
  const [baseline, setBaseline] = useState<Partial<Settings>>({});
  const [models, setModels] = useState<ModelEntry[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [categoryId, setCategoryId] = useState(CATALOGUE[0].id);
  const [query, setQuery] = useState("");

  const load = useCallback(() => {
    setLoading(true);
    setLoadError(null);
    api.getSettings()
      .then((s) => { setSettings(s); setBaseline(structuredClone(s)); })
      .catch((e) => setLoadError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(load, [load]);

  useEffect(() => {
    // The registry only fills pickers. A failure here degrades every lookup to
    // a text box rather than blocking the page, so it is not a load error.
    let cancelled = false;
    api.listModels()
      .then((m) => !cancelled && setModels(m))
      .catch(() => !cancelled && setModels(null));
    return () => { cancelled = true; };
  }, []);

  /** Every field whose value fails its own validator. Blocks Save. */
  const errors = useMemo(() => {
    const out: Record<string, string> = {};
    for (const e of allEntries()) {
      if (e.consumer === "none") continue;
      const raw = (settings as Record<string, unknown>)[e.key];

      // ABSENT IS NOT INVALID. A key the server did not send is not something a
      // person can see or fix, and flagging it would block every save on a
      // partial response — an older server, or a field added since. Only a
      // value that is actually present gets judged.
      if (raw === undefined) continue;

      // An emptied number box is a different matter: no numeric setting is
      // `Option<_>` server-side, so sending null earns a 422. Catching it here
      // names the box instead of failing the whole save.
      if (raw === null && e.control.kind === "number") {
        out[e.key] = "Enter a number.";
        continue;
      }

      if (!e.validate) continue;
      const msg = e.validate(raw);
      if (msg) out[e.key] = msg;
    }
    return out;
  }, [settings]);

  const errorCount = Object.keys(errors).length;
  const dirty = useMemo(
    () => Object.keys(diffSettings(baseline, settings)).length,
    [baseline, settings],
  );

  const patch = useCallback((key: keyof Settings, value: unknown) => {
    setSaved(false);
    setSaveError(null);
    setSettings((prev) => ({ ...prev, [key]: value }));
  }, []);

  async function save() {
    const body = diffSettings(baseline, settings);
    if (!Object.keys(body).length || saving || errorCount) return;
    setSaving(true);
    setSaveError(null);
    try {
      const updated = await api.updateSettings(body);
      // Fold against the patch we SENT, not the pre-save baseline — the rule
      // the classic Settings panel follows. The server can answer with a value
      // it derived rather than the one we sent (geocoding rewrites the
      // coordinates), and that echo has to be adopted or every later save
      // re-sends a stale value forever.
      setSettings((prev) => foldServerState(prev, { ...baseline, ...body }, updated));
      setBaseline(structuredClone(updated));
      setSaved(true);
    } catch (e) {
      // Shown verbatim, not through `friendlyMessage`: a 422 from this endpoint
      // names the field and the accepted values, and that sentence is the whole
      // reason the request failed.
      setSaveError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  }

  const searching = query.trim().length > 0;
  const category = CATALOGUE.find((c) => c.id === categoryId) ?? CATALOGUE[0];

  const groups: Subcategory[] = useMemo(() => {
    if (!searching) return category.groups;
    const q = query.trim().toLowerCase();
    return CATALOGUE
      .flatMap((c) => c.groups)
      .map((g) => ({
        ...g,
        entries: g.entries.filter((e) =>
          `${e.label} ${e.key} ${e.note ?? ""}`.toLowerCase().includes(q)),
      }))
      .filter((g) => g.entries.length > 0);
  }, [searching, query, category]);

  const resultCount = groups.reduce((n, g) => n + g.entries.length, 0);

  const totals = useMemo(() => {
    const all = allEntries();
    return {
      total: all.length,
      live: all.filter((e) => e.consumer === "live").length,
      app: all.filter((e) => e.consumer === "app").length,
      none: all.filter((e) => e.consumer === "none").length,
      proposed: all.filter((e) => e.proposed).length,
    };
  }, []);

  const goTo = useCallback((id: string) => { setCategoryId(id); setQuery(""); }, []);

  const { clauses, reach, tail } = postureClauses(settings);
  const systemZone = detectTimezone();
  const zoneDiffers = systemZone != null && settings.timezone !== systemZone;

  /** The one control with an action beside it: fill the zone from the device. */
  function extraFor(entry: Entry): React.ReactNode {
    if (entry.key !== "timezone" || !zoneDiffers) return undefined;
    return (
      <button type="button" className="scat__detect" onClick={() => patch("timezone", systemZone)}>
        <Crosshair size={12} aria-hidden="true" />
        Use {systemZone}
      </button>
    );
  }

  let lastTier: string | null = null;

  const saveLabel = saving
    ? "Saving…"
    : errorCount
      ? `Fix ${errorCount} field${errorCount === 1 ? "" : "s"}`
      : dirty
        ? `Save ${dirty} change${dirty === 1 ? "" : "s"}`
        : saved ? "Saved" : "No changes";

  return (
    <div className="scat">
      <header className="scat__head">
        <div>
          {onBack && (
            <button type="button" className="hub-back-btn scat__back" onClick={onBack}>
              ← Back
            </button>
          )}
          <h1 className="scat__title">Settings</h1>
          <p className="scat__sub">Everything this pond is, knows, hears, and is allowed to do.</p>
        </div>
        <div className="scat__headActions">
          <div className="scat__search">
            <Search size={14} aria-hidden="true" />
            <input
              type="search"
              className="scat__searchInput"
              aria-label="Search settings"
              placeholder={`Search ${totals.total} settings`}
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>
          <Button variant="primary" isDisabled={!dirty || saving || errorCount > 0} onPress={save}>
            {saveLabel}
          </Button>
        </div>
      </header>

      {loadError && <ErrorBanner error={loadError} onRetry={load} />}

      {saveError && (
        <p className="scat__saveError" role="alert">
          <AlertCircle size={14} aria-hidden="true" />
          <span>{saveError}</span>
        </p>
      )}

      {loading ? (
        <SkeletonList rows={8} />
      ) : (
        <>
          <section className="scat__posture" aria-labelledby="scat-now">
            <div className="scat__eyebrow" id="scat-now">Right now</div>
            <div className="scat__postureBody">
              <p className="scat__sentence">
                Your pond{" "}
                {/* Anchors, not buttons. The engine normalises `display: inline`
                    on a <button> to inline-block, which makes each clause an
                    atomic box — the comma after it then becomes its own
                    line-break opportunity and orphans onto the next line. An
                    <a> is inline natively, so punctuation stays with its
                    clause. These are in-page navigation, so a link is also the
                    honest element, and it is keyboard-reachable for free. */}
                {clauses.map((c, i) => (
                  <Fragment key={c.category + c.text}>
                    <a
                      href={`#${c.category}`}
                      className="scat__clause"
                      onClick={(e) => { e.preventDefault(); goTo(c.category); }}
                    >
                      {c.text}
                    </a>
                    {i < clauses.length - 2 ? ", " : i === clauses.length - 2 ? ", and " : ". "}
                  </Fragment>
                ))}
                {reach ? (
                  <>
                    It can reach{" "}
                    <a
                      href={`#${reach.category}`}
                      className="scat__clause"
                      onClick={(e) => { e.preventDefault(); goTo(reach.category); }}
                    >
                      {reach.text}
                    </a>.
                  </>
                ) : tail}
              </p>
              <div className="scat__tally">
                <span className="scat__tallyItem">
                  <span className="scat__dot scat__dot--live" /><b>{totals.live}</b><span>connected</span>
                </span>
                <span className="scat__tallyItem">
                  <span className="scat__dot scat__dot--app" /><b>{totals.app}</b><span>app only</span>
                </span>
                <span className="scat__tallyItem">
                  <span className="scat__dot scat__dot--none" /><b>{totals.none}</b><span>not connected</span>
                </span>
                <span className="scat__tallyItem">
                  <span /><b>{totals.proposed}</b><span>new control</span>
                </span>
              </div>
            </div>
          </section>

          <div className="scat__grid">
            <nav className="scat__rail" aria-label="Settings categories">
              {CATALOGUE.map((c: CatalogueCategory) => {
                const newTier = c.tier !== lastTier;
                if (newTier) lastTier = c.tier;
                const inert = inertCount(c);
                const bad = c.groups
                  .flatMap((g) => g.entries)
                  .filter((e) => errors[e.key]).length;
                return (
                  <div key={c.id} className="scat__railItem">
                    {newTier && (
                      <>
                        <div className="scat__tier">{c.tier}</div>
                        <p className="scat__tierNote">{TIER_NOTE[c.tier]}</p>
                      </>
                    )}
                    <button
                      type="button"
                      className="scat__navBtn"
                      aria-current={!searching && categoryId === c.id}
                      onClick={() => goTo(c.id)}
                    >
                      <span>{c.name}</span>
                      <span className="scat__navFlags">
                        {bad > 0 && (
                          <span className="scat__flag scat__flag--bad" title={`${bad} field${bad === 1 ? "" : "s"} to fix here`}>
                            {bad}
                          </span>
                        )}
                        {inert > 0 && (
                          <span className="scat__flag" title={`${inert} setting${inert === 1 ? "" : "s"} here that nothing reads`}>
                            {inert}
                          </span>
                        )}
                      </span>
                    </button>
                  </div>
                );
              })}
            </nav>

            <div className="scat__panel">
              <div className="scat__panelHead">
                <h2 className="scat__panelTitle">
                  {searching ? `${resultCount} result${resultCount === 1 ? "" : "s"}` : category.name}
                </h2>
                <p className="scat__panelSub">
                  {searching
                    ? `Matching “${query.trim()}” across all ${totals.total} settings.`
                    : category.blurb}
                </p>
              </div>

              {groups.length === 0 ? (
                <div className="scat__group scat__empty">
                  <b>Nothing matches “{query.trim()}”</b>
                  <span>Try a setting name, or part of a key like voice_.</span>
                </div>
              ) : (
                groups.map((g) => (
                  <section className="scat__group" key={`${category.id}-${g.name}`}>
                    <div className="scat__groupHead">
                      <h3>{g.name}</h3>
                      {g.name === "Where and when" && !searching && (
                        <p className="scat__groupHint">
                          Save a location name and the pond looks up its coordinates for you.
                          Typing a coordinate yourself keeps the one you typed.
                        </p>
                      )}
                    </div>
                    {g.entries.map((e) => (
                      <EntryRow
                        key={e.key}
                        entry={e}
                        value={(settings as Record<string, unknown>)[e.key]}
                        error={errors[e.key] ?? null}
                        options={e.control.kind === "lookup" && models ? optionsFor(e.control.source, models) : null}
                        onChange={patch}
                        extra={extraFor(e)}
                      />
                    ))}
                  </section>
                ))
              )}
            </div>
          </div>

          <div className="scat__legend">
            <div className="scat__legendTitle">What the mark beside each setting means</div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--live" />
              <span><b>Connected</b>The pond reads this and acts on it.</span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--app" />
              <span><b>App only</b>This app honours it. The pond never sees it.</span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__dot scat__dot--none" />
              <span><b>Not connected</b>Nothing reads this yet, so the control is not offered.</span>
            </div>
            <div className="scat__legendItem">
              <span className="scat__new">New</span>
              <span><b>New control</b>Reachable only through the API before now.</span>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
