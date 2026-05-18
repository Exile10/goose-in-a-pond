import React from "react";
import { STEPS } from "../onboarding.constants";

// ── Step header with progress bar ─────────────────────────

interface StepHeaderProps {
  stepIndex: number;
}

export function StepHeader({ stepIndex }: StepHeaderProps) {
  // Welcome (0) and Complete (last) don't show a header
  if (stepIndex === 0 || stepIndex === STEPS.length - 1) return null;

  const meta = STEPS[stepIndex];
  const total = STEPS.length - 2; // exclude welcome + complete
  const pct = Math.round((stepIndex / total) * 100);

  return (
    <div className="ob-step-header">
      <div className="ob-step-header__counter">
        Step {stepIndex} of {total}
      </div>
      <h1 className="ob-step-header__title">{meta.label}</h1>
      <div className="ob-step-header__track">
        <div
          className="ob-step-header__bar"
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

// ── Action bar (Back / Skip / Continue) ───────────────────

interface ActionsProps {
  onBack: () => void;
  onSkip?: () => void;
  onNext: () => void;
  nextLabel?: string;
  disabled?: boolean;
  isPersisting?: boolean;
}

export function Actions({
  onBack,
  onSkip,
  onNext,
  nextLabel = "Continue",
  disabled = false,
  isPersisting = false,
}: ActionsProps) {
  return (
    <div className="ob-actions">
      <button
        type="button"
        onClick={onBack}
        className="ob-actions__back"
      >
        &larr; Back
      </button>
      <div className="ob-actions__spacer" />
      {onSkip && (
        <button
          type="button"
          onClick={onSkip}
          className="ob-actions__skip"
        >
          Skip for now
        </button>
      )}
      <button
        type="button"
        onClick={onNext}
        disabled={disabled || isPersisting}
        className="ob-actions__next"
      >
        {isPersisting ? "Saving\u2026" : `${nextLabel} \u2192`}
      </button>
    </div>
  );
}

// ── Lead paragraph ────────────────────────────────────────

export function Lead({ children }: { children: React.ReactNode }) {
  return <p className="ob-lead">{children}</p>;
}

// ── Error banner ──────────────────────────────────────────

interface ErrorBannerProps {
  message: string;
  onDismiss: () => void;
}

export function ErrorBanner({ message, onDismiss }: ErrorBannerProps) {
  return (
    <div className="ob-error-banner" role="alert">
      <span className="ob-error-banner__text">{message}</span>
      <button type="button" onClick={onDismiss} className="ob-error-banner__dismiss">&times;</button>
    </div>
  );
}
