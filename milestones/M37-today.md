# M37 — Today

**Read `SPEC.md` in full first**, then this file, and the frontend-design skill before any of it
is built. §8.3 (practice exams), §8.5 (the current division), §11 (grades), §12 (the dashboard,
the workspace's sections, the Job Center) and §16 (the Anki idea, which lands here) are what
this milestone touches. It is mostly `src/`; the backend gains three small tables and four
commands.

## Why

The app knows things and does not say them. The dashboard has no data action beyond M31's chip
and no surface for today; eleven proposals once waited from Aug 27 with no sign on screen while
Homework 1 came due, and the shift's work since M34 is visible only in the Job Center and on the
rows it touched. Fields the front end already fetches are dropped on the floor: a lecture's
contribution summary, a Part's week range, a job's summary, a deadline's notes. Applied's card
shows no division because its Parts carry no dates, though its filed lectures say which week it
is in.

Two things the documents already carry are discarded on read. A practice exam's self-scoring
panel knows which answers were marked right and, since M32, posts them to its parent, which
nothing receives; the next exam cannot re-test what was missed. And every guide and digest
writes a cards sidecar since M32 that nothing indexes — the spec's own future idea, an Anki
export, is ten minutes on the bus, and the phone for free.

Not built: a prose version of Today through the API (a template says it), a spaced-repetition
scheduler beyond three boxes, any change to the generated documents' design.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as it
stands:

- What Today would list right now from existing queries: meetings today with their current
  division and topic; deadlines due within three days; the last `shift_runs` row; announcements
  since the app was last opened (there is no `last_opened_at` yet — note it); pending cards,
  series and sign-in states; recordings `new`; pre-reads ready.
- The fields fetched and not rendered: `Contribution.summary`, `Unit.firstWeek/lastWeek`,
  `JobInfo.summary`, `Deadline.notes` (a tooltip only), `ModelOption.maxTokens`,
  `GradeItem.gradedAt` — confirm each is still fetched.
- Applied's filed lectures and the week each was filed into (expected: Week 1 for Aug 25, Week 2
  for Sept 1, Week 3 for Sept 8 where it exists) — the current-week reading M18 could not make.
- The practice exam's panel: the `postMessage` shape M32 settled, read from a generated exam.
- The cards sidecars on disk per class and their counts; Anki's tab-separated import shape
  (front, back, tags; HTML allowed; one card a line).
- The weighted-grade formula's inputs today (expected: every class's weights, no items).

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — Today

