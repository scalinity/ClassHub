-- SPEC §8.4 — the ledger of what the professor flagged.
--
-- A digest writes a hints sidecar beside its corpus note: emphasis, exam
-- hints, corrections, where the room got stuck, actions, and the threads the
-- session built on. The rows are what the workspace's Flagged section lists,
-- what every guide, exam and the master receive as a block, and what the chat
-- overview counts. Keyed by the contribution, so a redistill replaces them
-- with the session row and a lecture that leaves the tree takes them along.
CREATE TABLE lecture_hints (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    unit_id INTEGER NOT NULL REFERENCES units(id),
    contribution_id INTEGER NOT NULL
        REFERENCES lecture_contributions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,        -- emphasis|exam_hint|correction|confusion|action|thread
    text TEXT NOT NULL,
    anchor TEXT,               -- an HH:MM the transcript's own anchors resolve
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_hints_class ON lecture_hints(class_id, created_at);
CREATE INDEX idx_hints_contribution ON lecture_hints(contribution_id);

-- SPEC §11 — what a division set out to teach, as the syllabus states it: a
-- JSON array of strings, read by the syllabus scan beside the divisions. A
-- rescan replaces it; a scan that finds none leaves it.
ALTER TABLE units ADD COLUMN objectives TEXT;
