import { useState, useEffect, useCallback, useRef } from "react";
import {
  Button,
  Card,
  CardContent,
  Switch,
} from "@heroui/react";
import {
  Puzzle,
  Trash2,
  ChevronDown,
  ChevronUp,
  Plus,
  RefreshCw,
  Wrench,
  Download,
  Check,
  Star,
  AlertCircle,
  Store,
  KeyRound,
  Eye,
  EyeOff,
  X,
  Music,
  ExternalLink,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { useAppState } from "../state/AppContext";
import type { Extension, AddExtensionRequest, MarketplaceExtension, SecretRequirement } from "../api/types";

// ── Secret Config Modal ───────────────────────────────────────

type SecretModalMode = "install" | "edit";

interface SecretConfigModalProps {
  ext: MarketplaceExtension;
  mode: SecretModalMode;
  /** fulfilled map for edit mode — key → already stored */
  fulfilledMap?: Record<string, boolean>;
  onClose: () => void;
  onComplete: (secrets: Record<string, string>) => Promise<void>;
}

/** One password field with show/hide toggle and inline error. */
function SecretField({
  req,
  value,
  onChange,
  error,
  fulfilled,
  disabled,
}: {
  req: SecretRequirement;
  value: string;
  onChange: (v: string) => void;
  error?: string;
  fulfilled?: boolean;
  disabled: boolean;
}) {
  const [visible, setVisible] = useState(false);

  return (
    <div className="secret-modal__field">
      <label className="secret-modal__field-label" htmlFor={`secret-${req.key}`}>
        {req.display_name}
        {req.required && <span className="secret-modal__required-badge">Required</span>}
        {fulfilled && !value && (
          <span className="secret-modal__fulfilled-indicator">
            <Check size={11} strokeWidth={2.5} />
            Saved
          </span>
        )}
      </label>

      <div className="secret-modal__input-wrap">
        <input
          id={`secret-${req.key}`}
          type={visible ? "text" : "password"}
          className={`secret-modal__input${fulfilled && !value ? " secret-modal__input--fulfilled" : ""}`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={fulfilled ? "Leave blank to keep existing value" : `Enter your ${req.display_name}`}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          disabled={disabled}
        />
        <button
          type="button"
          className="secret-modal__input-toggle"
          onClick={() => setVisible((v) => !v)}
          aria-label={visible ? "Hide value" : "Show value"}
          tabIndex={-1}
        >
          {visible
            ? <EyeOff size={13} strokeWidth={1.8} />
            : <Eye size={13} strokeWidth={1.8} />
          }
        </button>
      </div>

      {req.description && !error && (
        <p className="secret-modal__field-hint">{req.description}</p>
      )}
      {error && (
        <p className="secret-modal__field-error">{error}</p>
      )}
    </div>
  );
}

/** OAuth sign-in block for a single oauth_flow requirement. */
function OAuthBlock({
  req,
  extensionId,
  onAuthorized,
  disabled,
}: {
  req: SecretRequirement;
  extensionId: string;
  onAuthorized: () => void;
  disabled: boolean;
}) {
  const [oauthState, setOauthState] = useState<"idle" | "polling" | "done">("idle");
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  function stopPoll() {
    if (pollRef.current !== null) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }

  useEffect(() => () => stopPoll(), []);

  async function handleSignIn() {
    setError(null);
    setOauthState("polling");
    try {
      const { auth_url } = await api.initiateOAuth(req.key, extensionId);
      window.open(auth_url, "_blank", "noopener,noreferrer");

      // Poll checkSecret every 2s until the token appears
      pollRef.current = setInterval(async () => {
        try {
          const exists = await api.checkSecret(req.key);
          if (exists) {
            stopPoll();
            setOauthState("done");
            onAuthorized();
          }
        } catch {
          // ignore transient check failures, keep polling
        }
      }, 2000);
    } catch (err) {
      setOauthState("idle");
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  // Determine icon — music for Spotify-like, generic KeyRound otherwise
  const isMusic = req.key.toLowerCase().includes("spotify") || req.display_name.toLowerCase().includes("spotify");
  const BtnIcon = isMusic ? Music : KeyRound;

  return (
    <div className="secret-modal__oauth-block">
      <p className="secret-modal__oauth-desc">{req.description}</p>

      {oauthState === "idle" && (
        <button
          type="button"
          className="secret-modal__oauth-btn"
          onClick={handleSignIn}
          disabled={disabled}
        >
          <BtnIcon size={13} strokeWidth={1.8} />
          Sign in with {req.display_name}
          <ExternalLink size={11} strokeWidth={2} />
        </button>
      )}

      {oauthState === "polling" && (
        <>
          <div className="secret-modal__oauth-polling">
            <div className="secret-modal__oauth-spinner" />
            <span>Waiting for authorisation in browser…</span>
          </div>
          <button
            type="button"
            className="secret-modal__oauth-btn"
            onClick={handleSignIn}
            disabled={disabled}
            style={{ marginTop: 4 }}
          >
            <ExternalLink size={11} strokeWidth={2} />
            Reopen sign-in window
          </button>
        </>
      )}

      {oauthState === "done" && (
        <div className="secret-modal__fulfilled-indicator">
          <Check size={13} strokeWidth={2.5} />
          Authorised
        </div>
      )}

      {error && <p className="secret-modal__field-error">{error}</p>}
    </div>
  );
}

function SecretConfigModal({
  ext,
  mode,
  fulfilledMap = {},
  onClose,
  onComplete,
}: SecretConfigModalProps) {
  const apiKeyReqs = ext.required_secrets.filter((r) => r.kind === "api_key" || r.kind === "generic");
  const oauthReqs = ext.required_secrets.filter((r) => r.kind === "oauth_flow");

  // Track text values for api_key / generic fields
  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(apiKeyReqs.map((r) => [r.key, ""])),
  );
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [globalError, setGlobalError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  // Track which oauth secrets are now authorised (checked by poll)
  const [oauthDone, setOauthDone] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(oauthReqs.map((r) => [r.key, fulfilledMap[r.key] ?? false])),
  );

  function setValue(key: string, val: string) {
    setValues((prev) => ({ ...prev, [key]: val }));
    setFieldErrors((prev) => { const n = { ...prev }; delete n[key]; return n; });
  }

  function validate(): boolean {
    const errors: Record<string, string> = {};
    for (const req of apiKeyReqs) {
      if (req.required && !values[req.key]?.trim() && !fulfilledMap[req.key]) {
        errors[req.key] = `${req.display_name} is required.`;
      }
    }
    for (const req of oauthReqs) {
      if (req.required && !oauthDone[req.key]) {
        errors[req.key] = `Please authorise ${req.display_name} before continuing.`;
      }
    }
    setFieldErrors(errors);
    return Object.keys(errors).length === 0;
  }

  async function handleSave() {
    if (!validate()) return;
    setSaving(true);
    setGlobalError(null);
    try {
      // Build secrets map — only include non-empty values
      const secrets: Record<string, string> = {};
      for (const req of apiKeyReqs) {
        if (values[req.key]?.trim()) {
          secrets[req.key] = values[req.key].trim();
        }
      }
      await onComplete(secrets);
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  // Close on backdrop click
  function handleBackdropClick(e: React.MouseEvent<HTMLDivElement>) {
    if (e.target === e.currentTarget) onClose();
  }

  // Close on Escape
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  const hasMixedSecrets = apiKeyReqs.length > 0 && oauthReqs.length > 0;

  return (
    <div className="secret-modal-backdrop" onClick={handleBackdropClick}>
      <div className="secret-modal" role="dialog" aria-modal="true" aria-label={`Configure ${ext.name}`}>

        {/* Header */}
        <div className="secret-modal__header">
          <div className="secret-modal__header-left">
            <div className="secret-modal__icon">
              <KeyRound size={15} strokeWidth={1.8} />
            </div>
            <div className="secret-modal__title-group">
              <h2 className="secret-modal__title">
                {mode === "edit" ? `Update secrets — ${ext.name}` : `Configure ${ext.name}`}
              </h2>
              <p className="secret-modal__subtitle">
                {mode === "edit"
                  ? "Update credentials or re-authorise connections."
                  : "This extension needs credentials to work."}
              </p>
            </div>
          </div>
          <button
            type="button"
            className="secret-modal__close"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>

        {/* Body */}
        <div className="secret-modal__body">

          {/* API key / generic fields */}
          {apiKeyReqs.length > 0 && (
            <>
              {hasMixedSecrets && (
                <p className="secret-modal__section-label">API credentials</p>
              )}
              {apiKeyReqs.map((req) => (
                <SecretField
                  key={req.key}
                  req={req}
                  value={values[req.key] ?? ""}
                  onChange={(v) => setValue(req.key, v)}
                  error={fieldErrors[req.key]}
                  fulfilled={fulfilledMap[req.key]}
                  disabled={saving}
                />
              ))}
            </>
          )}

          {/* Divider between mixed sections */}
          {hasMixedSecrets && <hr className="secret-modal__divider" />}

          {/* OAuth flow blocks */}
          {oauthReqs.length > 0 && (
            <>
              {hasMixedSecrets && (
                <p className="secret-modal__section-label">Account connections</p>
              )}
              {oauthReqs.map((req) => (
                <div key={req.key}>
                  <OAuthBlock
                    req={req}
                    extensionId={ext.id}
                    onAuthorized={() =>
                      setOauthDone((prev) => ({ ...prev, [req.key]: true }))
                    }
                    disabled={saving}
                  />
                  {fieldErrors[req.key] && (
                    <p className="secret-modal__field-error" style={{ marginTop: 4 }}>
                      {fieldErrors[req.key]}
                    </p>
                  )}
                </div>
              ))}
            </>
          )}

          {/* Global error */}
          {globalError && (
            <div className="secret-modal__global-error">
              <AlertCircle size={13} strokeWidth={2} style={{ flexShrink: 0, marginTop: 1 }} />
              {globalError}
            </div>
          )}
        </div>

        {/* Footer actions */}
        <div className="secret-modal__actions">
          <button
            type="button"
            className="secret-modal__cancel-btn"
            onClick={onClose}
            disabled={saving}
          >
            Cancel
          </button>
          <button
            type="button"
            className="secret-modal__save-btn"
            onClick={handleSave}
            disabled={saving}
          >
            {saving && <span className="secret-modal__save-spinner" />}
            {mode === "edit" ? "Save Changes" : "Save & Install"}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Extension Card ────────────────────────────────────────────

function ExtensionCard({
  ext,
  onToggle,
  onDelete,
  onConfigureSecrets,
  hasSecrets,
  disabled,
}: {
  ext: Extension;
  onToggle: (name: string, enabled: boolean) => void;
  onDelete: (name: string) => void;
  onConfigureSecrets?: (name: string) => void;
  hasSecrets?: boolean;
  disabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className={`ext-card${ext.enabled ? " ext-card--enabled" : " ext-card--disabled"}`}>
      <div className="ext-card__header">
        {/* Icon + info */}
        <div className="ext-card__icon">
          <Puzzle size={15} strokeWidth={1.8} />
        </div>
        <div className="ext-card__info">
          <div className="ext-card__title-row">
            {ext.status && (
              <span
                className={`ext-card__status-dot ext-card__status-dot--${ext.status}`}
                title={
                  ext.status === "error" && ext.last_error
                    ? `Error: ${ext.last_error}`
                    : ext.status.charAt(0).toUpperCase() + ext.status.slice(1)
                }
                aria-label={`Status: ${ext.status}`}
              />
            )}
            <span className="ext-card__name">{ext.name}</span>
            <span className={`ext-card__kind-badge ext-card__kind-badge--${ext.kind === "stdio" ? "stdio" : "http"}`}>
              {ext.kind}
            </span>
            {ext.tools.length > 0 && (
              <span className="ext-card__tool-count">
                {ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}
              </span>
            )}
          </div>
          {ext.description && (
            <div className="ext-card__desc">{ext.description}</div>
          )}
          {ext.last_error && (
            <div className="ext-card__error-line" title={ext.last_error}>
              {ext.last_error}
            </div>
          )}
        </div>

        {/* Actions */}
        <div className="ext-card__actions">
          {hasSecrets && onConfigureSecrets && (
            <button
              className="ext-card__secrets-btn"
              onClick={() => onConfigureSecrets(ext.name)}
              aria-label={`Configure credentials for ${ext.name}`}
              title="Update credentials"
            >
              <KeyRound size={12} strokeWidth={1.8} />
            </button>
          )}
          {ext.tools.length > 0 && (
            <button
              className="ext-card__expand-btn"
              onClick={() => setExpanded((v) => !v)}
              aria-label={expanded ? "Collapse tools" : "Expand tools"}
              aria-expanded={expanded}
            >
              {expanded ? <ChevronUp size={14} strokeWidth={1.8} /> : <ChevronDown size={14} strokeWidth={1.8} />}
            </button>
          )}
          <Switch
            isSelected={ext.enabled}
            onValueChange={(val) => onToggle(ext.name, val)}
            isDisabled={disabled}
            size="sm"
            aria-label={`${ext.enabled ? "Disable" : "Enable"} ${ext.name}`}
          />
          <button
            className="ext-card__delete-btn"
            onClick={() => onDelete(ext.name)}
            disabled={disabled}
            aria-label={`Delete ${ext.name}`}
            title={`Delete ${ext.name}`}
          >
            <Trash2 size={13} strokeWidth={1.8} />
          </button>
        </div>
      </div>

      {/* Expandable tools list */}
      {expanded && ext.tools.length > 0 && (
        <div className="ext-card__tools">
          <div className="ext-card__tools-header">
            <Wrench size={11} strokeWidth={1.8} />
            <span>Available tools</span>
          </div>
          <div className="ext-card__tool-list">
            {ext.tools.map((tool) => (
              <div key={tool} className="ext-card__tool-item">
                <code>{tool}</code>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Add Extension Form ────────────────────────────────────────

type ExtKind = "stdio" | "streamable_http";

function AddExtensionForm({
  onAdd,
  disabled,
}: {
  onAdd: (req: AddExtensionRequest) => Promise<void>;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [kind, setKind] = useState<ExtKind>("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [uri, setUri] = useState("");

  function reset() {
    setName("");
    setKind("stdio");
    setCommand("");
    setArgs("");
    setUri("");
    setError(null);
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) { setError("Name is required."); return; }
    if (kind === "stdio" && !command.trim()) { setError("Command is required for stdio extensions."); return; }
    if (kind === "streamable_http" && !uri.trim()) { setError("URI is required for HTTP extensions."); return; }

    const req: AddExtensionRequest = {
      name: name.trim(),
      kind,
      ...(kind === "stdio" && {
        command: command.trim(),
        args: args.trim() ? args.split(",").map((a) => a.trim()).filter(Boolean) : [],
      }),
      ...(kind === "streamable_http" && { uri: uri.trim() }),
    };

    setSubmitting(true);
    setError(null);
    try {
      await onAdd(req);
      reset();
      setOpen(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="ext-add-accordion">
      <button
        className="ext-add-accordion__trigger"
        onClick={() => { setOpen((v) => !v); if (!open) setError(null); }}
        aria-expanded={open}
      >
        <span className="ext-add-accordion__trigger-label">
          <Plus size={13} strokeWidth={2} />
          Add Extension
        </span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <form className="ext-add-form" onSubmit={handleSubmit} noValidate>
          {/* Name */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-name">
              Name <span className="ext-form-required">*</span>
            </label>
            <input
              id="ext-name"
              className="ext-form-input"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="my-extension"
              required
              autoComplete="off"
              disabled={disabled || submitting}
            />
          </div>

          {/* Kind */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-kind">Kind</label>
            <select
              id="ext-kind"
              className="ext-form-select"
              value={kind}
              onChange={(e) => setKind(e.target.value as ExtKind)}
              disabled={disabled || submitting}
            >
              <option value="stdio">stdio</option>
              <option value="streamable_http">streamable_http</option>
            </select>
          </div>

          {/* Conditional: stdio */}
          {kind === "stdio" && (
            <>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-command">
                  Command <span className="ext-form-required">*</span>
                </label>
                <input
                  id="ext-command"
                  className="ext-form-input"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="/usr/local/bin/my-mcp-server"
                  required
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-args">
                  Args
                  <span className="ext-form-hint"> (comma-separated)</span>
                </label>
                <input
                  id="ext-args"
                  className="ext-form-input"
                  value={args}
                  onChange={(e) => setArgs(e.target.value)}
                  placeholder="--port, 8080, --verbose"
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
            </>
          )}

          {/* Conditional: http */}
          {kind === "streamable_http" && (
            <div className="ext-form-row">
              <label className="ext-form-label" htmlFor="ext-uri">
                URI <span className="ext-form-required">*</span>
              </label>
              <input
                id="ext-uri"
                className="ext-form-input"
                value={uri}
                onChange={(e) => setUri(e.target.value)}
                placeholder="http://localhost:3001/mcp"
                type="url"
                required
                autoComplete="off"
                disabled={disabled || submitting}
              />
            </div>
          )}

          {/* Error */}
          {error && (
            <p className="ext-form-error">{error}</p>
          )}

          {/* Actions */}
          <div className="ext-form-actions">
            <Button
              variant="ghost"
              size="sm"
              onPress={() => { setOpen(false); reset(); }}
              isDisabled={submitting}
              type="button"
            >
              Cancel
            </Button>
            <Button
              variant="secondary"
              size="sm"
              type="submit"
              isDisabled={disabled || submitting}
            >
              {submitting ? "Adding…" : "Add Extension"}
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}

// ── Marketplace Card ──────────────────────────────────────────

function MarketplaceCard({
  ext,
  isInstalled,
  onInstall,
}: {
  ext: MarketplaceExtension;
  isInstalled: boolean;
  onInstall: (id: string, secrets?: Record<string, string>) => Promise<void>;
}) {
  const [installing, setInstalling] = useState(false);
  const [justInstalled, setJustInstalled] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [showSecretModal, setShowSecretModal] = useState(false);

  const needsSecrets = ext.required_secrets && ext.required_secrets.length > 0;

  async function handleInstallClick() {
    if (isInstalled || justInstalled || installing) return;
    if (needsSecrets) {
      // Show the secret config modal instead of installing immediately
      setShowSecretModal(true);
    } else {
      await doInstall();
    }
  }

  async function doInstall(secrets?: Record<string, string>) {
    setInstalling(true);
    setInstallError(null);
    try {
      await onInstall(ext.id, secrets);
      setJustInstalled(true);
      setShowSecretModal(false);
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
      throw err; // let modal show the error inline
    } finally {
      setInstalling(false);
    }
  }

  const installed = isInstalled || justInstalled;

  return (
    <>
      <div className={`mkt-card${ext.featured ? " mkt-card--featured" : ""}`}>
        {ext.featured && (
          <div className="mkt-card__featured-badge">
            <Star size={9} strokeWidth={2} />
            Featured
          </div>
        )}
        <div className="mkt-card__header">
          <div className="mkt-card__icon">
            <Puzzle size={15} strokeWidth={1.8} />
          </div>
          <div className="mkt-card__info">
            <div className="mkt-card__name">{ext.name}</div>
            <div className="mkt-card__meta">
              <span className="mkt-card__category-badge">{ext.category}</span>
              <span className="mkt-card__tool-count">
                {ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}
              </span>
              {needsSecrets && (
                <span className="mkt-card__tool-count" title="Requires credentials">
                  <KeyRound size={9} strokeWidth={2} style={{ display: "inline", verticalAlign: "middle" }} /> credentials
                </span>
              )}
            </div>
          </div>
          <div className="mkt-card__action">
            {installed ? (
              <span className="mkt-card__installed-badge">
                <Check size={11} strokeWidth={2.5} />
                Installed
              </span>
            ) : (
              <button
                className="mkt-card__install-btn"
                onClick={handleInstallClick}
                disabled={installing}
                aria-label={`Install ${ext.name}`}
              >
                {installing ? (
                  <span className="mkt-card__install-spinner" />
                ) : (
                  <Download size={12} strokeWidth={2} />
                )}
                {installing ? "Installing…" : "Install"}
              </button>
            )}
          </div>
        </div>
        <p className="mkt-card__desc">{ext.description}</p>
        {installError && (
          <div className="ext-card__error-line" title={installError}>
            {installError}
          </div>
        )}
        <div className="mkt-card__footer">
          <span className="mkt-card__author">by {ext.author}</span>
          <span className={`mkt-card__kind-badge mkt-card__kind-badge--${ext.kind === "stdio" ? "stdio" : "http"}`}>
            {ext.kind}
          </span>
        </div>
      </div>

      {showSecretModal && (
        <SecretConfigModal
          ext={ext}
          mode="install"
          onClose={() => setShowSecretModal(false)}
          onComplete={doInstall}
        />
      )}
    </>
  );
}

// ── Browse Tab ────────────────────────────────────────────────

function BrowseTab({
  installedExtensions,
  onInstallSuccess,
}: {
  installedExtensions: Extension[];
  onInstallSuccess: (ext: Extension) => void;
}) {
  const [items, setItems] = useState<MarketplaceExtension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api.listMarketplace()
      .then((list) => { if (!cancelled) setItems(list); })
      .catch((err) => { if (!cancelled) setError(String(err)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  const installedNames = new Set(installedExtensions.map((e) => e.name.toLowerCase()));

  async function handleInstall(id: string, secrets?: Record<string, string>) {
    const ext = await api.installMarketplaceExtension(id, secrets);
    onInstallSuccess(ext);
  }

  if (loading) {
    return (
      <div className="mkt-loading">
        <div className="mkt-loading__spinner" />
        <span>Loading marketplace…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="mkt-error">
        <AlertCircle size={16} strokeWidth={1.8} />
        <span>{error}</span>
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="empty-state">
        <Store size={32} strokeWidth={1.2} />
        <div>
          <div style={{ fontWeight: "var(--weight-semibold)", marginBottom: 4 }}>
            No extensions available
          </div>
          <div style={{ fontSize: "var(--text-sm)", color: "var(--grey-500)" }}>
            The marketplace is empty right now. Check back later.
          </div>
        </div>
      </div>
    );
  }

  // featured first, then alphabetical
  const sorted = [...items].sort((a, b) => {
    if (a.featured && !b.featured) return -1;
    if (!a.featured && b.featured) return 1;
    return a.name.localeCompare(b.name);
  });

  return (
    <div className="mkt-grid">
      {sorted.map((ext) => (
        <MarketplaceCard
          key={ext.id}
          ext={ext}
          isInstalled={installedNames.has(ext.id) || installedNames.has(ext.name.toLowerCase())}
          onInstall={handleInstall}
        />
      ))}
    </div>
  );
}

// ── Main Extensions Component ─────────────────────────────────

type Tab = "installed" | "browse";

interface SecretEditState {
  /** The installed extension name being configured */
  extName: string;
  /** The marketplace metadata (for required_secrets list) — may be null if not found */
  mktExt: MarketplaceExtension | null;
  /** Which secrets are already fulfilled */
  fulfilledMap: Record<string, boolean>;
}

export function Extensions() {
  const state = useAppState();
  const [activeTab, setActiveTab] = useState<Tab>("installed");
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);

  // For edit-mode secret modal on installed extensions
  const [marketplaceCache, setMarketplaceCache] = useState<MarketplaceExtension[]>([]);
  const [secretEditState, setSecretEditState] = useState<SecretEditState | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.listExtensions();
      // Backend returns { extensions: [...] } or plain array
      const list = Array.isArray(res)
        ? (res as Extension[])
        : ((res as { extensions: Extension[] }).extensions ?? []);
      setExtensions(list);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3500);
  }

  async function handleToggle(name: string, enabled: boolean) {
    try {
      await api.toggleExtension(name, enabled);
      setExtensions((prev) =>
        prev.map((e) => e.name === name ? { ...e, enabled } : e),
      );
      flash(`${name} ${enabled ? "enabled" : "disabled"}.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleDelete(name: string) {
    if (!confirm(`Delete extension "${name}"? This cannot be undone.`)) return;
    try {
      await api.removeExtension(name);
      setExtensions((prev) => prev.filter((e) => e.name !== name));
      flash(`${name} removed.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleAdd(req: AddExtensionRequest) {
    const ext = await api.addExtension(req);
    setExtensions((prev) => [...prev, ext]);
    flash(`${ext.name} added.`);
  }

  // Lazily load marketplace listing to get required_secrets for installed exts
  useEffect(() => {
    if (marketplaceCache.length === 0) {
      api.listMarketplace()
        .then((list) => setMarketplaceCache(list))
        .catch(() => { /* non-critical, ignore */ });
    }
  }, [marketplaceCache.length]);

  async function handleConfigureSecrets(extName: string) {
    // Find matching marketplace entry by name
    const mktExt = marketplaceCache.find(
      (m) => m.name.toLowerCase() === extName.toLowerCase() || m.id.toLowerCase() === extName.toLowerCase(),
    ) ?? null;

    if (!mktExt || mktExt.required_secrets.length === 0) return;

    // Fetch current fulfillment status
    let fulfilledMap: Record<string, boolean> = {};
    try {
      const res = await api.getExtensionSecrets(extName);
      fulfilledMap = res.fulfilled;
    } catch {
      // ignore — we'll show unfilled state
    }

    setSecretEditState({ extName, mktExt, fulfilledMap });
  }

  async function handleSecretEditComplete(secrets: Record<string, string>) {
    if (!secretEditState) return;
    if (Object.keys(secrets).length > 0) {
      await api.setExtensionSecrets(secretEditState.extName, secrets);
    }
    flash(`Credentials updated for ${secretEditState.extName}.`);
    setSecretEditState(null);
  }

  function handleInstallFromMarketplace(ext: Extension) {
    setExtensions((prev) => {
      const exists = prev.some((e) => e.name === ext.name);
      return exists ? prev : [...prev, ext];
    });
    flash(`${ext.name} installed.`);
    setActiveTab("installed");
  }

  const enabledCount = extensions.filter((e) => e.enabled).length;

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h1 className="page-header__title">Extensions</h1>
        <div className="page-header__action">
          {activeTab === "installed" && (
            <Button size="sm" variant="ghost" onPress={load} isDisabled={loading}>
              <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
              Refresh
            </Button>
          )}
        </div>
      </div>

      {/* Summary banner */}
      <div className="seg-banner seg-banner--info">
        <Puzzle size={14} />
        <span>
          MCP server extensions give the agent additional tools.
          {!loading && (
            <> {extensions.length} registered, {enabledCount} active.</>
          )}
        </span>
      </div>

      {/* Tab bar */}
      <div className="ext-tab-bar" role="tablist">
        <button
          className={`ext-tab-bar__tab${activeTab === "installed" ? " ext-tab-bar__tab--active" : ""}`}
          role="tab"
          aria-selected={activeTab === "installed"}
          onClick={() => setActiveTab("installed")}
        >
          Installed
          {extensions.length > 0 && (
            <span className="ext-tab-bar__count">{extensions.length}</span>
          )}
        </button>
        <button
          className={`ext-tab-bar__tab${activeTab === "browse" ? " ext-tab-bar__tab--active" : ""}`}
          role="tab"
          aria-selected={activeTab === "browse"}
          onClick={() => setActiveTab("browse")}
        >
          <Store size={12} strokeWidth={2} />
          Browse
        </button>
      </div>

      {/* Action feedback */}
      {actionMsg && (
        <p
          style={{
            margin: 0,
            fontSize: "var(--text-sm)",
            color: actionMsg.ok ? "var(--color-success)" : "var(--color-destructive)",
          }}
        >
          {actionMsg.text}
        </p>
      )}

      {/* Tab panels */}
      {activeTab === "installed" && (
        <>
          {/* Extensions list */}
          <Card shadow="none" className="giap-card">
            <CardContent>
              {loading && (
                <p style={{ margin: 0, fontSize: "var(--text-sm)", color: "var(--color-text-tertiary)" }}>
                  Loading extensions…
                </p>
              )}
              {!loading && error && (
                <p style={{ margin: 0, fontSize: "var(--text-sm)", color: "var(--color-destructive)" }}>
                  {error}
                </p>
              )}
              {!loading && !error && extensions.length === 0 && (
                <div className="empty-state">
                  <Puzzle size={32} strokeWidth={1.2} />
                  <div>
                    <div style={{ fontWeight: "var(--weight-semibold)", marginBottom: 4 }}>
                      No extensions registered
                    </div>
                    <div style={{ fontSize: "var(--text-sm)", color: "var(--grey-500)" }}>
                      Add an MCP server extension below, or{" "}
                      <button
                        className="ext-inline-link"
                        onClick={() => setActiveTab("browse")}
                      >
                        browse the marketplace
                      </button>
                      .
                    </div>
                  </div>
                </div>
              )}
              {!loading && !error && extensions.length > 0 && (
                <div className="ext-list-stack">
                  {extensions.map((ext) => {
                    const mktEntry = marketplaceCache.find(
                      (m) => m.name.toLowerCase() === ext.name.toLowerCase() || m.id.toLowerCase() === ext.name.toLowerCase(),
                    );
                    const hasSecrets = (mktEntry?.required_secrets?.length ?? 0) > 0;
                    return (
                      <ExtensionCard
                        key={ext.name}
                        ext={ext}
                        onToggle={handleToggle}
                        onDelete={handleDelete}
                        onConfigureSecrets={hasSecrets ? handleConfigureSecrets : undefined}
                        hasSecrets={hasSecrets}
                        disabled={!state.serverOnline}
                      />
                    );
                  })}
                </div>
              )}
            </CardContent>
          </Card>

          {/* Add form */}
          <AddExtensionForm onAdd={handleAdd} disabled={!state.serverOnline} />
        </>
      )}

      {activeTab === "browse" && (
        <Card shadow="none" className="giap-card">
          <CardContent>
            <BrowseTab
              installedExtensions={extensions}
              onInstallSuccess={handleInstallFromMarketplace}
            />
          </CardContent>
        </Card>
      )}

      {/* Edit-mode secret config modal for installed extensions */}
      {secretEditState && secretEditState.mktExt && (
        <SecretConfigModal
          ext={secretEditState.mktExt}
          mode="edit"
          fulfilledMap={secretEditState.fulfilledMap}
          onClose={() => setSecretEditState(null)}
          onComplete={handleSecretEditComplete}
        />
      )}
    </div>
  );
}
