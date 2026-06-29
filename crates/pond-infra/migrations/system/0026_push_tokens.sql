-- Push-notification tokens for paired GOTG mobile devices (#95, Q2-18).
--
-- One current token per device (keyed by device_id). Read by the push path
-- (#99) to reach a backgrounded phone via FCM/APNs. The FK cascade drops a
-- device's token when the device is unregistered, so tokens never outlive
-- the device they belong to.
CREATE TABLE IF NOT EXISTS push_tokens (
    device_id  TEXT PRIMARY KEY,
    token      TEXT NOT NULL,
    platform   TEXT NOT NULL,           -- 'fcm' | 'apns' | 'expo'
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);
