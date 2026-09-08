-- SPEC §8.4 — when a lecture was last read for what was flagged.
--
-- A session whose sidecar was legitimately empty has no ledger rows, and a
-- session distilled before the ledger existed has none either; only a stamp
-- on the contribution tells them apart, and the Lectures row offers a
-- redistill on the second alone.
ALTER TABLE lecture_contributions ADD COLUMN hints_read_at INTEGER;
