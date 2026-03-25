# 🦆 Goose In A Pond — Onboarding Guide

> Version 0.1.0 | March 2026

---

## Overview

The onboarding system ensures that protected features of the GIAP API are locked until the device has been fully set up. A new user must complete all onboarding steps before accessing chat, devices, or settings.

Onboarding state is persisted in SQLite and survives server restarts. The system can be driven via two interfaces: the REST API or the interactive CLI wizard.

---

## Onboarding Steps

There are four steps that must be completed in order:

| # | Step | Description |
|---|------|-------------|
| 1 | `VerifyDevice` | Detects and records the local IP address of the host device |
| 2 | `CreateProfile` | Collects a username to identify the primary user |
| 3 | `ConfigurePersonality` | Selects the AI assistant personality (Friendly, Professional, etc.) |
| 4 | `ConnectDevices` | Placeholder step — press Enter to skip (device detection not yet implemented) |
| ✓ | `Completed` | All steps done — protected API routes are now unlocked |

> **Note:** The `ConnectDevices` step is not yet implemented. Device discovery is planned for a future release.

---

## CLI Access

The onboarding wizard can be run directly from the terminal. It walks through each step interactively, prompting for input where needed.

### Start the Wizard

```bash
cargo run -p pond-server -- onboard
```

The wizard picks up from where it left off. If onboarding was previously started, it resumes at the current step. If already completed, it reports completion and exits.

### Example Session

```
🦆 Goose In A Pond — Interactive Onboarding Wizard

Starting onboarding from scratch...

Step: Verify Device
Detected device IP: 192.168.1.42

Step: Create Profile
Enter your username: john doe

Step: Configure Personality
Choose a personality for your assistant:
  1) Friendly
  2) Professional
  3) Casual
  4) Funny
  5) Stoic
Enter the number of your choice: 1
You selected: Friendly

Step: Connect Devices
Press ENTER when all devices are connected...

Onboarding complete!
```

---

## HTTP API Access

Onboarding can also be driven via the REST API. This is the interface the web dashboard uses. Both endpoints are public — they do not require onboarding to be complete before they can be called.

### Start the Server

```bash
cargo run -p pond-server -- serve
# or on a custom port:
cargo run -p pond-server -- serve --port 8080
```

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/api/v1/onboard` | Start onboarding (or report current state if already started) |
| `GET` | `/api/v1/onboard/status` | Return current step and progress |

---

### POST /api/v1/onboard

Starts the onboarding process. Returns one of three responses depending on current state.

**First call — starts onboarding:**
```bash
curl -X POST http://localhost:4000/api/v1/onboard
```

**Called again while in progress:**
```json
{"current_step":"CreateProfile","onboarded":false,"steps_completed":2,"total_steps":4}                                                                                                                                                              
```

**Called after completion:**
```json
{ "status": "already_complete", "message": "Onboarding has already been completed" }
```

---

### GET /api/v1/onboard/status

Returns the current onboarding state including step name, steps completed, and whether onboarding is finished.

```bash
curl http://localhost:4000/api/v1/onboard/status
```

**Not started:**
```json
{ "onboarded": false, "current_step": "not_started", "steps_completed": 0, "total_steps": 4 }
```

**Mid-flow:**
```json
{ "onboarded": false, "current_step": "CreateProfile", "steps_completed": 2, "total_steps": 4 }
```

**Completed:**
```json
{ "onboarded": true, "current_step": "Completed", "steps_completed": 4, "total_steps": 4 }
```

---

## Route Guard

The following routes are locked until onboarding is complete. Any request before completion returns `403 Forbidden`:

| Method | Route |
|--------|-------|
| `POST` | `/api/v1/chat` |
| `GET` | `/api/v1/devices` |
| `POST` | `/api/v1/devices` |
| `GET` | `/api/v1/settings` |
| `PUT` | `/api/v1/settings` |

The following routes are always public regardless of onboarding state:

| Method | Route |
|--------|-------|
| `GET` | `/api/v1/health` |
| `POST` | `/api/v1/handshake` |
| `POST` | `/api/v1/onboard` |
| `GET` | `/api/v1/onboard/status` |
| `GET` | `/api/v1/system/info` |

**403 response shape:**
```json
{ "error": "onboarding_required", "message": "Complete onboarding before using this feature" }
```

---

## State Persistence

Onboarding state is stored in the system SQLite database at:

```
~/.local/share/goose-in-a-pond/pond_system.db
```

State survives server restarts. To reset onboarding and start fresh, delete the database directory:

```bash
rm -rf ~/.local/share/goose-in-a-pond/
```

---

## Running Tests

| Level | Crate | Command | What it tests |
|-------|-------|---------|---------------|
| Unit | `pond-core` | `cargo test -p pond-core` | Service logic: start, advance, status |
| Adapter | `pond-infra` | `cargo test -p pond-infra` | SQLite read/write correctness |
| Integration | `pond-api` | `cargo test -p pond-api --test onboarding_integration_test` | Route guard and HTTP responses |

Run all tests at once:

```bash
cargo test --workspace --exclude goose
```