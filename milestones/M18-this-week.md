# M18 — This week

**Read `SPEC.md` in full first**, then this file. §5 (`units.starts_on`), §7.2 (structure
precedence), §8.5 (week → Part resolution) and §12 (the card) are what this milestone uses.

## Why

Three of the four courses publish a dated weekly schedule, and the syllabus scan already put it
in `units`: 46 of 49 divisions carry `starts_on`. The only thing the app does with those dates
is print them beside the row. Nothing says "it is Week 3 of Biostatistics" — not the card, not
the workspace, not the chat. The lecture form already resolves a date to a week
(`units::week_slots`, `units::nearest_week`); the same resolution against today answers the
question every week starts with.

## Phase 1 — The current division

`units::current_unit(conn, class_id, today) -> Option<UnitInfo>`: for a course with dated
weeks, the slot whose `starts_on` is the latest on or before today — the slot containing today;
`week_slots` already orders them. For a course declaring Parts, the Part whose range contains
that week (`parse_week_range`, the M14 join). A course with no dated schedule returns `None`
and the UI says nothing, because a guess there would be a week number from arithmetic, which
SPEC §1 forbids.

`db::ClassCard` gains `current_unit: Option<{ name, kind }>`.

## Phase 2 — Where it shows

- **Card:** a mono line under the meeting block, `WEEK 3 · DATA EXPLORATION, PROCESSING, AND
  QUALITY`, in the accent. A "No Class" week shows exactly that — the syllabus's own words.
- **Structure:** the current row carries a small `NOW` tag and the accent dot the card uses
  for in-session. The list does not scroll to it; it marks it.
- **Workspace header:** the eyebrow gains the division after the meeting time, so a workspace
  opens on where the course is.
- **Chat:** `overview_text` prints `Now: Week 3 — …` on each class's block. M19 widens the
  overview further; this line lands here because the card needs the same query.

## Acceptance

- On 2026-09-02 the dashboard reads Week 2 for Fundamentals, Week 2 for Design Studio, Week 2
  for Biostatistics (Week 3 begins Sep 3), and nothing for Applied Generative AI.
- On 2026-11-26 Biostatistics reads `Week 15 — No Class (Thanksgiving Week)`.
- A unit-test table of dates against the seeded syllabi covers a semester boundary, a skipped
  week and a Part course.
- `npx tsc --noEmit` passes.

## Watch for

- **Never derive the week from the date arithmetically.** The units table is the only
  source; a course without dates gets no answer.
- **The card is already dense.** One line, mono, accent. If it fights the deadline line, the
  deadline wins the space.
