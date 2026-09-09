-- SPEC §8.3 — a practice exam's self-score, as its panel posts it: one row
-- per question, replaced when the exam is scored again. `scope` is the
-- division, folder or semester the exam was written for, read off its job
-- when the result lands, so the next exam of that scope finds the topics
-- this one missed. The question is the panel's own label (`Q01`).
CREATE TABLE practice_results (
    id INTEGER PRIMARY KEY,
    guide_id INTEGER NOT NULL REFERENCES guides(id) ON DELETE CASCADE,
    scope TEXT,
    question TEXT NOT NULL,
    topic TEXT NOT NULL,
    correct INTEGER NOT NULL,
    recorded_at INTEGER NOT NULL
);
CREATE INDEX idx_practice_results_guide ON practice_results(guide_id);

-- SPEC §12 — the cards every guide and digest writes beside itself (§8.1,
-- §8.4), indexed on read: upserted on (class, scope, front) so the box and
-- the due date survive a rewrite, and dropped when the sidecar is gone.
-- Three boxes, due in one, three, then seven days; a card answered wrong
-- goes back to box one for tomorrow and its topic counts among the class's
-- weak topics until it is answered right.
CREATE TABLE cards (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    scope TEXT NOT NULL,
    front TEXT NOT NULL,
    back TEXT NOT NULL,
    source TEXT,
    topic TEXT,
    box INTEGER NOT NULL DEFAULT 1,
    due_on TEXT,                       -- YYYY-MM-DD; NULL until first shown, which is due now
    wrong_at INTEGER,
    UNIQUE(class_id, scope, front)
);
CREATE INDEX idx_cards_due ON cards(due_on, box);
