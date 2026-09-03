-- SPEC §5/§7.2 — a division's identity survives a rescan.
--
-- A syllabus row was matched on its name, and a model does not spell a name
-- the same way twice: a rescan that named a Part without its `(Weeks 1-8)`
-- suffix forked a second row beside the first. The identity is now the
-- course's own label — the number its name opens with, read into `number` —
-- together with the source and the kind, matched after the Canvas id and
-- after the exact name. The weeks a division spans are data on the row rather
-- than a suffix a rescan may drop, and the join between a lecture's week and
-- its Part (SPEC §8.5) reads the columns, never the name.
ALTER TABLE units ADD COLUMN number INTEGER;
ALTER TABLE units ADD COLUMN first_week INTEGER;
ALTER TABLE units ADD COLUMN last_week INTEGER;
CREATE UNIQUE INDEX idx_units_label
    ON units(class_id, source, kind, number) WHERE number IS NOT NULL;
-- Existing rows get their number and range from their names in Rust, inside
-- this migration's own transaction (`units::backfill_labels`): SQL reads no
-- Roman numerals.

-- A guide's and a job's scope name the division by id — `unit:24` — so a
-- rename touches neither. A row naming a division no longer in the table
-- keeps the name it has; nothing resolved it before either.
UPDATE guides SET scope = 'unit:' || (
    SELECT u.id FROM units u
    WHERE u.class_id = guides.class_id AND u.name = substr(guides.scope, 6))
WHERE scope LIKE 'unit:%'
  AND EXISTS (SELECT 1 FROM units u
              WHERE u.class_id = guides.class_id AND u.name = substr(guides.scope, 6));
UPDATE jobs SET scope = 'unit:' || (
    SELECT u.id FROM units u
    WHERE u.class_id = jobs.class_id AND u.name = substr(jobs.scope, 6))
WHERE scope LIKE 'unit:%' AND class_id IS NOT NULL
  AND EXISTS (SELECT 1 FROM units u
              WHERE u.class_id = jobs.class_id AND u.name = substr(jobs.scope, 6));
