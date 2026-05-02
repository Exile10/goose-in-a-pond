import { useEffect, useRef } from "react";
import { CheckCircle, XCircle, Bell, X } from "lucide-react";
import { useAppState, useAppDispatch } from "../state/AppContext";
import type { ScheduleToast } from "../state/reducer";

/* ── Single toast card ─────────────────────────────────────── */
function ToastCard({ toast }: { toast: ScheduleToast }) {
  const dispatch = useAppDispatch();
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    timerRef.current = setTimeout(() => {
      dispatch({ type: "DISMISS_TOAST", payload: toast.id });
    }, 5000);
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current);
    };
  }, [toast.id, dispatch]);

  const isOk = toast.status === "completed";
  const preview = isOk
    ? toast.result?.slice(0, 80) ?? null
    : toast.error?.slice(0, 80) ?? null;

  return (
    <div
      className="toast-card"
      data-status={toast.status}
      role="alert"
      aria-live="polite"
      onClick={() => dispatch({ type: "DISMISS_TOAST", payload: toast.id })}
    >
      <div className="toast-card__icon">
        {isOk
          ? <CheckCircle size={16} />
          : <XCircle size={16} />
        }
      </div>
      <div className="toast-card__body">
        <div className="toast-card__title">
          <Bell size={11} />
          <span>{toast.schedule_label}</span>
          <span className={`toast-card__badge toast-card__badge--${toast.status}`}>
            {toast.status}
          </span>
        </div>
        {preview && (
          <p className="toast-card__preview">{preview}</p>
        )}
      </div>
      <button
        className="toast-card__close"
        onClick={(e) => {
          e.stopPropagation();
          dispatch({ type: "DISMISS_TOAST", payload: toast.id });
        }}
        aria-label="Dismiss"
      >
        <X size={12} />
      </button>
    </div>
  );
}

/* ── Toast container rendered globally ────────────────────── */
export function ToastContainer() {
  const state = useAppState();
  const { scheduleToasts } = state;

  if (scheduleToasts.length === 0) return null;

  return (
    <div className="toast-container" aria-label="Notifications">
      {scheduleToasts.map((t) => (
        <ToastCard key={t.id} toast={t} />
      ))}
    </div>
  );
}
