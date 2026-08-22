import { useState, useEffect } from "react";
import { Button, Separator, Switch } from "@heroui/react";
import {
  Monitor, Cpu, Activity, Power, Settings, Plus, X, Radio, Smartphone,
  Lightbulb, Lock, Thermometer, Fan, Blinds, RefreshCw,
  PlugZap, WashingMachine, Waves, Wind, MonitorPlay, Bot, BellRing,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError, type Device, type MatterStatus } from "../api/types";
import { refreshHomeData } from "../hub/state/hubDataStore";

/** The server's own message, without the `ApiError:` prefix `String(e)` adds.
 *  Users were being shown the exception class name alongside the advice. */
function errorText(e: unknown): string {
  return e instanceof ApiError ? e.message : String(e);
}

/** How the Matter section reads in each state. Kept as data so the chip, the
 *  hint, and the commission gate cannot drift apart. */
const MATTER_STATE_LABEL: Record<MatterStatus["state"], string> = {
  disabled:    "Off",
  connecting:  "Starting…",
  connected:   "Connected",
  unreachable: "Cannot reach controller",
};

/** How often to re-check while the controller is starting up. */
const MATTER_POLL_MS = 2000;


function DeviceIcon({ kind }: { kind: string | undefined }) {
  if (kind === "host")   return <Cpu size={22} />;
  if (kind === "gotg")   return <Smartphone size={22} />; // the mobile companion
  if (kind === "sensor") return <Activity size={22} />;
  // Matter device types (inferred from clusters by the backend), so a
  // commissioned bulb / lock / thermostat / fan / blind reads as what it is.
  if (kind === "light")      return <Lightbulb size={22} />;
  if (kind === "lock")       return <Lock size={22} />;
  if (kind === "thermostat") return <Thermometer size={22} />;
  if (kind === "fan")        return <Fan size={22} />;
  if (kind === "covering")   return <Blinds size={22} />;
  // Types the node states for itself via its Matter Descriptor. Without these a
  // dishwasher arrives wearing a lightbulb, because On/Off is all a cluster can
  // say about it.
  if (kind === "plug")       return <PlugZap size={22} />;
  if (kind === "appliance")  return <WashingMachine size={22} />;
  if (kind === "pump")       return <Waves size={22} />;
  if (kind === "air")        return <Wind size={22} />;
  if (kind === "media")      return <MonitorPlay size={22} />;
  if (kind === "vacuum")     return <Bot size={22} />;
  if (kind === "alarm")      return <BellRing size={22} />;
  return <Monitor size={22} />;
}

function iconClass(kind: string | undefined, isOnline: boolean): string {
  if (!isOnline) return "device-card__icon";
  if (kind === "host")   return "device-card__icon device-card__icon--host";
  // An alarm reads as a sensor: it is a thing that reports, not one you drive.
  if (kind === "sensor" || kind === "alarm") {
    return "device-card__icon device-card__icon--sensor";
  }
  return "device-card__icon device-card__icon--edge";
}

