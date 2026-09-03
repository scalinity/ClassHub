# M23 — Divisions that survive a rescan

**Read `SPEC.md` in full first**, then this file. §5 (`units`), §7.2 (the scan's matching
rule) and §8.5 (the week-to-Part join) are what this milestone changes.

## Why

A syllabus rescan matches a division by its name, and a model does not spell a name the same
way twice. On 2026-09-03 a scan of Applied Generative AI's Canvas syllabus page followed the
link to the PDF and reported the three Parts without their `(Weeks 1-8)` suffixes; three new
rows landed beside the three that carry the ranges, and were deleted by hand. The same day a
rescan of Fundamentals inserted `No class (Nov. 24) — Thanksgiving Break` at ordinal 14 and
moved `Week 14` to ordinal 15 — and the Add lecture form reads the week number off the
ordinal, so a lecture on Dec 1 would be filed as Week 15 under the course's own Week 14. A
renamed week would strand its guide, whose scope is `unit:<name>`, and leave its corpus folder
named for a division that no longer exists. The join between a lecture and its Part reads the
range out of the Part's name, so a name without the suffix files no lectures at all.

Three things, then: a division needs an identity a rename cannot break, a Part's week range
belongs in a column rather than in a name, and a rescan has to be an update in place.

## Phase 0 — Measure

Without spending a scan: against a copy of the live database, feed `units::upsert` the three
suffix-less Part names the M22 scan produced and the Fundamentals list with the Thanksgiving
row, and record exactly what happens to `units`, `lecture_contributions`, the `guides` row
scoped `unit:Week 2 — …`, and the corpus path on the contribution row. Record `week_slots` for
Fundamentals as it stands. Both findings go in the notes; nothing is fixed until they are
written down.

## Phase 1 — Identity

`units.number INTEGER NULL`: the course's own number for the division, read from the label the
prompt already requires the name to open with — `Week 7 —` is 7, `Module 3` is 3, `Part II:`
is 2. A row whose name opens with no such label (`Reading Days — No Class`) has none. `upsert`
matches on the Canvas id where there is one, then on `(class, source, kind, number)`, then on
the exact name as it does today; a partial unique index on the label makes the identity real
in the schema. The ordinal is not the identity: it is the position in the list the model
reported, and the Fundamentals rescan shows it moving.

A rescan claims each existing row once. Two entries resolving to one row are reported the way
two entries under one name are today, not written twice. The summary counts what was updated
in place beside what was added.

## Phase 2 — The range as data

`units.first_week` and `units.last_week`: the weeks a division spans, when it spans several.
The scan fills them from the model — the prompt asks for `first_week` and `last_week` on a
division that groups weeks — or from the name when the model gives none; a rescan that states
no range keeps the one held, because a Part without its range files no lectures. `week_slots`
reads the columns and never the name. A week-kind row's week is its number, falling back to
its ordinal only where it has none, and a numbered row wins a week an ordinal-only row would
also claim — Fundamentals' Week 14 keeps week 14, and the Thanksgiving row gets no slot, which
is right, since no class meets.

## Phase 3 — What a rename carries

`guides.scope` and `jobs.scope` name the division by id: `unit:24`, never `unit:<name>`.
Migration `0012` rewrites the rows that exist, and the server carries the label — `label` on
a guide, `scopeLabel` on a job — so nothing on screen changes. A rename inside `upsert` then
has two things to move, both named for the division and both performed after the commit the
way a refile's effects are: the corpus folder under `.classhub/corpus/`, with every
contribution row's `corpus_rel_path` rewritten to match, and the guide file under
`Study Guides/`, with the row's `rel_path` following. A target that already exists refuses
the rename inside the transaction rather than overwriting.

The chat's scope resolver answers `week14` by the number, and accepts `unit:<id>` copied out
of a listing.

## Acceptance

- `cargo test`: a rescan that drops a Part's suffix updates the row in place, keeps its
  range, and the week-to-Part join still resolves every week; a renamed week keeps its id,
  its guide row and its contribution, and its corpus note is at the new path; a rescan that
  inserts an unnumbered row mid-list leaves Week 14 at week 14; a second identical rescan
  writes nothing and reports nothing updated.
- Live, on the migrated database: the Fundamentals Week 2 guide row is scoped `unit:24`, the
  Structure row still offers VIEW GUIDE, the Job Center labels its job by name, and the Add
  lecture form lists Fundamentals' Dec 1 session as Week 14.
- Live, two rescans of the Applied Generative AI syllabus — the Canvas syllabus page, which
  is the scan that forked, then the PDF: three rows before and after each, ids 37–39
  unchanged, ranges 1–8, 9–12 and 13–16 intact, sixteen weeks on the Add lecture form.

## Watch for

- **The backfill runs once, on the version bump.** Existing rows get their number and range
  from their names inside the migration's own transaction; a number another row of the same
  class, source and kind already holds is left empty rather than failing the launch.
- **The installed app is the Aug 25 build** and predates units guides entirely, so no job of
  its own can finish under the old scope shape. The payloads of queued jobs are not rewritten;
  a unit-guide job running in an older build at the moment of upgrade would record its guide
  under the old scope.
- **Two scans is the budget.** Syllabus scans run on the subscription; the Phase 0
  measurement spends none.
