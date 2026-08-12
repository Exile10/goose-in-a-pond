-- Composite index for the sensor range and aggregate queries (#90).
--
-- get_history, get_history_limited and aggregate all match on
-- (device_id, sensor_type) and then range-scan created_at. The two-column index
-- from 0002 stopped at the pair, so SQLite had to examine every row a sensor
-- ever wrote: a one-hour window cost the same as the full 7-day retention
-- window, and MIN/MAX/AVG had nothing to seek on.
CREATE INDEX IF NOT EXISTS idx_sensor_readings_device_type_time
    ON sensor_readings(device_id, sensor_type, created_at);

-- idx_sensor_readings_device_type is a strict prefix of the index above, so it
-- can no longer win a query plan and only taxes every INSERT on the ingest path.
DROP INDEX IF EXISTS idx_sensor_readings_device_type;

-- idx_sensor_readings_created_at stays: prune_sensor_readings sweeps on
-- created_at alone and is not served by an index led by device_id.
