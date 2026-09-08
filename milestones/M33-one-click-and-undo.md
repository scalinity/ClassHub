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
card between it and the deadline list only costs a click. Twelve of the nineteen cards waiting
today are one recurring live-coding session — worth 20% of Fundamentals — split into twelve.
Deadline dates are stored in two shapes, date-only and date-time, and nothing reads them by one
rule: the UI reads both by the day, so a timed deadline that passed this afternoon still reads
`today`, while Rust sorts the text, so a date-only row lands before a timed row on the same
day — the card's "next deadline" and the tab's order disagree with the strip. One Biostatistics
reading sits in both the old and the new layout, indexed and extracted twice and read twice by
every guide that lists it. And several of the controls this milestone leans on are
hover-revealed at `opacity-0`: `Write guide`, `Practice exam`, `Distill`, every row edit.

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

`undo_audit(ids)` in `lib.rs`, one command over one audit row or a batch of them, dispatching on
each row's `action` to an inverse in the module that wrote it, each inside one transaction with
its own audit row `undo.<action>` naming the row it reversed; a row already reversed, or one no
inverse exists for, is refused by name and the rest of the batch still runs:

- `sort.move` (an approved card, or a by-name click) and `canvas.filed` (a file the sync placed):
  the file moves back through the same-volume rename with its index row and its extract following
  where it stays in the tree, refused by name when the source path is now taken. What the move
  skipped comes back: an approved or sync-placed file returns to the inbox with its card pending
  again, so the reader can redirect it or sort it by content, and a by-name click's row offers
  `File under Week NN` again. A file back in the inbox is unindexed, as any inbox file is, so a
  second filing extracts it again.
- `ui.upsert_deadline`, `chat.upsert_deadline`, `ui.delete_deadline`, `chat.delete_deadline`,
  `ui.set_deadline_status`, `chat.complete_deadline`: the row before is restored, or the row
  created is removed. `syllabus.insert_deadline`: removed, and its card returns to the queue.
- `ui.write_note`, `chat.write_note`: the replaced content is restored — refused when the note's
  current content is not what the save wrote, which the payload records from now on as a hash,
  so a save recorded before the hash existed is refused too.
- `ui.save_grade_item`, `ui.save_grade_category`, `chat.add_grade_item`,
  `chat.upsert_grade_category`, and the deletes: the row before restored or the row created
  removed; a chat category row recorded before it carried its previous state is refused.
- `canvas.*` deadline and grade rows are not reversible: a sync would redo them, and §7.2's rule
  that nothing reopens what Canvas closed stands. `canvas.filed` is the one Canvas row that is,
  because a re-sync matches the returned file by name and size and leaves it to its card.

A notice at the bottom of the window after any reversible action — from the UI, from chat, and
later from the shift — reading what happened and `Undo`: `Filed 9 files where Canvas keeps
them · Undo`, `Marked Homework 1 done · Undo`. The backend emits it with the audit rows it
wrote, so every surface that writes — a command, a chat tool, the sync — reaches the same
notice. It fades after a while; the last few stay reachable from the Job Center's panel. An
undo writes its own audit row carrying the paths it moved, so the write guard excludes its move
the way it excludes any app move.

## Phase 2 — Filing without a card

**Canvas places, the app files.** In `canvas_sync.rs`, a staged file whose destination Canvas
supplies is moved on the spot, not carded: to `Weeks/<week folder>/<name>` when the file's own
name carries a week the course declares (the reading §10 step 7 takes); under the folder's own
name inside the week folder when Canvas's folder carries the week (step 8's folder reading);
else into Canvas's folder mapped onto the tree's vocabulary. A destination a file of that name
already occupies lands beside it at the next free name, as the week alternative does today. A
file whose name reads a *module* as a week keeps its card with the alternative, as today. A
loose file whose name carries a week is filed by it the same way; one whose name carries none
still gets its sort job and its card. The Canvas cards already waiting when the sync runs are
filed by the same rule first, since a re-sync refreshes a waiting card rather than stacking one.
Each move writes `canvas.filed` with the reason and a batch id; the batch is one notice per class
with one `Undo`, which reverses every row of the batch. The sync rescans a class it moved files
for and runs the extract pipeline, so an auto-filed PDF's extract follows the move as it follows
an approval today.

