-- SPEC §7.2 — what the professor said. A course's announcements, read on
-- every Canvas sync and kept as a record: the workspace lists them newest
-- first and the chat overview carries the latest few. No unread state, no
-- queue — nothing here waits on a decision. The body is Canvas's HTML
-- stripped to text; Canvas HTML is never rendered in the app. The Canvas id
-- is what makes a re-sync an update in place rather than a second row, and a
-- discussion-topic id is global across Canvas, so it is unique on its own.
CREATE TABLE announcements (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    canvas_id TEXT NOT NULL UNIQUE,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    posted_at TEXT NOT NULL      -- local wall-clock ISO, YYYY-MM-DDTHH:MM
);
CREATE INDEX idx_announcements_class ON announcements(class_id, posted_at);
