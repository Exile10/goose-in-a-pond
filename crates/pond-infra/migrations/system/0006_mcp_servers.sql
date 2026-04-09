-- Persisted external MCP server connections.
-- Each row represents one MCP server GIAP will connect to as a client on startup.

CREATE TABLE IF NOT EXISTS mcp_servers (
    id          TEXT    PRIMARY KEY,
    name        TEXT    NOT NULL UNIQUE,
    kind        TEXT    NOT NULL,           -- 'stdio' | 'streamable_http'
    description TEXT    NOT NULL DEFAULT '',
    command     TEXT,                        -- stdio only
    args        TEXT    NOT NULL DEFAULT '[]',  -- JSON array
    env         TEXT    NOT NULL DEFAULT '{}',  -- JSON object
    uri         TEXT,                        -- http only
    enabled     INTEGER NOT NULL DEFAULT 1,
    created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_mcp_servers_name ON mcp_servers(name);
