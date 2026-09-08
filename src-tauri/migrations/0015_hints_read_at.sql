-- SPEC §8.4 — when a lecture was last read for what was flagged.
--
-- A session whose sidecar was legitimately empty has no ledger rows, and a
-- session distilled before the ledger existed has none either; only a stamp
-- on the contribution tells them apart, and the Lectures row offers a
-- redistill on the second alone.
ALTER TABLE lecture_contributions ADD COLUMN hints_read_at INTEGER;
-- A contribution that already holds ledger rows was read: it gets the stamp
-- of its newest row, so the Lectures row does not offer it a redistill.
UPDATE lecture_contributions
   SET hints_read_at = (SELECT MAX(h.created_at) FROM lecture_hints h
                         WHERE h.contribution_id = lecture_contributions.id)
 WHERE id IN (SELECT DISTINCT contribution_id FROM lecture_hints);
