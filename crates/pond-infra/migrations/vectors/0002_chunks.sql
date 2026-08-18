-- One row can now carry MANY vectors, one per chunk of its text.
--
-- Embedding a whole document as a single vector is the thing chunking exists to
-- fix: a mail subject drowns in 2 KB of body, and the one vector ends up
-- describing the signature block as much as the point. So the identity gains
-- the chunk index, and a row's vectors are `(corpus, row_id, 0..n)`.
--
-- # Offsets, not text
--
-- `chunk_start` / `chunk_len` are a SPAN into the live source row, never a copy
-- of the words. That is the same rule the index has followed since phase A and
-- for the same reason: under WAL a source row and its vector cannot be deleted
-- atomically, so orphans are inevitable, and an orphan holding a snippet is
-- deleted data that survived a deletion promise. An orphan holding an offset
-- resolves to nothing on the join and drops out.
--
-- Retrieval reads the matching passage with `substr(text, chunk_start + 1,
-- chunk_len)` -- SQLite's substr is 1-indexed while the chunker counts from 0,
-- which is the off-by-one this comment exists to stop.
--
-- # Why existing rows are chunk 0 with NULL offsets
--
-- NULL means "this vector is of the whole text", which is exactly what every
-- vector written before this migration is. That keeps them valid and
-- searchable rather than orphaning ~1,300 mail vectors and 28 memory ones on
-- upgrade, and it lets a corpus that never wants chunking (memories are short
-- and whole) keep writing one vector per row without pretending to be chunked.

-- SQLite cannot alter a primary key in place, so the table is rebuilt. The
-- INSERT ... SELECT carries every existing vector across as chunk 0.
CREATE TABLE IF NOT EXISTS vectors_chunked (
    corpus       TEXT NOT NULL,
    row_id       TEXT NOT NULL,

    -- 0 for an unchunked row. Chunks of one row are contiguous from 0.
    chunk_ix     INTEGER NOT NULL DEFAULT 0,
    -- Byte offset and length into the source text. NULL = the whole of it.
    chunk_start  INTEGER,
    chunk_len    INTEGER,

    model_id     TEXT NOT NULL,
    dims         INTEGER NOT NULL,
    vector       BLOB NOT NULL,
    source_rev   TEXT,
    embedded_at  TEXT NOT NULL,

    PRIMARY KEY (corpus, row_id, chunk_ix)
);

INSERT INTO vectors_chunked
    (corpus, row_id, chunk_ix, chunk_start, chunk_len,
     model_id, dims, vector, source_rev, embedded_at)
SELECT corpus, row_id, 0, NULL, NULL,
       model_id, dims, vector, source_rev, embedded_at
FROM vectors;

DROP TABLE vectors;
ALTER TABLE vectors_chunked RENAME TO vectors;

CREATE INDEX IF NOT EXISTS idx_vectors_model ON vectors (model_id);
CREATE INDEX IF NOT EXISTS idx_vectors_corpus_model ON vectors (corpus, model_id);
-- Deleting or re-chunking one row touches every chunk it owns.
CREATE INDEX IF NOT EXISTS idx_vectors_row ON vectors (corpus, row_id);
