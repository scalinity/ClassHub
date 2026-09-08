# M33 — One click, and undo

**Read `SPEC.md` in full first**, then this file. §6 (the audit row as the guard's exclusion),
§7 (the scan), §7.2 (what a sync does with a placed file and with an assignment), §10 (steps 4,
7 and 8), §11 (deadlines and the syllabus scan's proposals) and §12 (rows, chips and notices)
are what this milestone touches.

## Why

M28 measured filing one week of Biostatistics material at nine Canvas approvals, then four clicks
and six more: nineteen clicks for one week of one course, and "one propose click is several
approve clicks" is recorded as left as it is. Two of the placements behind those clicks are
observations, not inferences: where Canvas keeps a file — the professor put it there, and the
card's confidence is `NULL` for exactly that reason — and a week word in a file's own name, the
reading M27 and M28 built and tested. A model's guess (a sort job's card) is the thing §10's
"no file ever moves without explicit approval" was written to hold, and it keeps holding it.

The same is true of a Canvas assignment's due date: it is the assignment's own field, and the
card between it and the deadline list only costs a click. Twelve of the sixteen cards waiting
today are one recurring live-coding session — worth 20% of Fundamentals — split into twelve.
Deadline dates are stored in two shapes, date-only and date-time, and a date-only row sorts as
midnight, so a homework due "today" reads as overdue at noon. One Biostatistics reading sits in
both the old and the new layout, indexed and extracted twice and read twice by every guide that
lists it. And several of the controls this milestone leans on are hover-revealed at
`opacity-0`: `Write guide`, `Practice exam`, `Distill`, every row edit.

What makes the automatic moves safe is the rule the personal-project philosophy states outright:
cheap undo replaces a confirmation. The audit log already holds the before-state of every note,
deadline and grade edit and the source and destination of every move. One command that reverses
a row, and a notice with `Undo` after every action, is the mechanism; the auto-filing, the batch
approvals and the direct deadlines all stand on it, and M34's shift files nothing without it.

Not built: chat's undo tool (M38), the shift (M34), any automatic approval of a sort job's or
chat's card, any move of a module-numbered file without its card (Design Studio's `Module 2`
spans two weeks and that reading is knowingly wrong there, §10 step 7).

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as it
stands:

- Every audit action in use (28 as of Sept 4) with one payload each: which carry enough to
  reverse. Expected: `sort.move` carries source and destination; `ui.write_note` and
  `chat.write_note` the replaced content; `ui.upsert_deadline` and `ui.set_deadline_status` the
  row before; `ui.save_grade_item` the row before; `syllabus.insert_deadline` the row written;
  `canvas.staged_file` the inbox path. Which do not (expected: `canvas.complete_deadline`, which
  the sync would redo).
- The pending cards by source and destination shape after M31's live sync: how many name
  Canvas's own folder, how many offer a week alternative and by which reading (week word,
  folder, module), how many are sort cards for loose files.
- `deadlines.due_at` by shape (expected: 22 date-only, 17 with a time), and every comparison of
  it in Rust and TypeScript (`grep -n due_at`), noting which sort or test a date-only row as
  midnight.
- The twelve `Live coding session MM/DD` proposals: titles, dates, whether the syllabus scan's
  JSON carried them as one entry or twelve (expected: twelve).
- Duplicate hashes within a class (`GROUP BY class_id, sha256 HAVING count(*) > 1`); expected:
  one pair in Biostatistics, both extracted.
- The `opacity-0` row controls (`grep -n 'opacity-0' src/components/`) — the list, by section.
- The unused dependencies: `grep -rn 'clsx\|tailwind-merge\|class-variance-authority\|radix-ui'
  src/` (expected: `cn()` in `src/lib/utils.ts` only, itself unimported).

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — Undo

`undo_audit(id)` in `lib.rs`, one command, dispatching on the row's `action` to an inverse in the
module that wrote it, each inside one transaction with its own audit row `undo.<action>` naming
the row it reversed:

