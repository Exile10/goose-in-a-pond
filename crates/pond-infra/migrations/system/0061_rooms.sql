-- Rooms, given real identity instead of each device carrying a free-text label.
--
-- `devices.room` held whatever name was typed into the Configure panel, so
-- "Kitchen" and "Kitchen " were different rooms, renaming one meant rewriting
-- every device row by hand, and "list the rooms" only ever meant "list the
-- distinct words devices happen to hold".
--
-- Every distinct non-blank `devices.room` becomes one row here, each device is
-- pointed at it by id, and the old text column is dropped once copied over.

CREATE TABLE IF NOT EXISTS rooms (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO rooms (id, name)
SELECT lower(hex(randomblob(16))), trimmed
FROM (SELECT DISTINCT trim(room) AS trimmed FROM devices WHERE room IS NOT NULL AND trim(room) <> '');

ALTER TABLE devices ADD COLUMN room_id TEXT REFERENCES rooms(id);

UPDATE devices
SET room_id = (SELECT id FROM rooms WHERE rooms.name = trim(devices.room))
WHERE room IS NOT NULL AND trim(room) <> '';

ALTER TABLE devices DROP COLUMN room;

CREATE INDEX IF NOT EXISTS idx_devices_room_id ON devices(room_id);
