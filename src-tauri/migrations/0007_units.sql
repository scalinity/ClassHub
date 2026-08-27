-- SPEC §5 — a course's own divisions, plus the lecture→unit map M14 fills.
--
-- Both tables land together so M14 needs no schema work of its own.
--
-- `units` replaces "a top-level folder is a module". The four courses genuinely
-- disagree about their own shape — 14 weekly topics, 15 weekly topics, 3 Parts
-- over 16 weeks, and one that publishes nothing — so `kind` and `name` carry
-- the course's own words and the app never shows the reader the word "unit".
-- `source` records where a division came from, which is what keeps a hand-made
-- folder from being mistaken for something the course actually declared.
CREATE TABLE units (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    ordinal INTEGER NOT NULL,
    kind TEXT NOT NULL,        -- module|week|part, as the course names it
    name TEXT NOT NULL,        -- 'Module 3' | 'Week 7 — Tree-Based Models' | 'Part II'
    canvas_id TEXT,            -- set when Canvas is the source
    rel_path TEXT,             -- its folder, when it has one; units need not be folders
    starts_on TEXT,
    ends_on TEXT,
    source TEXT NOT NULL,      -- canvas|syllabus|folder — precedence in that order
    UNIQUE(class_id, name)
);

-- One span of one lecture, mapped to one unit (SPEC §8.5). The join that lets a
-- three-hour lecture feed two guides, each from its own half, without the
-- transcript being stored twice or split. Filled in M14.
CREATE TABLE lecture_contributions (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    unit_id INTEGER NOT NULL REFERENCES units(id),
    rel_path TEXT NOT NULL,          -- the transcript this span is cut from
    start_ms INTEGER NOT NULL,
    end_ms INTEGER NOT NULL,
    start_line INTEGER NOT NULL,     -- resolved from the '## HH:MM' anchors
    end_line INTEGER NOT NULL,
    corpus_rel_path TEXT NOT NULL,   -- the distilled note under .classhub/corpus/
    summary TEXT NOT NULL,
    confidence TEXT NOT NULL,        -- high|medium|low
    status TEXT NOT NULL,            -- applied|pending|dismissed
    created_at INTEGER NOT NULL,
    UNIQUE(class_id, rel_path, unit_id, start_ms)
);

-- Which Canvas course a class resolved to, and when it last synced. Neither is
-- a credential: the mapping is the answer to "did it match the right course",
-- which a sync must be able to show rather than assert, and the stamp is what
-- the Settings screen reads. Nothing here grants any access — the app's reach
-- lives entirely in a webview session that expires (SPEC §7.2).
ALTER TABLE classes ADD COLUMN canvas_course_id TEXT;
ALTER TABLE classes ADD COLUMN canvas_synced_at INTEGER;

-- Proposals now arrive from two places, and approval stamps the deadline with
-- where it came from. Existing rows are all syllabus scans.
ALTER TABLE deadline_proposals ADD COLUMN source TEXT NOT NULL DEFAULT 'syllabus';

CREATE INDEX IF NOT EXISTS idx_units_class ON units(class_id, ordinal);
CREATE INDEX IF NOT EXISTS idx_contributions_unit ON lecture_contributions(unit_id);
CREATE INDEX IF NOT EXISTS idx_contributions_class_path
    ON lecture_contributions(class_id, rel_path);
