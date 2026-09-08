-- SPEC §6 — the idle shift's runs: one per night, the steps and the jobs it
-- ran as JSON, why it stopped, and the process that ran it, so two builds on
-- one database never both start a night's run.
CREATE TABLE shift_runs (
    id INTEGER PRIMARY KEY,
    night TEXT NOT NULL,                -- YYYY-MM-DD, the day the window opened on
    trigger TEXT NOT NULL,              -- idle|launch|manual
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    steps TEXT NOT NULL DEFAULT '[]',   -- JSON: [{name, state, outcome}]
    jobs TEXT NOT NULL DEFAULT '[]',    -- JSON: [job id]
    stopped_by TEXT,                    -- done|budget|rate_limit|paused|error
    summary TEXT,
    owner_pid INTEGER
);
CREATE INDEX shift_runs_night ON shift_runs(night);
