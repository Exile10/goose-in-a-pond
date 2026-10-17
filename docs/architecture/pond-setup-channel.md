# The setup channel: BLE discovery and first connection

Phase P5 of [pond networking](pond-networking.md). A pond with no network, no screen and no
keyboard advertises itself over Bluetooth LE. A phone that holds its setup card, or hears its spoken
code, connects over BLE, claims or pairs with it, tells it which Wi-Fi to join, and then carries on
over HTTPS on the address the pond reports. This document is the contract both sides build to: the
pond in `crates/pond-connect`, the phone in goose-on-the-go.

Status: **DESIGNED 2026-10-17.**

## Decisions

| Date | Decision | Why |
|---|---|---|
| 2026-10-17 | **`crates/pond-connect` is the pond's central connection crate.** It holds the setup channel, the BLE peripheral and the Wi-Fi station for every OS, and later the mDNS advertiser and interface inventory (P2). | One place for every way the pond is found or reached. `pond-net` stays the egress leaf it is: radio and D-Bus dependencies must not reach every crate that makes an HTTP call. |
| 2026-10-17 | **The advertisement is a service UUID and a local name only.** | CoreBluetooth lets a peripheral advertise nothing else (no service data), and a macOS pond must look the same as a Linux one. |
| 2026-10-17 | **Messages are JSON, the same shapes as the HTTPS API.** | The phone already speaks them. The channel adds framing and sealing, not a second vocabulary. |
| 2026-10-17 | **App-layer security, no Bluetooth bonding.** CPace authenticates, then ChaCha20-Poly1305 seals. | A headless pond has no way to confirm a passkey, and "Just Works" bonding has no protection against a man in the middle. Neither side shows a system pairing dialog. |
| 2026-10-17 | **macOS runs the BLE peripheral in a helper process.** | Without a Bluetooth usage grant for the responsible app, macOS kills the process that touches CoreBluetooth (measured with the Matter controller, `docs/matter.md`). A helper's death only switches BLE off. |

## Crate layout

```
crates/pond-connect/
  src/setup_channel/   framing, sealing, messages, the per-connection session (any OS, fully testable)
  src/ble/             GATT layout; linux.rs (BlueZ via bluer), macos.rs (helper client), unsupported.rs
  src/wifi/            linux.rs (NetworkManager over zbus), macos.rs (CoreWLAN), unsupported.rs
  src/capabilities.rs  what this host can do, probed at runtime, with the reason when it cannot
```

Policy stays in `pond-core/src/network/`: which secret is valid when (`SetupClaimService`), the
guess and spoken-code limits, the `WifiStation` port, and when to advertise. `pond-connect`
implements the ports and runs the transport. `pond-server` wires it.

## Advertising

- **Service UUID** `185b51bd-e038-4b6c-99b0-4e2379dd362c`, the only one advertised.
- **Local name** `Pond <id>`, where `<id>` is the card's four-hex discriminator in lowercase
  (`Pond c1cd`), matching the card's title. Nothing else: no host name, no household, no address,
  no claim state.
- **When.** While the pond is unclaimed, and while it has no usable network (no IPv4 address on any
  interface that is not loopback or a tunnel), checked every 10 s. Otherwise it does not advertise.
  The setup window after a lost network is P6.
- A phone scans for the service UUID, and when it holds a card, connects to the advertiser whose
  local name carries the card's id.

## GATT

| Characteristic | UUID | Properties | Direction |
|---|---|---|---|
| `rx` | `72172de5-d65e-47ad-857d-1b9dd7623d33` | write (with response) | phone to pond |
| `tx` | `1065b140-d075-431c-b981-7e1a735960cb` | notify | pond to phone |

The phone subscribes to `tx`, then writes requests to `rx`. One connection at a time: a second
central is refused while a session is open. A session closes after 120 s with no request, on an
unreadable frame, or on any sealing failure.

## Framing

A message is one UTF-8 JSON object of at most 16 KiB. On the wire it is a 4-byte big-endian length
followed by the bytes, split across writes (phone) or notifications (pond) of at most `ATT_MTU - 3`
bytes each. The receiver reassembles until it has the length. A length above 16 KiB, or a new
message starting before the last one finished, closes the session.

## Messages

Every request carries `id` (an integer the phone chooses) and `type`. Every response carries the
same `id` and `type`, plus `"ok": true` and fields, or `"ok": false`, `"error": "<code>"` and, where
it applies, `"retry_after": <seconds>`. The error codes are the HTTPS API's, unchanged.

### Before the exchange (in the clear)

| `type` | Request fields | Response fields |
|---|---|---|
| `hello` | none | `protocol: 1`, `id` (discriminator), `claimed`, `pin` (`sha256/...`), `capabilities` (subset of `wifi_scan`, `wifi_join`, `spoken_code`) |
| `spoken_code` | none | as `POST /handshake/spoken-code`: `spoken`, `expires_at`; or `spoken_code_limited`, `spoken_codes_disabled`, `announcer_unavailable`. The code is never sent. |
| `pake_init` | as `POST /handshake/pake/init`: `credential`, `client_id`, `client_type`, `client_version`, `channel_binding` (the `pin` from `hello`), `client_nonce` | as the HTTPS answer: `challenge_id`, `challenge`, `expires_at`, `suite`, `share`; or its refusals |
| `pake_verify` | as `POST /handshake/pake/verify`: `challenge_id`, `device_name`, `share`, `confirm` | `server_proof` (hex) and `sealed`; or `accepted: false` with `rejection_reason` |

