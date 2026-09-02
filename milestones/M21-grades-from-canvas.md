# M21 — Grades from Canvas

**Read `SPEC.md` in full first**, then this file. §5 (`grade_categories`, `grade_items`,
`deadlines`), §7.2 (Canvas sync) and §11 (grades) are what this milestone extends.

## Why

The Grades section is manual entry: three categories typed for Biostatistics, no items. Canvas
holds every score the moment it is posted, and the same read says which assignments have been
submitted. Quiz 1 is 2026-09-03; its grade will be typed by hand unless this lands. This is the
one Canvas read that makes a section stop needing anyone at all.

## Phase 0 — What the API returns for these courses

Confirm, in a signed-in session:

- `/courses/:id/assignment_groups` — names, `group_weight`, and whether the course sets
  `apply_assignment_group_weights`;
- `/courses/:id/assignments?include[]=submission` — `submission.score`, `points_possible`,
  `submission.workflow_state`, `submitted_at`, `graded_at`, `posted_at`, `excused`.

Record in SPEC §1 which of the four courses weight their groups, since that decides where
category weights come from.

## Phase 1 — Schema

Migration `0010_canvas_grades.sql`: `grade_categories.canvas_group_id TEXT NULL`,
`grade_items.canvas_assignment_id TEXT NULL`, `deadlines.canvas_assignment_id TEXT NULL` and
the same on `deadline_proposals`, each unique per class where set. The assignment id is the
join between a deadline, a score and the thing on Canvas; storing it is what makes a re-sync an
update.

## Phase 2 — Categories and items

Inside `canvas_sync::sync_class`, after assignments:

- **Categories** upsert by `canvas_group_id`; a hand-made category with the same name
  (case-insensitive, the chat tool's rule) is claimed rather than duplicated. Weight comes from
  `group_weight` when the course applies weights; otherwise the existing weight is left alone
  and the ≠100% warning does its job.
- **Items** upsert by `canvas_assignment_id` for every submission that is graded and posted
  (`score` non-null, `posted_at` set, not excused): name, score, `points_possible` as max,
  `graded_at`. Direct writes with audit rows — grades are reversible through the UI, which is
  the app's rule for skipping a confirm step.
- **Deadlines:** a proposal or deadline created from a Canvas assignment records its id; a
  submission with `submitted_at` marks the matching open deadline done, with an audit row. A
  re-sync never reopens one.

`hub-changed` fires for `grades` and `deadlines`; the card's grade chip updates through
`grades::weighted_grade` unchanged.

## Phase 3 — UI

`VIA CANVAS` on categories and items the sync owns (the deadline row's source tag, reused). The
Settings sync report counts grades recorded and deadlines completed. Editing a Canvas-owned
score by hand is allowed and audited; the next sync overwrites it with Canvas's number, and the
item's tooltip says so.

## Acceptance

- After Quiz 1 is graded, one sync creates the item under Quizzes with the right score and max,
  and the card shows the weighted grade.
- Quiz 1's deadline is done, with an audit row naming the submission.
- A second sync changes nothing: no duplicate categories, items or audit rows.
- A course that weights its groups shows those weights summing to 100.
- `cargo test` covers the graded-and-posted predicate and the category claim;
  `npx tsc --noEmit` passes.

## Watch for

- **A muted or unposted grade is not a grade.** `posted_at` null means the professor has not
  released it; recording it early is the wrong kind of early.
- **`points_possible` can be 0 or null** for ungraded assignments; the CHECK on
  `max_score > 0` rejects them, so skip before insert.
- **Reads only.** Nothing here writes to Canvas; submitting is not this app's job.
