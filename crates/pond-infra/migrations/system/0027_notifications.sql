-- Offline notification queue for the push path (#99, Q2-22).
--
-- A targeted notification is enqueued here (undelivered) so a device that is
-- offline receives it when it next opens its notification stream. Rows are
-- stamped `delivered_at` on delivery rather than deleted. FK-cascade so a
-- device's queued notifications don't outlive the device. Broadcasts
-- (target = "broadcast") are ephemeral and NOT queued here.
CREATE TABLE IF NOT EXISTS notifications (
    id           TEXT PRIMARY KEY,
    device_id    TEXT NOT NULL,         -- the notification's target device
    category     TEXT NOT NULL,         -- 'alert' | 'info' | 'action_required'
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    timestamp    TEXT NOT NULL,         -- the notification's own timestamp
    data         TEXT,                  -- optional JSON client payload
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    delivered_at TEXT,
    FOREIGN KEY (device_id) REFERENCES devices(id) ON DELETE CASCADE
);

-- The hot path: undelivered notifications for one device.
CREATE INDEX IF NOT EXISTS idx_notifications_undelivered
    ON notifications (device_id, delivered_at);