- `sort.move` and the by-name and Canvas approvals (the same action): the file moves back through
  the same-volume rename with its index row and its extract following, refused by name when the
  source path is now taken.
- `ui.upsert_deadline`, `chat.upsert_deadline`, `ui.delete_deadline`, `chat.delete_deadline`,
  `ui.set_deadline_status`, `chat.complete_deadline`: the row before is restored, or the row
  created is removed. `syllabus.insert_deadline` and the new direct Canvas rows: removed.
- `ui.write_note`, `chat.write_note`: the replaced content is restored — refused when the note's
  current content is not what the save wrote, which the payload records from now on as a hash.
- `ui.save_grade_item`, `ui.save_grade_category`, `chat.add_grade_item`, and the deletes: the row
  before restored or the row created removed.
- `canvas.*` rows are not reversible: a sync would redo them, and §7.2's rule that nothing
  reopens what Canvas closed stands.

A notice at the bottom of the window after any reversible action — from the UI, from chat, and
later from the shift — reading what happened and `Undo`: `Filed 9 files where Canvas keeps
them · Undo`, `Marked Homework 1 done · Undo`. It fades after a while; the last few stay
reachable from the Job Center's panel. An undo writes its own audit row, so the write guard
excludes its move the way it excludes any app move.

## Phase 2 — Filing without a card

**Canvas places, the app files.** In `canvas_sync.rs`, a staged file whose destination Canvas
supplies is moved on the spot, not carded: to `Weeks/<week folder>/<name>` when the file's own
name carries a week the course declares (the reading §10 step 7 takes); under the folder's own
name inside the week folder when Canvas's folder carries the week (step 8's folder reading);
else into Canvas's folder mapped onto the tree's vocabulary. A file whose name reads a *module*
as a week keeps its card with the alternative, as today. A loose file still gets its sort job and
its card. Each move writes `canvas.filed` with the reason and a batch id; the batch is one notice
with one `Undo`, which reverses every row of the batch. An approved-PDF's extract follows the
move as it follows an approval today.

**A by-name click is the move.** `propose_week_filing` from a file's row moves the file, with the
audit row and the notice, instead of writing a card that needs a second click; a folder's row
moves every file under it as one batch, one notice, one `Undo`. The validation is unchanged —
every destination checked before anything moves, a collision names the file and moves nothing —
and the chip on the row reads `Filed under Week 03` for the rest of the visit. Chat's
`propose_file_moves` and the sort job's cards stay cards.

**Approve all.** The Inbox section offers `Approve all N` over its pending cards, each approval
the ordinary move with its audit row, all under one batch id and one `Undo`; a card whose
destination fails validation is left pending and named.

## Phase 3 — The calendar's shapes

**One instant for a due date.** A `due_instant(due_at)` in `deadlines.rs` — a date-only value is
the end of its day — used by every sort, strip window, `overdue` and `next deadline` computation
in Rust, and its twin in `src/lib/schedule.ts` for the strip and the card; tested on a date-only
row due today at noon (open, due today), at 23:59:30 (open), and the next morning (overdue).

**Canvas assignments are deadlines.** A dated assignment becomes or updates its deadline
directly — source `canvas`, keyed on the assignment id, with the same first-contact claim of a
syllabus row on the same title and day and the same audit row — and no card; an undated one is
a line in the sync report. The pending Canvas cards are resolved as approved by the first sync
that reads their assignments. The proposal queue keeps the syllabus's and, from M35, the
announcements' readings, which are a model's.

**A series is one card.** When pending proposals of one class share a title stem and a weekday
across three or more dates, the queue shows one card — `Live coding session · 12 dates` — with
`Add the series`, which inserts them all under one batch id and one `Undo`; `Skip` dismisses
them all. Detected when the queue is listed, never stored, so a rescan that adds a thirteenth
date joins the card.

