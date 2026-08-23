-- Grade invariants move into the schema: both write paths (grades.rs and the
-- chat tool) validate weight 0–100, score >= 0 and max_score > 0, but a CHECK
-- holds against any future path or a hand-edit — the UI divides by max_score
-- on the assumption. SQLite cannot add a CHECK in place, so both tables
-- rebuild; renaming the originals first keeps the child's foreign key pointed
-- at the old parent until the copies land (parent copied before child, child
-- dropped before parent).

ALTER TABLE grade_items RENAME TO grade_items_old;
ALTER TABLE grade_categories RENAME TO grade_categories_old;

CREATE TABLE grade_categories (
    id INTEGER PRIMARY KEY,
    class_id INTEGER NOT NULL REFERENCES classes(id),
    name TEXT NOT NULL,
    weight REAL NOT NULL CHECK (weight >= 0 AND weight <= 100)
);

INSERT INTO grade_categories (id, class_id, name, weight)
SELECT id, class_id, name, weight FROM grade_categories_old;

CREATE TABLE grade_items (
    id INTEGER PRIMARY KEY,
    category_id INTEGER NOT NULL REFERENCES grade_categories(id),
    name TEXT NOT NULL,
    score REAL NOT NULL CHECK (score >= 0),
    max_score REAL NOT NULL CHECK (max_score > 0),
    graded_at TEXT
);

INSERT INTO grade_items (id, category_id, name, score, max_score, graded_at)
SELECT id, category_id, name, score, max_score, graded_at FROM grade_items_old;

DROP TABLE grade_items_old;
DROP TABLE grade_categories_old;
