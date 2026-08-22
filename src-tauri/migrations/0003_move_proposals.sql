-- The drop-to-sort confirm queue (SPEC §9/§10). Chat's propose_file_moves
-- inserts rows here as proposals; the approval UI and the moves themselves
-- land in M9. Nothing in this table has moved anything.
CREATE TABLE move_proposals (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    source_rel_path TEXT NOT NULL,  -- class-relative, exists on disk at proposal time
    dest_rel_path TEXT NOT NULL,    -- class-relative target incl. filename; folders may not exist yet
    reasoning TEXT NOT NULL,
    confidence TEXT,                -- high|medium|low from sort_proposal jobs; NULL for chat proposals
    source TEXT NOT NULL,           -- chat|sort_job
    status TEXT NOT NULL,           -- pending|approved|dismissed
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