**A by-name click is the move.** `file_under_week` from a file's row moves the file, with the
audit row and the notice, instead of writing a card that needs a second click; a folder's row
moves every file under it as one batch, one notice, one `Undo`. The validation is unchanged —
every destination checked before anything moves, a collision names the file and moves nothing.
The tree refetches on the move, so the row is found under its week folder; the notice carries
the word. Chat's `propose_file_moves` and the sort job's cards stay cards.

**Approve all.** The Inbox section offers `Approve all N` over its pending cards, each approval
the ordinary move with its audit row, all under one batch id and one `Undo`; a card whose
destination fails validation is left pending and named.

## Phase 3 — The calendar's shapes

**One instant for a due date.** A `due_instant(due_at)` in `deadlines.rs` — a date-only value is
the end of its day, a timed one its own time — as one SQL expression every `ORDER BY due_at`
takes (the card's next deadline, the tab's order, the chat overview), and its twin in
`src/lib/schedule.ts` deciding `overdue` for the strip, the tab and the card, so a timed
deadline is overdue once its time has passed and a date-only one only the next morning; tested
on a date-only row due today at noon (open, due today), at 23:59:30 (open), and the next
morning (overdue).

**Canvas assignments are deadlines.** A dated assignment becomes or updates its deadline
directly — source `canvas`, keyed on the assignment id, with the same first-contact claim of a
syllabus row on the same title and day and the same audit row — and no card; an undated one is
a line in the sync report. Titles compare with `#` dropped, so Canvas's `Homework #1` claims
the syllabus's `Homework 1` on Sept 7 rather than standing beside it. The pending Canvas cards
are resolved as approved by the first sync that reads their assignments; a Canvas card declined
earlier stays declined, and the report says so. The proposal queue keeps the syllabus's and,
from M35, the announcements' readings, which are a model's.

**A series is one card.** When pending proposals of one class share a title stem and a weekday
across three or more dates, the queue shows one card — `Live coding session · 12 dates` — with
`Add the series`, which inserts them all under one batch id and one `Undo`; `Skip` dismisses
them all. Detected when the queue is listed, never stored, so a rescan that adds a thirteenth
date joins the card.

**Duplicates.** Migration `0016` (0015 landed in the Post-M32 pass, for `hints_read_at`):
`files.duplicate_of TEXT NULL`, the canonical copy's path. The scan marks the second of two rows
sharing a hash within a class — the canonical copy is the one under `Weeks/`, then the shallower
path, then the older row — and a duplicate is left out of every scope's sources (a folder's, a
division's, the master's), out of the extract pipeline while it is marked, and out of
`search_material`'s results (a hit in its extract is dropped), and its row reads `Duplicate of
<path>` with `Show in Finder` alone. The tree never removes it, and a copy whose canonical
leaves the tree is canonical at the next scan.

## Phase 4 — The controls you never found

The frontend-design skill is read first. Row actions render at low emphasis — the muted text
and icon controls the rows already use — instead of `opacity-0`, so `Write guide`, `Practice
exam`, `Distill`, `File under Week NN` and the edit and delete controls are visible on every
row. The semester master strip gets its link in the sticky row; `Structure`, `Deadlines`,
`Grades`, `Materials`, `Lectures` and `Notes` links render only while their sections have
content, as `Inbox` and `Notices` already do. `cn()` and the four dependencies nothing imports
(`clsx`, `tailwind-merge`, `class-variance-authority`, `radix-ui`) are removed, the gate's
`tsc --noEmit` confirming nothing needed them; `tw-animate-css` is imported by `index.css` and
`shadcn` is the CLI behind `components.json`, so both stay.

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
- The duplicate reading's second row reads `Duplicate of …`, and the semester master's source
  set (`current_manifest` for `master`, not a run) names the reading once; neither copy sits
  under `Weeks/`, so no division ever listed it — the master and the Module 1 folder guide are
  where it counted twice.
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
