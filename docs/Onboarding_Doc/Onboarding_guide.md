# Onboarding Guide

Onboarding is the first-run setup: who the household is, where it is, how the assistant talks,
and which model it runs. Until it completes, the pond answers only the routes setup needs.

## Two gates

A request passes two independent checks in `crates/pond-api`:

| Gate | Where | Answers |
|---|---|---|
| Authentication | `auth_middleware`, `PUBLIC_ROUTES` (`src/middleware/mod.rs`) | `401` without a valid bearer token, unless the route's exposure admits the caller |
| Onboarding guard | `require_onboarding_complete` (`src/middleware/onboarding_guard.rs`) | `403 {"error":"onboarding_required","message":"Complete onboarding before using this feature"}` until the stored step is `Completed` |

Routes in the public router (`api_routes` in `src/routes.rs`) skip the onboarding guard; every
other route sits behind it. Which public routes also skip the token depends on whether the pond is
**claimed** (a phone scanned its setup card) and whether it is onboarded. The full table, and how
claiming works, is in [pond-networking.md, Claim before onboarding](../architecture/pond-networking.md#claim-before-onboarding).
In short: the wizard's writes answer a tokenless caller only from loopback on an unclaimed pond
(the desktop app before it pairs); once claimed, they need the claiming device's token; once
onboarded, they close.

## Steps

`OnboardingStep` (`crates/pond-core/src/user_data/domain/onboarding.rs`), in order:

| # | Step | Collects |
|---|---|---|
| 1 | `Welcome` | Introduction; the handshake happens here |
| 2 | `Basics` | Display name, preferred name, birthday, avatar |
| 3 | `Location` | Locale, timezone, city, weather |
| 4 | `Accessibility` | Atypical speech, slow speech, high contrast, reduced motion |
| 5 | `Personality` | Prompt style and personality hint |
| 6 | `GooseIdentity` | Assistant name, TTS voice |
| 7 | `WakeWord` | A preset or custom wake phrase |
| 8 | `Model` | Chat provider and model |
| 9 | `Extensions` | Curated extensions |
| 10 | `Completed` | Terminal; lifts the onboarding guard |

The current step lives in the single-row `onboarding_state` table in `pond_system.db`, so progress
survives restarts. An unknown stored value restarts the wizard.

Answers are not stored per step: each surface writes them as settings (`PUT /api/v1/settings`) and
member preferences (`POST /api/v1/profiles`, `PATCH /api/v1/profiles/{id}`), and reports progress
with `POST /api/v1/onboard/step/{name}` so a quit resumes in the right place.

## Endpoints

| Method and path | Exposure before onboarding | Does |
|---|---|---|
| `GET /api/v1/onboard/status` | Anyone | `{"onboarded","current_step","steps_completed","total_steps"}`; `current_step` is `not_started` before the first call, `total_steps` is 10 |
| `POST /api/v1/onboard` | Wizard | Starts at `Welcome`, or reports `in_progress` / `already_complete` |
| `POST /api/v1/onboard/step/{name}` | Wizard | Moves progress forward to `name` (never back); `400` with `valid_steps` for an unknown name; refuses `Completed` |
| `POST /api/v1/onboard/complete` | Wizard | Checks required settings, saves `Completed`, and attributes the claiming device to the primary member |
| `POST /api/v1/onboard/reset` | Wizard; after onboarding, loopback with a token or the host credential | Back to `Welcome`, re-arming the guard; settings are kept |

"Wizard" is `Exposure::UntilOnboarded`: tokenless from loopback on an unclaimed pond, otherwise a
paired device's token, and closed once onboarding completes.

`POST /onboard/complete` answers `400` with `missing_fields` unless `user_name`,
`assistant_name`, `timezone` and `chat_model` are all set. `chat_model` is written by activating a
model for the chat role (`POST /api/v1/models/{category}/{name}/activate`); that route and the
model list, download and progress routes take a paired device's token before onboarding for this
reason.

Other routes setup uses before completion: `PUT /settings` (wizard), `GET /settings` (token),
`/time/zones`, `/location/detect`, `/warmup`, `/voice/tts/apply`, `/voice/calibrate`,
`/voice/sample`, `/system/info`, `/network/status`, `/connections`, `/profiles`. The authoritative
list is `PUBLIC_ROUTES`.

## Surfaces

### Desktop app

`App.tsx` shows `OnboardingWizard` (`pond-desktop/src/components/onboarding/`) whenever
`GET /onboard/status` says `onboarded: false`. It has six screens; `FE_STEP_TO_BE` in
`onboarding.constants.ts` maps each to the backend step it reports:

| Screen | Reports | Writes |
|---|---|---|
| Welcome | `Welcome` | Nothing |
| About you | `Basics` | `user_name`; preferred name, birthday, avatar and accessibility flags as profile preferences (creating the primary profile if needed) |
| Language and location | `Location` | `timezone`, `weather_location_name`, `weather_enabled`, coordinates |
| Personality and identity | `GooseIdentity` | `prompt_style`, `assistant_personality`, `assistant_name`, `voice_tts_voice`, `voice_tts_speed` |
| Wake word | `WakeWord` | `voice_wake_word` |
| All set | `Extensions` | `agent_memory_inject` |

The persistence is `hooks/useOnboardingPersist.ts`. On finish the app calls
`POST /onboard/complete`. The wizard has no model screen (its summary reads "You choose one after
setup"), and it treats a refused completion as non-fatal, so a pond without a `chat_model` stays
un-onboarded and the wizard returns on the next launch until a model is activated.

Settings has a "Run setup again" button (developer view, behind a confirm) that calls
`POST /onboard/reset`.

### Phone (goose-on-the-go)

A pond with no screen is set up from the phone: it scans the printed setup card, which claims the
pond, then runs owner setup with its own token, including choosing the chat model. See
[pond-networking.md](../architecture/pond-networking.md).

### CLI

```bash
cargo run -p pond-server -- onboard           # resumes at the stored step
cargo run -p pond-server -- onboard --reset   # starts over
```

`run_onboard` in `crates/pond-server/src/main.rs` walks the same ten steps in the terminal (name,
timezone, conversation style, assistant name, wake word preset, a model from the catalog) and saves
the answers as settings when it reaches `Completed`.

## Tests

| Level | Command | Covers |
|---|---|---|
| Domain and service | `cargo test -p pond-core onboarding` | Step order, `advance_to` never moving back, reset |
| SQLite adapter | `cargo test -p pond-infra onboarding` | `onboarding_state` reads and writes |
| Routes | `cargo test -p pond-api --test onboarding_integration_test` | The guard, public routes, status shapes |
| Routes | `cargo test -p pond-api --test onboarding_model_routes` and `--test device_routes_before_onboarding` | What a paired phone may reach before completion |
| Real server | `bash scripts/live-test.sh` | Completes onboarding on a server that really started, then checks with the loopback bypass off that the wizard's routes close and a reset reopens them |
| Desktop | `cd pond-desktop && npx vitest run src/components/onboarding` | Step persistence and the personality screen |
