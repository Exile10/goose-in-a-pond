import { useState } from 'react'
import { api, isPreviewMode } from '../api'
import VerifyDevice from '../steps/VerifyDevice'
import CreateProfile from '../steps/CreateProfile'
import ConfigurePersonality from '../steps/ConfigurePersonality'
import ConnectDevices from '../steps/ConnectDevices'
import logo from '../assets/logo.png'
import '../onboarding.css'

// Mirrors OnboardingStep in pond-core/src/domain/onboarding.rs
type Step = 0 | 1 | 2 | 3 | 4

export interface OnboardingContext {
  sessionToken: string
  clientId: string
  profileName: string
  displayName: string
  personality: string
  assistantStyle: string
  devices: { id: string; name: string; type: string }[]
}

const STEP_LABELS = [
  'Verify Device',
  'Create Profile',
  'Configure Personality',
  'Connect Devices',
]

interface Props {
  onComplete: (token: string, displayName: string) => void
}

export default function Onboarding({ onComplete }: Props) {
  const [step, setStep] = useState<Step>(0)
  const [ctx, setCtx] = useState<Partial<OnboardingContext>>({})
  const [completing, setCompleting] = useState(false)
  const [completeError, setCompleteError] = useState<string | null>(null)

  function next(data: Partial<OnboardingContext>) {
    setCtx(prev => ({ ...prev, ...data }))
    setStep(s => (s < 4 ? ((s + 1) as Step) : 4))
  }

  function back() {
    setStep(s => (s > 0 ? ((s - 1) as Step) : 0))
  }

  if (step === 4) {
    return (
      <div className="ob-shell">
        <div className="ob-card ob-complete">
          <div className="ob-complete-icon">&#10003;</div>
          <h2>You're all set!</h2>
          <p>
            <strong>{ctx.displayName ?? ctx.profileName}</strong>, your pond is ready.
          </p>
          {completeError && (
            <p className="ob-error">
              Could not save onboarding state: {completeError}. Please try again.
            </p>
          )}
          <button className="ob-btn ob-btn-primary" disabled={completing} onClick={async () => {
            if (ctx.devices && ctx.devices.length > 0) {
              localStorage.setItem('pond_devices', JSON.stringify(ctx.devices))
            }

            // Tell the backend onboarding is complete. This lifts the onboarding
            // guard middleware so protected routes become accessible. The call
            // must succeed before we navigate — if it fails we show the error
            // and stay on this screen rather than letting the user reach the
            // dashboard in a broken state where every API call would be rejected.
            if (!isPreviewMode(ctx.sessionToken ?? '')) {
              setCompleting(true)
              setCompleteError(null)
              try {
                await api.completeOnboarding()
              } catch (err) {
                setCompleting(false)
                setCompleteError(err instanceof Error ? err.message : 'Could not reach the server')
                return
              }
              setCompleting(false)
            }

            // Sync personality settings now that protected routes are accessible.
            // Non-fatal: settings are in localStorage and will be re-synced when
            // the user visits Settings on the dashboard.
            if (ctx.sessionToken && !isPreviewMode(ctx.sessionToken) && ctx.personality) {
              try {
                await api.saveSettings(
                  { personality: ctx.personality, assistant_style: ctx.assistantStyle ?? 'proactive' },
                  ctx.sessionToken,
                )
              } catch {
                // intentionally ignored — see comment above
              }
            }
            onComplete(ctx.sessionToken ?? '', ctx.displayName ?? ctx.profileName ?? '')
          }}>
            {completing ? 'Saving…' : 'Go to dashboard'}
          </button>
        </div>
      </div>
    )
  }

  return (
    <div className="ob-shell">
      <div className="ob-card">
        <div className="ob-header">
          <img src={logo} alt="Goose In A Pond" className="ob-logo" />
          <span className="ob-step-count">Step {step + 1} of 4</span>
        </div>

        <div className="ob-steps">
          {STEP_LABELS.map((label, i) => (
            <div
              key={label}
              className={`ob-step ${i < step ? 'done' : i === step ? 'active' : ''}`}
            >
              <div className="ob-step-dot">
                {i < step ? <span>&#10003;</span> : <span>{i + 1}</span>}
              </div>
              <span className="ob-step-label">{label}</span>
            </div>
          ))}
        </div>

        <div className="ob-body">
          {step === 0 && <VerifyDevice onNext={next} />}
          {step === 1 && <CreateProfile ctx={ctx} onNext={next} onBack={back} />}
          {step === 2 && <ConfigurePersonality ctx={ctx} onNext={next} onBack={back} />}
          {step === 3 && <ConnectDevices ctx={ctx as OnboardingContext} onNext={next} onBack={back} />}
        </div>
      </div>
    </div>
  )
}
