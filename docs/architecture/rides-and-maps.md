# Rides and maps

How the pond helps a member get somewhere: directions in a maps app, and a ride with Uber or Bolt.
The goal is for GIAP to order and track rides for its members, and to make navigation usable from
Goose On The Go (GOTG). This document covers what the ride companies allow, what is built
(phase 1), and what each later phase needs.

**The pond never books, pays for or tracks a ride on its own in phase 1.** It hands the member a
link. The ride app opens with the trip filled in, and the member confirms and pays there. Every
ride is confirmed by a person in the app that charges them.

Status, 2026-10-06: phase 1 is built behind `ext_travel_enabled`, which ships off. Phases 1b and 2
are not started.

## What the ride companies allow

| | Book and track from another app | Open the app with a trip filled in |
|---|---|---|
| **Uber** | The Ride Requests API. New apps need approval through Uber business development ([Uber developer docs](https://developer.uber.com/docs/riders/introduction)). | Yes. The documented universal link `https://m.uber.com/ul/?action=setPickup` takes pickup and drop-off. A drop-off needs a latitude and longitude ([deep links](https://developer.uber.com/docs/riders/ride-requests/tutorials/deep-links/introduction)). |
| **Bolt** | No public or private API. Booking integrations are for strategic partners only ([Bolt support](https://bolt.eu/en/support/articles/360017256060/)). | No documented link format. Bolt's partner integrations hand over to the app through a deep link, but the format is not published. |

Reverse-engineered APIs and automating the apps' screens are ruled out. They break both companies'
terms, they risk the member's account being banned, and they would put the pond between the member
and their payment method.

## Phase 1: links (built)

One builtin extension, `giap-travel`, registered when `ext_travel_enabled` is on.

| Tool | Does | Leaves the pond |
|---|---|---|
| `get_directions_link` | Google Maps and Apple Maps directions to a place, for driving, walking, transit or cycling. With no origin, the maps app starts from the phone's location. | Nothing. The maps apps resolve free text themselves, so the links are built locally. |
| `get_ride_link` | An Uber link with pickup and drop-off filled in. With no pickup, Uber uses the phone's location (`pickup=my_location`). Asking for Bolt returns an error result that says Bolt has no such link. | The destination name (and a named pickup), sent to the Open-Meteo geocoder for coordinates. |

Code: `crates/pond-mcp-server/src/travel.rs`. The extension name is `TRAVEL_EXTENSION` in
`pond-core`'s `tool_group.rs`. It is registered in `giap_registration.rs` and routed by the direct
dispatcher. It is not on `DIRECT_DISPATCH_ALLOWLIST`.

### Why it ships off

- Two more tool schemas in every turn's prompt, on a window where schemas are already most of it.
- A destination is personal. The geocoder is told the place name, though not who asked or from
  where. `open-meteo.com` is a known public host, so `allowlist` mode lets it through and `offline`
  mode refuses it. Under `offline`, `get_ride_link` still returns an Uber link, with the drop-off
  left for the member to fill in.

### The geocoder's limits

Open-Meteo matches towns, neighbourhoods and well-known places, not street addresses. So:

- The result always names the place that was matched ("Westlands, Kenya (matched from
  "Westlands")"). Uber shows the pin before the member confirms.
- When nothing matches, the drop-off is left empty and the result says so. The link still opens
  Uber.
- A better match needs a geocoder that knows addresses. Google's Geocoding API needs a key and
  billing, and Nominatim has a strict usage policy. Either is a later, opt-in choice.

### Results state facts

Each result ends with "Nothing has been booked", and the ride tool's description says never to
claim a ride is booked. As in `giap-weather`, results state facts and never tell the model what to
say, which a test in `travel.rs` enforces.

### How the link reaches the phone

In phase 1 the link is in the assistant's reply. In GOTG's chat it is tappable. Through a speaker
or the desktop, it isn't useful yet: that is phase 1b.

## Phase 1b: send the link to the member's phone

The pond pushes the link to the phone of the person who asked, as a notification with a button
that opens it. Three pieces, two of them outside `giap-travel`:

1. **Who is speaking.** The tool needs the speaker's `ProfileScope` from the call's engine session
   (`DraftAuthority::actor_for_engine_session`). `RepoDraftAuthority` exists in `pond-core`, but no
   binary installs one, so `init_context_authority` has no caller and `giap-context` refuses every
   call today. Installing it in `pond-server` is a change of its own. It also unblocks
   `giap-context`.
2. **Which phones are theirs.** `BroadcastNotificationSender::send_to_profile` already delivers to
   one member's devices through `DeviceAttribution::devices_for_profile`, and never falls back to
   a broadcast. It has no production caller yet. `pond-mcp-server` holds only the
   `NotificationSender` port, so a small port for profile-addressed delivery is needed.
   A speaker that resolves to `Household` or `Guest` gets the link in the reply, never a push.
3. **GOTG opens it.** The `Notification.data` field carries an action GOTG acts on. Proposed
   contract:

   ```json
   {
     "action": "open_url",
     "kind": "ride",
     "url": "https://m.uber.com/ul/?action=setPickup&pickup=my_location&dropoff[latitude]=...",
     "label": "Open Uber"
   }
   ```

   `kind` is `ride` or `directions`. For directions the pond sends the Google Maps link on Android
   and the Apple Maps link on iOS. GOTG shows the notification with a button labelled `label`, and
   opens `url` with the system handler, which opens the app when it is installed. GOTG must accept
   only `https` URLs on hosts it knows (`m.uber.com`, `www.google.com`, `maps.apple.com`).

   For Bolt, GOTG can at least open the app: package `ee.mtakso.client` on Android, App Store id
   `675033630` on iOS, with the destination shown in the notification body for the member to type.
   This would use `"action": "open_app"`.

GOTG's code is in `jarida-io/goose-on-the-go`. Its half is item 3.

## Phase 2: order and track (needs partnerships)

Real ordering and tracking needs partner access to Uber's Ride Requests API and, separately, a
Bolt partnership. That is a business conversation for Jarida, not an engineering task. With access:

- The member connects their Uber account once through the pond's OAuth flow
  (`crates/pond-api/src/oauth_callback.rs`). The token is stored with the other secrets.
- `get_ride_link` gains a sibling that requests the ride. It shows the fare and needs the member to
  confirm on their phone before anything is requested. A subagent is never given it
  (`groups_denied_to_subagents`), and neither is a guest.
- The pond follows the trip's status and posts updates (driver assigned, arriving, arrived) to the
  member's phone through the phase 1b path.
- Uber's hosts stay `Sensitive` in the egress classification: the calls carry the member's account.

The `music` extension is the model for this: an optional third-party service, with credentials,
whose terms decide what is possible.

## Not planned

- Background location from GOTG. A ride is booked from where the phone is, which the ride app
  already knows. GIAP has no need for a location history.
- Fare comparison across apps. Bolt publishes no fares to compare.
