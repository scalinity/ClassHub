-- SPEC §5/§11 — the Canvas ids that turn a re-sync into an update in place.
--
-- An assignment group becomes a grade category and a graded, posted
-- submission becomes a grade item; a Canvas assignment is also what a deadline
-- and its proposal came from. Without the id each sync could only match on a
-- name, and a professor renaming a group or moving a due date would fork a
-- second row beside the first. The id is the join between a deadline, a score
-- and the thing on Canvas.
--
-- Every column is nullable: rows the reader typed or chat recorded carry
-- nothing here, and a hand-made category is claimed by name on the first sync
-- rather than duplicated. Each is unique per class where set, so two rows can
-- never both claim to be the same Canvas object. Items have no class column,
-- and an assignment id is unique across all of Canvas, so theirs is global.
ALTER TABLE grade_categories ADD COLUMN canvas_group_id TEXT;
ALTER TABLE grade_items ADD COLUMN canvas_assignment_id TEXT;
ALTER TABLE deadlines ADD COLUMN canvas_assignment_id TEXT;
ALTER TABLE deadline_proposals ADD COLUMN canvas_assignment_id TEXT;

CREATE UNIQUE INDEX idx_grade_categories_canvas
    ON grade_categories(class_id, canvas_group_id) WHERE canvas_group_id IS NOT NULL;
CREATE UNIQUE INDEX idx_grade_items_canvas
    ON grade_items(canvas_assignment_id) WHERE canvas_assignment_id IS NOT NULL;
CREATE UNIQUE INDEX idx_deadlines_canvas
    ON deadlines(class_id, canvas_assignment_id) WHERE canvas_assignment_id IS NOT NULL;
CREATE UNIQUE INDEX idx_deadline_proposals_canvas
    ON deadline_proposals(class_id, canvas_assignment_id) WHERE canvas_assignment_id IS NOT NULL;
