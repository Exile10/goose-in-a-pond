-- Optional room grouping for devices ("Living Room", "Kitchen", …)
-- Hub UI groups devices into RoomPills based on this field; NULL means "Home".

ALTER TABLE devices ADD COLUMN room TEXT;
