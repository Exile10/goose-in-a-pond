-- Causal edge relationships between memory fragments.
-- Enables graph-based retrieval: BFS from recent memories to discover
-- causally related context that recency alone would miss.

CREATE TABLE IF NOT EXISTS memory_edges (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    from_id   TEXT    NOT NULL,
    to_id     TEXT    NOT NULL,
    relation  TEXT    NOT NULL,       -- 'caused', 'referenced', 'superseded'
    created_at TEXT   NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (from_id) REFERENCES memories(id),
    FOREIGN KEY (to_id)   REFERENCES memories(id)
);

CREATE INDEX idx_memory_edges_from ON memory_edges(from_id);
CREATE INDEX idx_memory_edges_to   ON memory_edges(to_id);
