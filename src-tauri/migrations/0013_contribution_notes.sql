-- SPEC §8.5 — a division holds one note per transcript name.
--
-- A corpus note is keyed by its division and its transcript's name, and a
-- Part spans several week folders, so two transcripts named alike in
-- different weeks of one Part derive one note path: the second digest would
-- write over the first note with both rows naming it. The filing refuses
-- that in Rust (`lectures::refuse_held_note`), but a check made by one
-- process is advisory to the other — the installed app and a dev build
-- share this database — so the index makes it structural.
--
-- Rows that already collide are reduced to the earliest, which is the row
-- whose note the rule protects; the later ones named a note that was never
-- theirs, and their transcripts are re-recorded by the next digest under a
-- name of their own.
DELETE FROM lecture_contributions
 WHERE id NOT IN (SELECT MIN(id) FROM lecture_contributions
                   GROUP BY class_id, corpus_rel_path);
CREATE UNIQUE INDEX idx_contributions_note
    ON lecture_contributions(class_id, corpus_rel_path);
