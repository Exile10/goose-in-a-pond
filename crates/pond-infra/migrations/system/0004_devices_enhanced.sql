-- Enhance devices table with runtime tracking fields

ALTER TABLE devices ADD COLUMN device_type  TEXT    NOT NULL DEFAULT 'gotg';
ALTER TABLE devices ADD COLUMN ip_address   TEXT;
ALTER TABLE devices ADD COLUMN capabilities TEXT    NOT NULL DEFAULT '[]';
ALTER TABLE devices ADD COLUMN last_seen    TEXT;
ALTER TABLE devices ADD COLUMN is_online    INTEGER NOT NULL DEFAULT 0;