function timeSince(iso: string | null | undefined): string {
  if (!iso) return "—";
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/**
 * When a device was last heard from.
 *
 * A device that is online is being vouched for right now — "online" is derived
 * from having been heard from recently — so it reads "now" rather than counting
 * the minutes since the last heartbeat, which would tick 1, 2, 3 for a device
 * that never went anywhere. An offline one shows when it actually went quiet,
 * which is the number worth having then.
 */
function lastSeen(device: Pick<Device, "is_online" | "last_seen">): string {
  return device.is_online ? "now" : timeSince(device.last_seen);
}

export function Devices() {
  const [devices, setDevices]     = useState<Device[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [showForm, setShowForm]   = useState(false);

  // Form state
  const [name, setName]               = useState("");
  const [setupCode, setSetupCode]     = useState("");
  const [submitting, setSubmitting]   = useState(false);
  const [formError, setFormError]     = useState<string | null>(null);

  // Matter section state
  const [matter, setMatter]           = useState<MatterStatus | null>(null);
  const [matterBusy, setMatterBusy]   = useState(false);
  const [matterError, setMatterError] = useState<string | null>(null);

  // Per-card action state
  const [busyId, setBusyId]           = useState<string | null>(null);
  const [detail, setDetail]           = useState<Device | null>(null);

  // Configure-modal edit state
  const [editName, setEditName]         = useState("");
  const [editHostname, setEditHostname] = useState("");
  const [editRoom, setEditRoom]         = useState("");
  const [editSubmitting, setEditSubmitting] = useState(false);
  const [editError, setEditError]       = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listDevices()
      .then(setDevices)
      .catch((e) => setError(errorText(e)))
      .finally(() => setLoading(false));
  }

  /** Read the Matter runtime's actual state — what it is doing, not what was
   *  saved. "Starting" and "unreachable" need different words from the user. */
  function loadMatter() {
    return api.getMatterStatus()
      .then((s) => {
        setMatter(s);
        return s;
      })
      .catch((e) => { setMatterError(errorText(e)); return null; });
  }

  useEffect(() => { load(); void loadMatter(); }, []);

  // Enabling installs and starts a controller, which the settings save does not
  // wait for — so the panel watches it come up rather than claiming it is done.
  useEffect(() => {
    if (matter?.state !== "connecting") return;
    const timer = setInterval(() => { void loadMatter(); }, MATTER_POLL_MS);
    return () => clearInterval(timer);
  }, [matter?.state]);

  /** Ask the runtime to try again.
   *
   *  Re-sending the current settings is what reconnects a failed controller —
   *  the reconciler treats an unchanged request while unreachable as a retry,
   *  which is precisely what this button means. */
  async function retryMatter() {
    setMatterBusy(true);
    setMatterError(null);
    try {
      await api.updateSettings({ matter_ws_url: matter?.url ?? "" });
      // Optimistic, so the chip moves the moment the button is pressed; the
      // poll above replaces this with whatever actually happened.
      setMatter((m) => (m ? { ...m, state: "connecting", error: undefined } : m));
      await loadMatter();
    } catch (e) {
      setMatterError(errorText(e));
      void loadMatter();
    } finally {
      setMatterBusy(false);
    }
  }

  function openForm() {
    setName(""); setSetupCode("");
    setFormError(null);
    setShowForm(true);
  }

  function closeForm() { setShowForm(false); setFormError(null); }

  /// Commission a Matter device: GIAP pairs it onto the fabric, then the bridge
  /// registers it from the controller's own report — so we just reload the list.
  async function handleCommission() {
    if (!setupCode.trim()) { setFormError("Enter the device's setup code."); return; }
    setSubmitting(true);
    setFormError(null);
    try {
      await api.commissionDevice(setupCode.trim(), name.trim() || undefined);
      closeForm();
      load();
    } catch (e) {
      setFormError(errorText(e));
    } finally {
      setSubmitting(false);
    }
  }

  // Toggle registry connectivity directly (heartbeat / offline), not the
  // giap-device-control MCP tool — that tool actuates a smart device's own
  // power state (a light/plug), a different concept from whether the device
  // itself is reachable. There is no "wake"/"restart" primitive in the
  // backend, so this is an honest on/off toggle: turn on when offline, off
  // when online.
  async function handlePower(d: Device) {
    setBusyId(d.id);
    try {
      if (d.is_online) {
        await api.markDeviceOffline(d.id);
      } else {
        await api.markDeviceOnline(d.id);
      }
      load();
      void refreshHomeData();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusyId(null);
    }
  }

  function openDetail(d: Device) {
    setDetail(d);
    setEditName(d.name);
    setEditHostname(d.hostname ?? "");
    setEditRoom(d.room ?? "");
    setEditError(null);
  }

  async function handleUpdateDevice() {
    if (!detail) return;
    if (!editName.trim()) { setEditError("Name is required."); return; }
    setEditSubmitting(true);
    setEditError(null);
    try {
      const updated = await api.updateDevice(detail.id, {
        name: editName.trim(),
        hostname: editHostname.trim() || undefined,
        room: editRoom.trim() || undefined,
      });
      setDetail(updated);
      load();
      void refreshHomeData();
    } catch (e) {
      setEditError(errorText(e));
    } finally {
      setEditSubmitting(false);
    }
  }

  async function handleUnregister(d: Device) {
    setBusyId(d.id);
    try {
      await api.unregisterDevice(d.id);
      setDetail(null);
      load();
      void refreshHomeData();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div className="screen screen--devices">
      {/* ── Page header ────────────────────────────────────── */}
      <div className="page-header">
        <div>
          <h1 className="page-header__title">Devices</h1>
          <p className="dev-header__sub">All registered nodes on your local network.</p>
        </div>
        <div className="page-header__action">
          <Button size="sm" variant="primary" className="page-header-btn" onPress={openForm}>
            <Plus size={14} /> Register device
          </Button>
        </div>
      </div>

      {/* ── Matter ────────────────────────────────────────────
          Status only. Matter runs by default and installs its own controller,
          so there is nothing here to switch on — the panel exists to say
          whether pairing will work right now, and to offer the one action worth
          offering when it will not. */}
      <section className="matter-panel">
        <div className="matter-panel__head">
          <Radio size={15} />
          <h2 className="matter-panel__title">Matter</h2>
          <span
            className={`matter-panel__chip matter-panel__chip--${matter?.state ?? "connecting"}`}
            data-testid="matter-state"
          >
            {MATTER_STATE_LABEL[matter?.state ?? "connecting"]}
          </span>
          <span className="matter-panel__spacer" />
          {matter?.state === "unreachable" && (
            <Button size="sm" variant="outline" isDisabled={matterBusy} onPress={() => void retryMatter()}>
              <RefreshCw size={13} /> Retry
            </Button>
          )}
        </div>

        <p className="matter-panel__hint">
          Lights, locks, and sensors paired onto your local fabric. The first
          device you add starts a Matter controller here, which can take a couple
          of minutes; after that it is always ready.
        </p>

        {matter?.state === "unreachable" && matter.error && (
          <p className="text-error text-error--sm">{matter.error}</p>
        )}
        {matterError && <p className="text-error text-error--sm">{matterError}</p>}
      </section>

      {loading && <p className="muted-12">Loading devices…</p>}
      {error   && <p className="muted-12 text-error">{error}</p>}

      {!loading && !error && devices.length === 0 && (
        <div className="empty-state">
          <Monitor size={32} />
          <span>No devices registered yet.</span>
          <button className="empty-state__cta" onClick={openForm}>
            <Plus size={14} /> Register device
          </button>
        </div>
      )}

      {devices.length > 0 && (
        <div className="devices-grid">
          {devices.map((d) => (
            <div
              key={d.id}
              className={`device-card${!d.is_online ? " device-card--offline" : ""}`}
            >
              {/* Top: icon + name + IP */}
              <div className="device-card__top">
                <span className={iconClass(d.device_type, d.is_online)}>
                  <DeviceIcon kind={d.device_type} />
                </span>
                <div className="device-card__info">
                  <div className="device-card__name">{d.name}</div>
                  <code className="device-card__ip">
                    {d.metadata?.ip != null ? String(d.metadata.ip) : "—"}
                  </code>
                </div>
              </div>

              {/* Chips: status + type + last seen */}
              <div className="device-card__chips">
                <span className={`device-card__chip device-card__chip--${d.is_online ? "online" : "offline"}`}>
                  <span className="device-card__dot" />
                  {d.is_online ? "online" : "offline"}
                </span>
                {d.device_type && (
                  <span className="device-card__chip">{d.device_type}</span>
                )}
                <span className="device-card__chip">{lastSeen(d)}</span>
              </div>

              {/* Actions */}
              <div className="device-card__actions">
                <button
                  className="device-card__action-btn"
                  onClick={() => handlePower(d)}
                  disabled={busyId === d.id}
                  type="button"
                >
                  <Power size={12} /> {d.is_online ? "Turn off" : "Turn on"}
                </button>
                <button
                  className="device-card__action-btn"
                  onClick={() => openDetail(d)}
                  type="button"
                >
                  <Settings size={12} /> Configure
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ── Register device modal ─────────────────────────── */}
      {showForm && (
        <div className="sched-modal__overlay" onClick={closeForm}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">Register device</h2>
              <button className="sched-modal__close" onClick={closeForm} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />

            <div className="sched-modal__body">
              {/* Only Matter devices are registered here. Phones pair with a
                  pairing code and the desktop app is this app, so a chooser
                  would have had one real option in it. */}
              <div className="sched-modal__field">
                <label className="sched-modal__label">Setup code</label>
                <input
                  className="sched-modal__input"
                  placeholder="20202021 or MT:-24J0AFN00KA0648G00"
                  value={setupCode}
                  onChange={(e) => setSetupCode(e.target.value)}
                  autoFocus
                />
                <p className="sched-modal__cron-hint">
                  The 11-digit pairing code or QR payload on the device, or its
                  8-digit passcode. GIAP commissions it onto your fabric.
                </p>
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Name <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room Light"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
                <p className="sched-modal__cron-hint">
                  Written to the device so GIAP and other apps use it — say
                  "turn on the living room light". Left blank, the device's own
                  name is used.
                </p>
              </div>

              {/* Commissioning needs a live controller. Say so here rather than
                  letting the user fill the form in and fail on submit. */}
              {matter?.state !== "connected" && (
                <p className="sched-modal__cron-hint" data-testid="matter-not-ready">
                  {matter?.state === "unreachable"
                    ? "The Matter controller cannot be reached. See the Matter section of this tab."
                    : "Matter is still starting up. This will be ready in a moment."}
                </p>
              )}

              {formError && (
                <p className="text-error text-error--sm">{formError}</p>
              )}
            </div>

            <Separator />

            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeForm}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={submitting || !setupCode.trim() || matter?.state !== "connected"}
                onPress={handleCommission}
              >
                {/* Commissioning is slow — say so rather than looking hung. */}
                {submitting ? "Commissioning…" : "Commission"}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── Device detail / configure modal ─────────────────── */}
      {detail && (
        <div className="sched-modal__overlay" onClick={() => setDetail(null)}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">{editName.trim() || detail.name}</h2>
              <button className="sched-modal__close" onClick={() => setDetail(null)} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />
            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Name</label>
                <input
                  className="sched-modal__input"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Hostname <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="raspberrypi.local"
                  value={editHostname}
                  onChange={(e) => setEditHostname(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Room <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room"
                  value={editRoom}
                  onChange={(e) => setEditRoom(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Type</label>
                <div className="muted-12">{detail.device_type ?? "—"}</div>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Status</label>
                <div className="muted-12">
                  {detail.is_online ? "online" : "offline"} · last seen {lastSeen(detail)}
                </div>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Address</label>
                <code className="device-card__ip">
                  {detail.metadata?.ip != null ? String(detail.metadata.ip) : "—"}
                </code>
              </div>
              {editError && (
                <p className="text-error text-error--sm">{editError}</p>
              )}
            </div>
            <Separator />
            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={() => setDetail(null)}>Close</Button>
              <Button
                size="sm"
                variant="danger"
                isDisabled={busyId === detail.id}
                onPress={() => handleUnregister(detail)}
              >
                {busyId === detail.id ? "Removing…" : "Unregister device"}
              </Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={editSubmitting || !editName.trim()}
                onPress={handleUpdateDevice}
              >
                {editSubmitting ? "Saving…" : "Save"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
