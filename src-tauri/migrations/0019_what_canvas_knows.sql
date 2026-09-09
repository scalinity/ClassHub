-- SPEC §7.1 — the recordings found behind the Zoom tool in Canvas's course
-- navigation: one row per recorded Zoom meeting, keyed on Zoom's own id for
-- it, so nothing is captured twice. A row is `new` until the capture files
-- it, `skipped` with the reason where it is not a lecture — a sub-minute
-- test, a day the course does not meet, a date already filed by hand — and
-- `failed` with the reason when the capture could not read it.
CREATE TABLE recordings (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    meeting_id TEXT NOT NULL UNIQUE,   -- Zoom's id for the recorded meeting
    play_url TEXT,                     -- the player link of its video, where one is listed
    recorded_at TEXT NOT NULL,         -- local wall-clock ISO, YYYY-MM-DDTHH:MM
    duration_minutes INTEGER NOT NULL,
    title TEXT NOT NULL,
    rel_path TEXT,                     -- the filed transcript, once captured
    status TEXT NOT NULL,              -- new|filed|skipped|failed
    note TEXT,                         -- why it was skipped or failed, or waits for the form
    seen_at INTEGER NOT NULL
);
CREATE INDEX idx_recordings_class ON recordings(class_id, status, recorded_at);

-- SPEC §7.2 — what the announcement scan read out of a notice: a to-do the
-- reader can tick, or a change the professor announced. Unique per notice
-- and text, so a rescan of an edited notice adds what is new and repeats
-- nothing.
CREATE TABLE announcement_actions (
    id INTEGER PRIMARY KEY,
    announcement_id INTEGER NOT NULL REFERENCES announcements(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,                -- todo|change
    text TEXT NOT NULL,
    done INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    UNIQUE(announcement_id, kind, text)
);

-- When the announcement scan last read the notice; NULL until it has, and
-- cleared again when a sync updates an edited notice in place.
ALTER TABLE announcements ADD COLUMN scanned_at INTEGER;

-- SPEC §7.2 — the assignment's own description from Canvas, stripped to
-- text and capped, refreshed on every sync for a tracked row; what a
-- homework brief is written from.
ALTER TABLE deadlines ADD COLUMN description TEXT;
