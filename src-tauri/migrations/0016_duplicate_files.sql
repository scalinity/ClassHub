-- SPEC §7 step 1 — a file whose content another row of the class already
-- holds. The scan marks the second copy with the canonical copy's path, and
-- every reader of a scope's sources, the extract pipeline and chat's search
-- leave a marked row out, so one reading is read once. The tree still lists
-- it; the app never removes source material.
ALTER TABLE files ADD COLUMN duplicate_of TEXT;
