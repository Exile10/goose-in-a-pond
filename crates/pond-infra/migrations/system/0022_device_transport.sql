-- Add addressing and structured capabilities so a controller adapter can reach a device.
-- transport: wire protocol ("http", "mqtt", "ws", "gotg", "matter", ...)
-- address:   protocol-specific endpoint (URL, MQTT broker+topic, hostname:port, …)
-- structured_capabilities: JSON array of DeviceCapability objects (key + kind)
--
-- All columns default to safe empty/null values so existing rows need no manual backfill.

ALTER TABLE devices ADD COLUMN transport              TEXT NOT NULL DEFAULT 'http';
ALTER TABLE devices ADD COLUMN address               TEXT;
ALTER TABLE devices ADD COLUMN structured_capabilities TEXT NOT NULL DEFAULT '[]';
