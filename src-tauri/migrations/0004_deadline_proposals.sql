-- The syllabus-scan confirm queue (SPEC §11). A syllabus_scan job's proposed
-- deadlines wait here between job completion and confirmation — the job's
-- result text is not persisted, and the confirm cards must survive an app
-- restart. Approval inserts into deadlines with source='syllabus'; nothing
-- in this table has created a deadline.
CREATE TABLE deadline_proposals (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    title TEXT NOT NULL,
    kind TEXT NOT NULL,      -- assignment|exam|quiz|project|other
    due_at TEXT NOT NULL,    -- ISO, same shape as deadlines.due_at
    notes TEXT,
    status TEXT NOT NULL,    -- pending|approved|dismissed
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
);
