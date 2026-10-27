# Notifications

How a notice from the Pond reaches a household member's phone. The routes themselves are in
[api.md](../api.md#devices-matter-and-notifications).

A notification is `Notification {id, target, category, title, body, timestamp, data}`
(`crates/pond-core/src/mcp/ports/notification.rs`). `category` is `alert`, `info` or
`action_required`; `target` is a device id or `"broadcast"`; `data` is an optional JSON object
that may carry an `open` route hint (written only through `OpenScreen`) or an `action` payload
(see [rides and maps](rides-and-maps.md)). Every producer goes through the `NotificationSender`
port, implemented by `BroadcastNotificationSender` (`crates/pond-infra/src/broadcast_notification_sender.rs`),
which caps the title at 200 characters and the body at 2000.

## Delivery classes

Each producer picks how far its notice goes, through one `NotificationSender` method:

| Method | Reaches | Queued and pushed to phones |
|---|---|---|
| `publish_live` | screens connected right now | never: transient status (the model warming up) |
| `broadcast` | the household: connected screens, and every paired phone's own copy | yes |
| `broadcast_except(device)` | every paired phone but `device`, which caused it | yes; no live broadcast copy, since the stream would hand that to `device` too |
| `send` | one device | yes |
| `MemberNotifier::notify_member` | one member's own devices, never a broadcast | yes |

Before 2026-10-27 the warm-up status ("Pond is getting ready", "Pond is ready") went through
`broadcast`, so once broadcasts became durable every restart queued and pushed both to every paired
phone. It is `publish_live` now; a test pins that a warm-up is never queued for a phone.

## How a notice reaches a phone (2026-10-20)

A **targeted** notice (one device, or a member's devices through `MemberNotifier`) is queued in
`notifications` for each device, relayed through the push relay, and published live.

A **household** notice goes through `NotificationSender::broadcast()`. Before 2026-10-20 that only
published to the stream subscribers connected at that moment, so a phone that was asleep never saw
it, and nothing was queued or relayed. Now `broadcast()` also gives every **paired phone**
(`device_type = "gotg"` with an unrevoked session token, `NotificationRecipients`) its own copy,
queued and relayed exactly like a targeted notice. That copy's id is
`"{notification_id}:{device_id}"` (`per_device_notification_id`). Desktops and other clients get the
live broadcast copy only.

**One notice, one id per phone.** The stream skips a broadcast when the device it serves has its
own queued copy (the copy arrives on its own, and stays undelivered until the phone acks it). The
push wake's `notification_id`, the stream and `/pending` therefore all carry the same per-device id,
and a client that dedupes by `id` shows the notice once however it arrived. A copy that fails to
queue has no row, so the stream still sends that phone the broadcast copy.

## Who marks a notice delivered (2026-10-20)

For a **phone** (`device_type = "gotg"`) only `POST /notifications/{id}/ack` does; the stream never
does, on connect or live. A write into a connection the phone abandoned without closing (Wi-Fi and
data turned off) succeeds on the Pond's side for as long as the kernel buffers it, so before this
change such a notice was stamped delivered while the phone was offline, and the push wake's
`/pending` then came back empty. Each new stream connection therefore re-sends every copy the phone
has not acked; the app dedupes by `id` and acks each item after presenting it. The keep-alive does
not close that gap: it too is a write that succeeds into a half-open connection, and only TCP
retransmission timeouts end one. **Any other device** (a desktop, a speaker) has no ack path, so the
stream marks what it sends delivered, as before. `notifications_stream_flushed` logs each connect's
re-send count and whether the device acks.

An app build from before the ack route never acks, so its queued copies stay undelivered: each
reconnect and wake offers them again (it shows each id once per run, by its own dedupe), until the
pruning pass below removes them at 30 days or beyond the newest 500 for that phone. The table stays
bounded either way.

## Retention

The pruning pass (every 6 h) deletes delivered rows after 7 days, every row after 30 days, and
anything beyond a device's newest 500 (`NOTIFICATIONS_*` in `crates/pond-infra/src/pruning.rs`).

## Setting up push

Without a key the Pond still queues every notice, and a phone receives it the next time it opens
the app, but nothing wakes a sleeping phone. To wake Android phones through Firebase Cloud
Messaging:

1. In the Firebase console for the app's project, create a service-account key (Project settings,
   Service accounts, Generate new private key). It downloads as a JSON file.
2. Put it on the Pond at `<data_dir>/secrets/fcm-service-account.json`, or point
   `POND_FCM_KEY_PATH` at it, and make it readable by the Pond's user only:
   `chmod 600 <data_dir>/secrets/fcm-service-account.json`.
3. Restart the Pond. Startup logs `kind="push_relay_fcm"` when the key is in use, and a WARN
   `kind="push_relay_stubbed"` with the `expected_path` when there is no key or it cannot be read.
   Only the path is logged, never the key.

The wake is data-only (`notification_id`, `category`, `wake: "1"`): no title or body passes through
Google. The phone reads the content from `/pending` or the stream.