**Duplicates.** Migration `0015`: `files.duplicate_of TEXT NULL`. The scan marks the second of
two rows sharing a hash within a class — the canonical copy is the one under `Weeks/`, then the
shallower path, then the older row — and a duplicate is left out of a division's sources, a
guide's manifest and `search_material`'s results (its extract path excluded from the scope), and
its row reads `Duplicate of <path>` with `Show in Finder`. The tree never removes it.

## Phase 4 — The controls you never found

The frontend-design skill is read first. Row actions render at low emphasis — the `meta` role in
the muted ink — instead of `opacity-0`, so `Write guide`, `Practice exam`, `Distill`, `File under
Week NN` and the edit and delete controls are visible on every row. The semester master strip
gets its link in the sticky row; `Structure`, `Deadlines`, `Grades`, `Materials`, `Lectures` and
`Notes` links render only while their sections have content, as `Inbox` and `Notices` already
do. `cn()` and the four dependencies nothing imports (`clsx`, `tailwind-merge`,
`class-variance-authority`, `radix-ui`) are removed, `npm run build`'s type check confirming
nothing needed them.

SPEC §7.2 ("Files download into `_Inbox/` and are proposed…"), §10 step 4 ("No file ever moves
without explicit approval") and steps 7 and 8, §11 (Canvas proposals) and §12 state what is then
true: a file moves without a card where its destination is an observation, every move is audited
and undoable, and a model's reading still asks.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for `due_instant`, the series
  grouping (three dates group, two do not; a differing weekday splits), the duplicate canonical
  choice, the auto-file destination rule (week word, week folder, Canvas folder, module reading
  keeps its card, loose file keeps its sort job) and each undo inverse on fixture rows,
  including the two refusals.
- Live, on the dev build, with fixtures: a `Week03 fixture.csv` added at a class root, then
  `Rescan`, offers `File under Week 03`; one click moves it with a `sort.move` row and a notice;
  `Undo` moves it back with an `undo.sort.move` row; a fixture folder of two `.csv` files moves
  as one batch and reverses as one. The fixtures are removed after.
- After a live sync, where the week brought new files: each file Canvas placed is in its folder
  or its week folder with a `canvas.filed` row, the notice lists the batch, a module-named file
  still has its card, and a loose file its sort card.
- `Approve all` on a class's pending cards moves them and one `Undo` reverses them.
- The twelve live-coding proposals read as one series card; `Add the series` inserts twelve rows
  and `Undo` removes them; the next sync leaves them alone.
- A fixture deadline dated today with no time reads `due today` at any hour of the day and
  `overdue` the next morning, on the strip and the tab.
- The duplicate reading's second row reads `Duplicate of …`, and the Module 1 division's source
  list (`unit_context`, not a run) names the reading once.
- Every row action is visible without hover; the master strip has a nav link; `package.json`
  carries no unused dependency and the app builds.
- SPEC §7.2, §10, §11 and §12 state the design; §14's box is ticked; the notes carry the
  measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** No synthesis job. An auto-filed PDF costs its extract ($0.65 in M29), which an
  approval would have cost anyway; fixtures are `.csv` and extract for nothing. No chat turn.
- **Never edit source material; add fixtures beside it and remove them after.** The live sync's
  moves are real and stay.
- **An undo is a move like any other**: it writes its audit row before the rename, so a job
  running at that moment is not blamed for it; a source path taken since is a refusal by name,
  never an overwrite.
- **A batch is one notice**, and the notice must survive a workspace change — keep it on the
  window, not the section.
- **The direct Canvas deadline changes the queue's meaning**: the badge's `N proposed deadlines`
  and the chat overview count what still asks; the sync report says how many it wrote.
- **Design Studio's module reading** stays a card; the auto rule never reads a module.
- **The installed app runs beside the dev build**; a sync from either files the same batch once,
  because the sync's mutex and the duplicate check already hold.