The exchange is the HTTPS one exactly: the same channel identifiers (`goose-pair-cpace-v1` or
`goose-setup-cpace-v1`, with the pond's pin and the `client_id`), the same claim latch, guess
throttle, failure budget and spoken-code limiter, shared with HTTPS. There is no TLS, so the pin
in `hello` is what the phone binds. A substituted pin fails confirmation, so **a completed exchange
proves the pin** the phone will later pin HTTPS to. The phone checks `server_proof` before it
opens `sealed`.

### Sealing

Both sides derive two keys from CPace's `ISK` and the session id `sid` (`challenge || client_nonce`):

```
prk   = HKDF-Extract(SHA-512, salt = sid, ikm = ISK)
k_p2c = HKDF-Expand(SHA-512, prk, "goose-setup-channel-v1 pond to phone", 32)
k_c2p = HKDF-Expand(SHA-512, prk, "goose-setup-channel-v1 phone to pond", 32)
```

A sealed payload is ChaCha20-Poly1305 over the inner JSON. The key is the direction's own; the
nonce is four zero bytes then an 8-byte big-endian counter, which starts at 0 in each direction
and goes up by one per sealed message; there is no associated data. It travels as
`{"sealed": "<standard base64 of ciphertext and tag>"}`. `pake_verify`'s `sealed` holds the
`HandshakeResponse` (session and refresh tokens, `expires_at`) under `k_p2c` with counter 0.
Every message after it, in both directions, is a sealed envelope whose inner JSON carries `id`
and `type`. A counter that does not match, or a tag that fails, closes the session.

The pond keeps `ISK` only in memory, for the life of the BLE session. It is never written down or
logged. `pond-infra/tests/fixtures/setup-channel-vectors.json` pins the keys and one sealed message
for a fixed `ISK` and `sid`, so the phone can check itself byte for byte, as it does for CPace.

### After the exchange (sealed)

| `type` | Request fields | Response fields |
|---|---|---|
| `status` | none | `claimed`, `onboarded`, `network` (`state`: `connected`, `connecting` or `disconnected`; `ssid`; `ipv4`), `https_port`, `hostname` |
| `wifi_scan` | none | `networks`: `[{ssid, security, signal, known}]`. `security` is `open`, `wep`, `wpa`, `wpa2`, `wpa3` or `enterprise`; `signal` is 0 to 100; strongest first; one entry per SSID |
| `wifi_join` | `ssid`, `passphrase` (omitted for open networks), `hidden` (optional) | `address` (IPv4), `https_port`, `hostname`; or `wrong_password`, `not_found`, `no_address`, `timeout`, `not_permitted`, `unsupported`, `busy` |

A sealed channel is a paired device's: the exchange that opened it minted the device's tokens. On
a claimed pond, only a spoken code opens one, so changing a claimed pond's Wi-Fi needs someone in
earshot.

**The passphrase** goes from the sealed message straight to the OS network manager, which keeps it
in its own store (NetworkManager's root-only connection files, the macOS keychain). The pond never
writes it down, logs it or returns it, and zeroizes its copy.

After `wifi_join` succeeds, the phone switches to HTTPS on `address:https_port`, pinned to the
proven pin, with the tokens it already holds, and runs owner setup there (P4).

## Per OS

| | Linux (Jetson, Pi) | macOS | Windows |
|---|---|---|---|
| BLE peripheral | BlueZ over D-Bus with `bluer` (needs `libdbus-1-dev` to build; the default D-Bus policy lets the pond's user serve GATT) | CoreBluetooth `CBPeripheralManager` in `pond-server ble-helper`, a child speaking NDJSON on stdin and stdout; the supervisor marks BLE unavailable if it dies | later (WinRT `GattServiceProvider`) |
| Wi-Fi | NetworkManager over D-Bus with `zbus`; needs the polkit grant `giap.sh install` writes, and `giap.sh doctor` checks for | CoreWLAN; scanning names networks only with Location permission on recent macOS, and the capability says so | later |

## Phases

| Phase | Scope | Status |
|---|---|---|
| P5a | `pond-connect`: framing, sealing, messages, session over an in-memory transport; channel keys from the handshake for setup-channel runs; `WifiStation` port and mock; the vectors file | not started |
| P5b | macOS adapters (helper peripheral, CoreWLAN) and server wiring: the Mac development loop with a real phone | not started |
| P5c | Linux adapters (bluer, NetworkManager), the polkit grant, doctor lines; compiled and unit-tested in `jetson-emu.sh box`, then verified on the Jetson last | not started |
| P5d | goose-on-the-go: BLE scan, connect, the exchange over BLE, Wi-Fi picker, then hand over to HTTPS | not started |
