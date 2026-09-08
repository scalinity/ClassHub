-- SPEC §6 — one run a night, as a constraint and not only as the insert's
-- check: the night's key is unique, so two builds on one database cannot
-- both file a run under it whatever path inserts.
DROP INDEX IF EXISTS shift_runs_night;
CREATE UNIQUE INDEX shift_runs_night ON shift_runs(night);
