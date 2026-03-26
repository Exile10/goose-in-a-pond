-- sensor_readings: time-series sensor data from smart home devices.
-- TTL: 7 days (pruned by background task in pruning.rs).

CREATE TABLE IF NOT EXISTS sensor_readings (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    device_id   TEXT    NOT NULL,
    sensor_type TEXT    NOT NULL,
    value       REAL    NOT NULL,
    unit        TEXT    NOT NULL,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_sensor_readings_device_type
    ON sensor_readings(device_id, sensor_type);

CREATE INDEX IF NOT EXISTS idx_sensor_readings_created_at
    ON sensor_readings(created_at);
