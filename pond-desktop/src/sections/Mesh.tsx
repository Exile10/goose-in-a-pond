import { useState, useEffect, useRef, useCallback } from "react";
import { Button, Separator } from "@heroui/react";
import { Share2, Plus, X, Trash2, Copy, Wifi, WifiOff } from "lucide-react";
import QRCode from "qrcode";
import { api } from "../api/PondApiClient";
import type { MeshPeer, MeshSelf } from "../api/types";
import { PageHeader } from "../components/shared";

function truncatePeerId(peerId: string): string {
  return peerId.length > 16 ? `${peerId.slice(0, 8)}…${peerId.slice(-8)}` : peerId;
}

function formatMillisats(msats: number): string {
  return `${(msats / 1000).toLocaleString()} sats`;
}

/// Parses either a bare peer-id hex string or a `pond-mesh://invite?...` URL
/// pasted from another Pond, so "Add trusted peer" accepts both.
function parseInvite(input: string): { peerId: string; address?: string } {
  const trimmed = input.trim();
  if (!trimmed.startsWith("pond-mesh://")) {
    return { peerId: trimmed };
  }
  try {
    const url = new URL(trimmed);
    const peerId = url.searchParams.get("peer") ?? "";
    const address = url.searchParams.get("addr") ?? undefined;
    return { peerId, address };
  } catch {
    return { peerId: trimmed };
  }
}

export function Mesh() {
  const [peers, setPeers] = useState<MeshPeer[]>([]);
  const [self, setSelf] = useState<MeshSelf | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [inviteInput, setInviteInput] = useState("");
  const [trustScope, setTrustScope] = useState<"self_owned" | "circle">("circle");
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  const canvasRef = useRef<HTMLCanvasElement>(null);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    Promise.all([api.listMeshPeers(), api.getMeshSelf()])
      .then(([p, s]) => {
        setPeers(p);
        setSelf(s);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  useEffect(() => {
    if (!self?.invite_url || !canvasRef.current) return;
    QRCode.toCanvas(canvasRef.current, self.invite_url, {
      width: 180,
      margin: 2,
      color: { dark: "#000000", light: "#ffffff" },
    }).catch((e) => console.error("QR render failed", e));
  }, [self?.invite_url]);

  function openForm() {
    setInviteInput("");
    setTrustScope("circle");
    setFormError(null);
    setShowForm(true);
  }

  function closeForm() {
    setShowForm(false);
    setFormError(null);
  }

  async function handleAddPeer() {
    const { peerId, address } = parseInvite(inviteInput);
    if (!peerId) {
      setFormError("Peer ID or invite link is required.");
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await api.addMeshPeer({ peer_id: peerId, trust_scope: trustScope, address });
      closeForm();
      load();
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  async function handleRemove(peer: MeshPeer) {
    setBusyId(peer.peer_id);
    try {
      await api.removeMeshPeer(peer.peer_id);
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  }

  function handleCopyInvite() {
    if (!self?.invite_url) return;
    navigator.clipboard.writeText(self.invite_url).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <div className="screen screen--mesh">
      <PageHeader
        title="Mesh"
        action={
          <Button size="sm" variant="primary" className="page-header-btn" onPress={openForm}>
            <Plus size={14} /> Add trusted peer
          </Button>
        }
      />

      {loading && <p className="muted-12">Loading mesh…</p>}
      {error && <p className="muted-12 text-error">{error}</p>}

      {!loading && self && !self.mesh_enabled && (
        <div className="empty-state">
          <Share2 size={32} />
          <span>Mesh is disabled. Enable it in Settings to invite trusted peers.</span>
        </div>
      )}

      {!loading && self?.mesh_enabled && (
        <div style={{ display: "flex", gap: 32, flexWrap: "wrap", marginBottom: 24 }}>
          <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 10 }}>
            <div
              style={{
                border: "1px solid var(--color-border)",
                borderRadius: 12,
                padding: 10,
                background: "#fff",
                lineHeight: 0,
              }}
            >
              <canvas ref={canvasRef} width={180} height={180} />
            </div>
            <Button size="sm" variant="secondary" onPress={handleCopyInvite}>
              <Copy size={13} style={{ display: "inline" }} /> {copied ? "Copied!" : "Copy invite link"}
            </Button>
          </div>
          <div style={{ flex: 1, minWidth: 220 }}>
            <p className="muted-12" style={{ marginBottom: 8 }}>Your Pond ID</p>
            <code style={{ fontSize: 12, wordBreak: "break-all" }}>{self.peer_id}</code>
            <p className="muted-12" style={{ marginTop: 16 }}>
              Share the QR code or invite link with a trusted device or friend's Pond.
              They can scan it or paste it when adding you as a trusted peer.
            </p>
          </div>
        </div>
      )}

      {!loading && !error && peers.length === 0 && (
        <div className="empty-state">
          <Share2 size={32} />
          <span>No trusted peers yet.</span>
          <button className="empty-state__cta" onClick={openForm}>
            <Plus size={14} /> Add trusted peer
          </button>
        </div>
      )}

      {peers.length > 0 && (
        <div className="devices-grid">
          {peers.map((p) => (
            <div
              key={p.peer_id}
              className={`device-card${!p.connected ? " device-card--offline" : ""}`}
            >
              <div className="device-card__top">
                <span className="device-card__icon">
                  <Share2 size={22} />
                </span>
                <div className="device-card__info">
                  <div className="device-card__name">{truncatePeerId(p.peer_id)}</div>
                  <code className="device-card__ip">{formatMillisats(p.credit_balance_millisats)}</code>
                </div>
              </div>

              <div className="device-card__chips">
                <span className={`device-card__chip device-card__chip--${p.connected ? "online" : "offline"}`}>
                  {p.connected ? <Wifi size={11} /> : <WifiOff size={11} />}
                  {p.connected ? "connected" : "offline"}
                </span>
                <span className="device-card__chip">
                  {p.trust_scope === "self_owned" ? "own device" : "circle"}
                </span>
              </div>

              <div className="device-card__actions">
                <button
                  className="device-card__action-btn"
                  onClick={() => handleRemove(p)}
                  disabled={busyId === p.peer_id}
                  type="button"
                >
                  <Trash2 size={12} /> {busyId === p.peer_id ? "Removing…" : "Remove"}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {showForm && (
        <div className="sched-modal__overlay" onClick={closeForm}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">Add trusted peer</h2>
              <button className="sched-modal__close" onClick={closeForm} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />

            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Peer ID or invite link</label>
                <input
                  className="sched-modal__input"
                  placeholder="pond-mesh://invite?peer=... or a raw peer ID"
                  value={inviteInput}
                  onChange={(e) => setInviteInput(e.target.value)}
                  autoFocus
                />
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Trust scope</label>
                <select
                  className="sched-modal__select"
                  value={trustScope}
                  onChange={(e) => setTrustScope(e.target.value as "self_owned" | "circle")}
                >
                  <option value="circle">Circle (friend / family Pond)</option>
                  <option value="self_owned">My own device</option>
                </select>
              </div>

              {formError && <p className="text-error text-error--sm">{formError}</p>}
            </div>

            <Separator />

            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeForm}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={submitting || !inviteInput.trim()}
                onPress={handleAddPeer}
              >
                {submitting ? "Adding…" : "Add peer"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
