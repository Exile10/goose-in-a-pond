# pond-api

The HTTP surface: the Axum router, its handlers, request and response DTOs, and the middleware in
front of them. It depends on `pond-core` ports only; `pond-server` builds the `AppState` and hands
in the adapters.

| File | Holds |
|---|---|
| `src/lib.rs` | `AppState` and the shared DTOs |
| `src/routes.rs` | The router and most handlers; also embeds `pond-desktop/dist` with `include_dir!` and serves it as the dashboard |
| `src/middleware/onboarding_guard.rs` | Blocks protected routes until onboarding completes |
| `src/host_guard.rs` | The loopback dashboard answers only loopback `Host` names and first-party `Origin`s (DNS-rebinding and cross-site defence) |
| `src/insecure_dev.rs` | A plaintext LAN listener for the companion app under Expo Go; debug builds only, opt-in by name |
| `src/connections.rs`, `src/network.rs`, `src/spoken_code.rs` | Setup channel, Wi-Fi and spoken setup-code routes |
| `src/model_acquisition.rs`, `src/model_views.rs`, `src/download_failure.rs` | Model download and catalogue views |
| `src/player.rs`, `src/musickit.rs`, `src/music_choice.rs`, `src/spotify_focus.rs` | Music player routes |
| `src/rides.rs`, `src/uber_accounts.rs` | Ride booking |
| `src/web_search.rs`, `src/voice_sample.rs`, `src/audio_devices.rs` | Feature routes of the same names |

Because the dashboard is embedded at compile time, build the UI (`cd pond-desktop && npm run build`)
before building this crate, or the server ships a placeholder page. `giap.sh doctor` detects that.

The full route list is in [docs/api.md](../../docs/api.md).

## Tests

`tests/` drives the real router over in-memory adapters (`tests/support/`). Some, such as
`stream_handler_parity.rs`, read source as text.

```bash
cargo test -p pond-api
```
