import { useCallback, useEffect, useId, useRef, useState, type FormEvent } from "react";
import {
  Eye,
  EyeOff,
  Info,
  Loader2,
  Lock,
  LockOpen,
  Plus,
  Search,
  Trash2,
  Wifi,
  WifiOff,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import type { WifiJoinRequest, WifiLink, WifiNetwork } from "../api/types";
import { LinkHealthNotice, Message, SignalBars, type Variant } from "./LinkPanels";
import { useConnections } from "./useConnections";
import {
  ENTERPRISE_NOTE,
  capabilityNotices,
  connectionErrorCode,
  connectionErrorText,
  endSentence,
  isConnectionLost,
  joinLeavesNetwork,
  joinMoved,
  passphraseProblem,
  reachLine,
  securityLabel,
  ssidProblem,
  unavailableReason,
} from "./connectionsView";
import "./network.css";

/** A join answers within 45 s; this is what the household is told to expect. */
const JOIN_WAIT_SECONDS = 45;

interface Outcome {
  tone: "ok" | "info" | "problem";
  text: string;
  detail?: string;
  /** `form` is shown beside the form that was open, so a wrong passphrase is read where it is fixed. */
  scope: "panel" | "form";
}

type Open =
  | { kind: "join"; ssid: string }
  | { kind: "hidden" }
  | { kind: "forget"; ssid: string; current: boolean };

const quote = (ssid: string) => `“${ssid}”`;

function PassphraseField({
  id,
  label,
  value,
  onChange,
  disabled,
  error,
  inputRef,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (v: string) => void;
  disabled: boolean;
  error: string | null;
  inputRef?: React.Ref<HTMLInputElement>;
}) {
  const [shown, setShown] = useState(false);
  return (
    <div className="net__field">
      <label htmlFor={id}>{label}</label>
      <div className="net__pass">
        <input
          id={id}
          ref={inputRef}
          className="net__input"
          type={shown ? "text" : "password"}
          value={value}
          disabled={disabled}
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? `${id}-err` : undefined}
          onChange={(e) => onChange(e.target.value)}
        />
        <button
          type="button"
          className="net__btn net__btn--icon"
          aria-pressed={shown}
          aria-label={shown ? "Hide passphrase" : "Show passphrase"}
          onClick={() => setShown((s) => !s)}
        >
          {shown ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
        </button>
      </div>
      {error && (
        <p className="net__fieldError" id={`${id}-err`} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

function LeavesNote({ from }: { from: string }) {
  return (
    <p className="net__headsup">
      <Info size={14} aria-hidden="true" />
      <span>
        This moves the pond off {quote(from)}. This window may lose contact with it for a moment; it
        keeps trying. A pond that is also on a cable stays reachable.
      </span>
    </p>
  );
}

function JoinForm({
  network,
  wifi,
  joining,
  problem,
  onJoin,
  onCancel,
}: {
  network: WifiNetwork;
  wifi: WifiLink;
  joining: boolean;
  problem: string | null;
  onJoin: (passphrase: string | undefined) => void;
  onCancel: () => void;
}) {
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const [passphrase, setPassphrase] = useState("");
  const [tried, setTried] = useState(false);
  const open = network.security === "open";
  const fieldProblem = open ? null : passphraseProblem(passphrase, network.security);

  useEffect(() => input.current?.focus(), []);

  function submit(e: FormEvent) {
    e.preventDefault();
    setTried(true);
    if (fieldProblem) return;
    onJoin(open ? undefined : passphrase);
  }

  return (
    <form
      className="net__form"
      aria-label={`Join ${network.ssid}`}
      onSubmit={submit}
      onKeyDown={(e) => e.key === "Escape" && !joining && onCancel()}
    >
      {open ? (
        <p className="net__line">
          {quote(network.ssid)} has no passphrase, so anyone nearby can connect to it.
        </p>
      ) : (
        <PassphraseField
          id={`${id}-pass`}
          label={`Passphrase for ${network.ssid}`}
          value={passphrase}
          onChange={setPassphrase}
          disabled={joining}
          error={tried ? fieldProblem : null}
          inputRef={input}
        />
      )}
      {joinLeavesNetwork(wifi, network.ssid) && <LeavesNote from={wifi.ssid ?? ""} />}
      {problem && <Message tone="problem" text={problem} />}
      {joining && <JoiningBanner ssid={network.ssid} />}
      <div className="net__actions">
        <button className="net__btn net__btn--raised" type="submit" disabled={joining} aria-label={`Join ${network.ssid}`}>
          Join
        </button>
        <button className="net__btn" type="button" disabled={joining} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function HiddenForm({
  wifi,
  joining,
  problem,
  onJoin,
  onCancel,
}: {
  wifi: WifiLink;
  joining: boolean;
  problem: string | null;
  onJoin: (req: WifiJoinRequest) => void;
  onCancel: () => void;
}) {
  const id = useId();
  const input = useRef<HTMLInputElement>(null);
  const [ssid, setSsid] = useState("");
  const [passphrase, setPassphrase] = useState("");
  const [tried, setTried] = useState(false);
  const nameProblem = ssidProblem(ssid);
  const keyProblem = passphraseProblem(passphrase, null);

  useEffect(() => input.current?.focus(), []);

  function submit(e: FormEvent) {
    e.preventDefault();
    setTried(true);
    if (nameProblem || keyProblem) return;
    onJoin({ ssid, hidden: true, ...(passphrase ? { passphrase } : {}) });
  }

  return (
    <form
      className="net__form"
      aria-label="Join a hidden network"
      onSubmit={submit}
      onKeyDown={(e) => e.key === "Escape" && !joining && onCancel()}
    >
      <div className="net__field">
        <label htmlFor={`${id}-ssid`}>Network name</label>
        <input
          id={`${id}-ssid`}
          ref={input}
          className="net__input"
          value={ssid}
          disabled={joining}
          autoComplete="off"
          autoCapitalize="off"
          autoCorrect="off"
          spellCheck={false}
          aria-invalid={tried && nameProblem ? true : undefined}
          aria-describedby={tried && nameProblem ? `${id}-ssid-err` : undefined}
          onChange={(e) => setSsid(e.target.value)}
        />
        {tried && nameProblem && (
          <p className="net__fieldError" id={`${id}-ssid-err`} role="alert">
            {nameProblem}
          </p>
        )}
      </div>
      <PassphraseField
        id={`${id}-pass`}
        label="Passphrase"
        value={passphrase}
        onChange={setPassphrase}
        disabled={joining}
        error={tried ? keyProblem : null}
      />
      <p className="net__line">Leave the passphrase empty only if the network is open.</p>
      {ssid.trim() !== "" && joinLeavesNetwork(wifi, ssid) && <LeavesNote from={wifi.ssid ?? ""} />}
      {problem && <Message tone="problem" text={problem} />}
      {joining && <JoiningBanner ssid={ssid} />}
      <div className="net__actions">
        <button className="net__btn net__btn--raised" type="submit" disabled={joining}>
          Join
        </button>
        <button className="net__btn" type="button" disabled={joining} onClick={onCancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}

function ForgetConfirm({
  ssid,
  current,
  busy,
  problem,
  onConfirm,
  onCancel,
}: {
  ssid: string;
  current: boolean;
  busy: boolean;
  problem: string | null;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="net__confirm" role="group" aria-label={`Forget ${ssid}`}>
      <p className="net__line net__line--strong">
        {current
          ? `${quote(ssid)} is the network the pond is on now. If you forget it, the pond may drop off your network and this app may stop reaching it. To bring the pond back you would set it up again from your phone, over Bluetooth.`
          : `Forget ${quote(ssid)}? The pond stops joining it on its own, and joining it again needs its passphrase.`}
      </p>
      {problem && <Message tone="problem" text={problem} />}
      <div className="net__actions">
        <button className="net__btn net__btn--danger" type="button" disabled={busy} onClick={onConfirm}>
          {busy && <Loader2 className="net__spin" size={14} aria-hidden="true" />}
          {current ? `Forget ${quote(ssid)} anyway` : `Forget ${quote(ssid)}`}
        </button>
        <button className="net__btn" type="button" disabled={busy} onClick={onCancel}>
          Keep it
        </button>
      </div>
    </div>
  );
}

/** Its own clock, so the seconds ticking over do not redraw the whole panel. */
function JoiningBanner({ ssid }: { ssid: string }) {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    const timer = window.setInterval(() => setSeconds((s) => s + 1), 1_000);
    return () => window.clearInterval(timer);
  }, []);
  return (
    <div className="net__notice" role="status">
      <Loader2 className="net__spin" size={15} aria-hidden="true" />
      <span>
        Joining {quote(ssid)}. This can take up to {JOIN_WAIT_SECONDS} seconds, and you can keep using the app.
        <span className="net__bar-track" aria-hidden="true">
          <span className="net__bar-fill" />
        </span>
        <span className="net__mono net__detail" aria-hidden="true">
          {seconds} s
        </span>
      </span>
    </div>
  );
}

export function WifiPanel({ variant }: { variant: Variant }) {
  const { connections, platform, recovered, refresh, expectDrop } = useConnections();
  const notesId = useId();

  const [networks, setNetworks] = useState<WifiNetwork[] | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanError, setScanError] = useState<string | null>(null);
  const [open, setOpen] = useState<Open | null>(null);
  const [joining, setJoining] = useState<string | null>(null);
  const [forgetting, setForgetting] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // What was said about losing contact is out of date once contact is back.
  useEffect(() => {
    if (recovered) setOutcome((o) => (o?.tone === "info" ? null : o));
  }, [recovered]);

  const wifi = connections?.wifi ?? null;
  const change = connections?.can_change ?? null;
  const changeReason = change && !change.allowed ? (change.reason ?? null) : null;

  const scan = useCallback(async () => {
    setScanning(true);
    setScanError(null);
    setOutcome(null);
    try {
      const found = await api.scanWifi();
      if (alive.current) setNetworks(found);
    } catch (e) {
      if (alive.current) setScanError(connectionErrorText(e, { action: "scan", platform }));
    } finally {
      if (alive.current) setScanning(false);
    }
  }, [platform]);

  async function join(req: WifiJoinRequest) {
    if (!wifi) return;
    const before = wifi;
    const scope: Outcome["scope"] = "form";
    setOutcome(null);
    setJoining(req.ssid);
    try {
      const result = await api.joinWifi(req);
      if (!alive.current) return;
      const moved = joinMoved(before, result);
      setOpen(null);
      setOutcome({
        tone: "ok",
        scope: "panel",
        text: moved
          ? `Joined ${quote(result.ssid)} at ${result.address}. The pond moved to another network, so this window may lose contact with it for a moment. If it does, the app keeps trying.`
          : `Joined ${quote(result.ssid)} at ${result.address}.`,
        detail: moved ? reachLine(result) : undefined,
      });
      setNetworks((list) =>
        list?.map((n) => ({
          ...n,
          current: n.ssid === result.ssid,
          known: n.known || n.ssid === result.ssid,
        })) ?? null,
      );
      if (moved) expectDrop();
      await refresh();
    } catch (e) {
      if (!alive.current) return;
      if (isConnectionLost(e)) {
        setOpen(null);
        setOutcome({
          tone: "info",
          scope: "panel",
          text: `Lost contact with the pond while it changed network. If it joined ${quote(req.ssid)} it will answer again shortly, and the app keeps trying.`,
        });
        expectDrop(true);
      } else {
        setOutcome({
          tone: "problem",
          scope,
          text: connectionErrorText(e, { action: "join", ssid: req.ssid, platform, changeReason }),
        });
        void refresh();
      }
    } finally {
      if (alive.current) setJoining(null);
    }
  }

  async function forget(ssid: string, current: boolean) {
    setForgetting(true);
    setOutcome(null);
    try {
      await api.forgetWifi(ssid, current);
      if (!alive.current) return;
      setOpen(null);
      setNetworks((list) => list?.map((n) => (n.ssid === ssid ? { ...n, known: false, current: false } : n)) ?? null);
      setOutcome({
        tone: current ? "info" : "ok",
        scope: "panel",
        text: current
          ? `Forgot ${quote(ssid)}. The pond may have left your network; if this app stops answering, it keeps trying.`
          : `Forgot ${quote(ssid)}.`,
      });
      if (current) expectDrop();
      await refresh();
    } catch (e) {
      if (!alive.current) return;
      const code = connectionErrorCode(e);
      if (isConnectionLost(e) && current) {
        setOpen(null);
        setOutcome({
          tone: "info",
          scope: "panel",
          text: `Lost contact with the pond while it left ${quote(ssid)}. The app keeps trying.`,
        });
        expectDrop(true);
      } else if (code === "in_use" && !current) {
        // The pond moved onto it since the list was drawn, so the stronger confirm applies.
        setOpen({ kind: "forget", ssid, current: true });
        setOutcome({ tone: "problem", scope: "form", text: connectionErrorText(e, { action: "forget", ssid }) });
      } else {
        if (code === "not_found") {
          setOpen(null);
          setNetworks((list) => list?.map((n) => (n.ssid === ssid ? { ...n, known: false } : n)) ?? null);
        }
        setOutcome({
          tone: "problem",
          scope: code === "not_found" ? "panel" : "form",
          text: connectionErrorText(e, { action: "forget", ssid, platform, changeReason }),
        });
        void refresh();
      }
    } finally {
      if (alive.current) setForgetting(false);
    }
  }

  function toggle(next: Open) {
    setOutcome(null);
    setOpen(next);
  }
  function cancel() {
    setOutcome(null);
    setOpen(null);
  }

  if (!wifi) {
    return (
      <div className={`net net--${variant}`}>
        <LinkHealthNotice always />
      </div>
    );
  }

  const notes = [...capabilityNotices(wifi), ...(change && !change.allowed ? [changeNote(change.reason)] : [])];
  const canScan = wifi.capabilities.scan.available && !joining;
  const writable = !!change?.allowed;
  const canJoin = wifi.capabilities.join.available && writable && !scanning && !joining;
  const canForget = wifi.capabilities.forget.available && writable && !joining && !forgetting;
  const describedBy = notes.length > 0 ? notesId : undefined;
  const inForm = outcome?.scope === "form" ? outcome : null;
  const onPanel = outcome?.scope === "panel" ? outcome : null;

  if (wifi.state === "unavailable") {
    return (
      <div className={`net net--${variant}`}>
        <LinkHealthNotice />
        <p className="net__empty">
          <WifiOff size={16} aria-hidden="true" />
          <span>
            <b>No Wi-Fi control here</b>
            {unavailableReason(wifi)}
          </span>
        </p>
      </div>
    );
  }

  const connected = wifi.state === "connected";

  return (
    <div className={`net net--${variant}`}>
      <LinkHealthNotice always />

      <div className="net__now" data-connected={connected || undefined}>
        <Wifi className="net__icon" size={20} aria-hidden="true" />
        <div className="net__text">
          {connected ? (
            <>
              <span className="net__title">
                {wifi.ssid ?? "Connected"}
                <span className="net__tag">Connected</span>
              </span>
              <span className="net__line">
                <SignalBars signal={wifi.signal} />
                {wifi.address && <span className="net__mono">{wifi.address}</span>}
              </span>
            </>
          ) : (
            <>
              <span className="net__title">Not on a Wi-Fi network</span>
              <span className="net__line">Find networks to see what the pond can join.</span>
            </>
          )}
        </div>
        {connected && wifi.ssid && (
          <button
            className="net__btn"
            type="button"
            disabled={!canForget}
            aria-describedby={describedBy}
            aria-label={`Forget ${wifi.ssid}`}
            onClick={() => toggle({ kind: "forget", ssid: wifi.ssid ?? "", current: true })}
          >
            <Trash2 size={14} aria-hidden="true" />
            Forget
          </button>
        )}
      </div>
      {open?.kind === "forget" && connected && open.ssid === wifi.ssid && (
        <ForgetConfirm
          ssid={open.ssid}
          current
          busy={forgetting}
          problem={inForm?.text ?? null}
          onConfirm={() => void forget(open.ssid, true)}
          onCancel={cancel}
        />
      )}

      {notes.length > 0 && (
        <ul className="net__notes" id={notesId}>
          {notes.map((n) => (
            <li key={n}>
              <Lock size={13} aria-hidden="true" />
              <span>{n}</span>
            </li>
          ))}
        </ul>
      )}

      {joining && !open && <JoiningBanner ssid={joining} />}
      {onPanel && <Message tone={onPanel.tone} text={onPanel.text} detail={onPanel.detail} />}

      <div className="net__actions">
        <button
          className="net__btn"
          type="button"
          disabled={!canScan || scanning}
          aria-describedby={describedBy}
          aria-busy={scanning || undefined}
          onClick={() => void scan()}
        >
          {scanning ? <Loader2 className="net__spin" size={14} aria-hidden="true" /> : <Search size={14} aria-hidden="true" />}
          {scanning ? "Finding networks" : "Find networks"}
        </button>
        <button
          className="net__btn"
          type="button"
          disabled={!canJoin}
          aria-describedby={describedBy}
          aria-expanded={open?.kind === "hidden"}
          onClick={() => (open?.kind === "hidden" ? cancel() : toggle({ kind: "hidden" }))}
        >
          <Plus size={14} aria-hidden="true" />
          Join a hidden network
        </button>
        <span className="net__when" aria-live="polite">
          {scanning
            ? "Scanning can take up to 15 seconds."
            : networks
              ? `${networks.length} network${networks.length === 1 ? "" : "s"} found.`
              : ""}
        </span>
      </div>

      {open?.kind === "hidden" && (
        <HiddenForm wifi={wifi} joining={!!joining} problem={inForm?.text ?? null} onJoin={(req) => void join(req)} onCancel={cancel} />
      )}

      {scanError && <Message tone="problem" text={scanError} />}

      {networks && networks.length === 0 && !scanError && (
        <p className="net__empty">
          <Wifi size={16} aria-hidden="true" />
          <span>
            <b>No networks found</b>
            Move the pond closer to a router, or join a hidden network by its name.
          </span>
        </p>
      )}

      {networks && networks.length > 0 && (
        <ul className="net__list" aria-label="Wi-Fi networks">
          {networks.map((n) => (
            <NetworkRow
              key={n.ssid}
              network={n}
              wifi={wifi}
              open={open}
              joining={joining}
              forgetting={forgetting}
              canJoin={canJoin}
              canForget={canForget}
              describedBy={describedBy}
              problem={inForm?.text ?? null}
              onOpen={toggle}
              onCancel={cancel}
              onJoin={(passphrase) =>
                void join({ ssid: n.ssid, hidden: false, ...(passphrase !== undefined ? { passphrase } : {}) })
              }
              onForget={(current) => void forget(n.ssid, current)}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function changeNote(reason: string | null): string {
  return `Changing the network is off for this device: ${endSentence(
    reason?.trim() || "only the phone that set up this pond, or a household member's device, can change it",
  )}`;
}

function NetworkRow({
  network: n,
  wifi,
  open,
  joining,
  forgetting,
  canJoin,
  canForget,
  describedBy,
  problem,
  onOpen,
  onCancel,
  onJoin,
  onForget,
}: {
  network: WifiNetwork;
  wifi: WifiLink;
  open: Open | null;
  joining: string | null;
  forgetting: boolean;
  canJoin: boolean;
  canForget: boolean;
  describedBy: string | undefined;
  problem: string | null;
  onOpen: (next: Open) => void;
  onCancel: () => void;
  onJoin: (passphrase: string | undefined) => void;
  onForget: (current: boolean) => void;
}) {
  // The list may be older than the status, so "now" is what either says.
  const current = n.current || (wifi.state === "connected" && wifi.ssid === n.ssid);
  const joinOpen = open?.kind === "join" && open.ssid === n.ssid;
  const forgetOpen = open?.kind === "forget" && open.ssid === n.ssid && !current;
  const enterprise = n.security === "enterprise";
  const Lockish = n.security === "open" ? LockOpen : Lock;

  return (
    <li className="net__item net__item--net" data-current={current || undefined}>
      <div className="net__row">
        <Lockish className="net__icon" size={16} aria-hidden="true" />
        <div className="net__text">
          <span className="net__title">
            {n.ssid}
            {current && <span className="net__tag">Connected</span>}
            {n.known && !current && <span className="net__tag">Saved</span>}
          </span>
          <span className="net__line">
            <span>{securityLabel(n.security)}</span>
            <SignalBars signal={n.signal} />
          </span>
          {enterprise && <span className="net__line">{ENTERPRISE_NOTE}</span>}
        </div>
        <div className="net__rowActions">
          {!enterprise && !current && !joinOpen && (
            <button
              className="net__btn"
              type="button"
              disabled={!canJoin}
              aria-describedby={describedBy}
              aria-label={`Join ${n.ssid}`}
              onClick={() => onOpen({ kind: "join", ssid: n.ssid })}
            >
              Join
            </button>
          )}
          {n.known && !current && !forgetOpen && (
            <button
              className="net__btn"
              type="button"
              disabled={!canForget}
              aria-describedby={describedBy}
              aria-label={`Forget ${n.ssid}`}
              onClick={() => onOpen({ kind: "forget", ssid: n.ssid, current: false })}
            >
              <Trash2 size={14} aria-hidden="true" />
              Forget
            </button>
          )}
        </div>
      </div>
      {joinOpen && (
        <JoinForm network={n} wifi={wifi} joining={joining === n.ssid} problem={problem} onJoin={onJoin} onCancel={onCancel} />
      )}
      {forgetOpen && open?.kind === "forget" && (
        <ForgetConfirm
          ssid={n.ssid}
          current={open.current}
          busy={forgetting}
          problem={problem}
          onConfirm={() => onForget(open.current)}
          onCancel={onCancel}
        />
      )}
    </li>
  );
}
