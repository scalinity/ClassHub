-- SPEC §9 — ranked search over everything the pipeline writes. Anthropic
-- publishes no embeddings API (§1), so retrieval stays agentic; what changes
-- is the order it comes back in. FTS5 is SQLite's own full-text index, and
-- `bm25()` ranks a hit by how much the term explains the document rather
-- than by where the file sorts on disk.
--
-- The content column is the only indexed one: a class, a path and a kind are
-- carried so a hit can name itself, and matching them is a scan of a table
-- holding one row per document — a few hundred, not a corpus.
CREATE VIRTUAL TABLE material_fts USING fts5(
    class_id UNINDEXED,
    rel_path UNINDEXED,          -- AIBHS-root-relative, as every tool path is
    kind UNINDEXED,              -- extract | corpus | note | guide
    content,
    tokenize = 'porter unicode61'
);

-- What the index holds and what it was made from. The reconcile stats each
-- file under the four searched folders and reads only the ones whose
-- modification time or length moved (SPEC §9), so a search over an unchanged
-- tree opens no write transaction at all; `fts_rowid` is how a changed or
-- vanished file's row is replaced without scanning the virtual table.
--
-- The index is derived: dropping this pair costs a rebuild and never data,
-- which is why the reconcile may read the disk and correct itself rather
-- than depending on every writer in the pipeline to remember it.
CREATE TABLE material_index (
    class_id INTEGER NOT NULL REFERENCES classes(id),
    rel_path TEXT NOT NULL,
    kind TEXT NOT NULL,
    mtime INTEGER NOT NULL,      -- unix seconds, from the file's own metadata
    size INTEGER NOT NULL,       -- bytes on disk
    fts_rowid INTEGER NOT NULL,
    PRIMARY KEY (class_id, rel_path)
) WITHOUT ROWID;