A `Today` block above the class cards, filled from the tables, zero tokens: each meeting today
with its class, time, division and topic and `Before class` where a pre-read exists; what is due
within three days as the strip's chips; what the last shift did (`Overnight: 2 sessions
distilled, 1 guide rebuilt, 6 files filed`) with `Undo` where a batch is still reversible;
announcements since the app was last opened, with their actions; and what waits on a decision —
cards, a series, a Canvas sign-in, a recording that could not be filed, a meeting with no
transcript. A `last_opened_at` setting is written on launch and on focus. Absent lines are
absent; the block is one sentence and one action when there is nothing.

Two more notifications through the plugin M34 added: a deadline due tomorrow, at nine in the
morning, and an announcement that carried an action, after the sync; each a setting.

## Phase 2 — The semester, per class

A strip at the top of the workspace, under the band: each division as a segment in ordinal
order, its meeting date, and its state in form — a transcript filed, a note distilled, a guide
fresh or stale, an assessment due in it — with today's mark. For Applied, whose Parts carry no
dates, the current week is the latest week a lecture was filed into on or before today, read from
`lecture_contributions` and the week folders, never from arithmetic; `units.rs`'s current-division
resolution takes that reading for a course with no dated division, so the card, the band, the
Structure row and the chat overview's `Now:` line all show it. `Structure` rows show a Part's
week range; the Lectures row shows the contribution's summary; the Job Center shows a job's
summary; a deadline's notes render as text under the title.

## Phase 3 — Quizzes with memory

Migration `0018`: `practice_results(id, guide_id REFERENCES guides(id), question INTEGER, topic
TEXT, correct INTEGER, recorded_at)` and `cards(id, class_id, scope, front, back, source,
topic, box INTEGER, due_on TEXT, UNIQUE(class_id, scope, front))`.

The guide viewer listens for the exam's message — origin `null`, the shape M32 settled, the
exam identified by the row the viewer opened — and records the results, replacing the exam's
earlier rows. The Practice exams section shows each exam's last score and its weak topics; the
`Focus on…` field, when left empty, defaults to the scope's weak topics from the last result,
and the prompt receives them as it receives a typed focus. The shift gains a step: two days
before an open deadline of kind `quiz` or `exam`, a practice exam for the division the quiz's
date sits in, focused on the weak topics, once per deadline.

## Phase 4 — Cards

The sidecars are indexed on read: `list_cards(class)` walks `.classhub/cards/` and the corpus
folders, upserts on `(class, scope, front)` keeping box and due date, and drops cards whose
sidecar is gone. `Export for Anki` on the class writes `Study Guides/Cards/<class>.tsv` —
front, back, tags (the class and the scope) — with `Show in Finder`; the owner imports it once
and re-imports as it grows, Anki deduplicating on the front. A `Ten cards` block on the dashboard
under Today shows the ten cards due soonest across the classes, front then back on a click,
`Right` and `Wrong`: right moves the card up a box (due in one, three, then seven days), wrong
sends it to box one for tomorrow and adds its topic to the class's weak topics. Nothing else
schedules anything.

## Phase 5 — Grade projection

Once a class holds at least one graded item, the Grades section adds one line from the formula
in `grades.rs`: the current weighted grade, the points still open, and what the remaining
categories need on average for the next letter up; the card's grade chip shows the open range.
With no item, nothing.

SPEC §8.3 gains the results and the default focus; §8.5 the undated course's reading; §11 the
projection; §12 Today, the strip, the cards and the surfaced fields; §16 loses the Anki line.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for the undated course's
  current week (the latest filed week on or before today; none before the first filing), the
  message parser (a wrong shape is dropped), the card boxes' schedule, the TSV's escaping, and
  the projection on a worked example.
- Live, on the dev build: Today lists today's meetings and a fixture deadline dated within three
  days, the last shift's line, and a fixture card in the inbox as a waiting decision; a
  notification fires for a fixture deadline due tomorrow (the time set to now for the test).
- The strip renders for all four classes; Applied's card, band, Structure row and chat overview
  name the same week, read from its filed lectures.
- A practice exam's self-score, totalled in the viewer, lands in `practice_results`; the exam's
  row shows the score and topics; the next `Practice exam` click's prompt carries them as the
  focus (verified in the job's prompt, no run needed).
- The cards from M32's sidecars list, the TSV opens in Anki (the owner imports it once), and
  `Ten cards` cycles a card through its boxes.
- A fixture grade item yields the projection line and the chip's range; deleting it removes
  them.
- SPEC §8.3, §8.5, §11, §12 and §16 state the design; §14's box is ticked; the notes carry the
  measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** No synthesis; the practice exam's memory is verified on an exam already on
  disk and on the next job's prompt text. No chat turn.
- **The frontend-design skill first**; Today, the strip and the cards are new surfaces and read
  in the app's paper and ink, the type roles and the class washes.
- **The exam's message** comes from a sandboxed frame whose origin is `null`; the viewer accepts
  only the settled shape and only for the exam it opened.
- **Fixtures**: the deadline, the card and the grade item are added for the test and removed
  after; no real deadline is touched.
- **Anki's import** is the owner's one manual step; the TSV is verified against Anki's
  documented shape before that.
- **Notifications at a time of day** need the app running; the tray from M34 is what keeps it
  so.
