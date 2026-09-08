# M31 — Done from the dashboard

**Read `SPEC.md` in full first**, then this file. §1 (what one real session costs, and how
each course names its divisions), §7.2 (what a sync drops in the inbox; a submission closes
a tracked deadline and nothing reopens one), §8.5 (a division's sources), §10 step 8 (the
Canvas card's week alternative), §11 (deadlines) and §12 (the dashboard strip) are what this
milestone touches.

## Why

Three things, in the order the week brings them.

**The lectures.** Sept 8–10 is the first week every course meets on the facelift, and each
session has a week to file into: Fundamentals' Week 3 (unit 25), Applied's Week 3 feeding
Part I (unit 37; row 7 stays stale until it lands and the guide is rebuilt from three notes
with two decks and the notebook listed), Design Studio's Week 3 (unit 42, its first lecture
and first guide) and Biostatistics' Week 4 (unit 9). Every recording that exists goes through
the form, the digest and the guide, and §1 records the cost of the Part I guide rebuilt from
three notes beside M27's Week 3 guide.

**The week's Canvas files.** M29 gave the Canvas card its alternative: a launch a day after
the last sync drops the week's deck and readings into the inbox as Canvas cards, and each
whose destination names a week offers `File under Week NN`. That was accepted on Design
Studio's `Introduction.pdf` and a fixture; the real files of a week have not been through it.
No sync has run since Sept 3 — the installed app's launch on Sept 7 found no stored session
and never started one — so the first sync of this week is a live one, and what it drops for
all four courses is the measurement: which cards offer the week, which do not, and why. Three
shapes are known before the sync and each may show a gap: Design Studio's Week 3 folder on
Canvas (the folder reading, if Canvas names the folder for the week; nothing if it names it
for the module page that spans weeks 2 and 3), Biostatistics' Module 4 deck (the module
reading), and Applied's Week 3 deck and notebook (its Week 2 notebook came loose — no Canvas
folder — so it got a sort job and a sort card that offers no week, though its name carries
`Week2`). Whatever gap the real files show is closed here, in the explicit shape M28's and
M29's readings have.

**A deadline done from the dashboard.** The `Due in the next 7 days` strip shows each open
deadline as a static chip washed in its class colour, and the only place to complete one is
the checkbox on the class's Deadlines tab, which calls `set_deadline_status`. The week's
first deadline — Fundamentals' Homework 1, Sept 7 — is the concrete moment: read on the
dashboard, done from the workspace, two views for one click. The chip's click marks its
deadline done through the same command: the same `ui.set_deadline_status` audit row, the
same `from Canvas` semantics (a tracked deadline done by hand stays done, and the sync never
reopens one). A done deadline leaves the strip, so the chip needs a cheap way back rather
than a confirm: it stays in the strip as done, clickable back to open, until the dashboard
is next opened. The chip and the tab's row read one query the backend's `deadlines` push
invalidates, so they agree the moment either changes.

Not built: a confirm step, a timer, a new command, a new audit action, any change to what
the sync proposes for a file Canvas has placed, and any reading of a Canvas page's prose.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code
as it stands:

- What each Structure row offers today: `unit_context` for units 25, 42 and 9 (expected:
  refused — nothing filed, nothing distilled — so the rows offer nothing); for unit 40
  (expected: one entry, `Introduction.pdf` under its Week 1 folder, no note, so the row
  offers a guide), and whether that guide is worth building from one deck with no lecture
  behind it (expected: no — the budget is the four guides with lectures); for unit 37
  (expected: five entries, two notes, row 7 stale) and unit 8 (expected: seven entries,
  one note, row 9 stale).
- What the queue holds for every class (expected: Design Studio's two project cards, 25
  and 27, its inbox holding the two files; every other inbox empty, no other pending card).
- What `week_alternative` derives for every pending Canvas destination (expected: none —
  `AI Design Project/` carries no week), and for the three shapes the week may bring:
  `Week 3 - HiPerGator and NaviGator II/<name>` on Design Studio (expected: week 3, under
  the folder's own name inside `Weeks/Week 03 — HiPerGator and NaviGator II/`) against a
  `Module 2/<name>` folder there (expected: none), `Slides/Biostatistics_Module4_Slides_class.pptx`
  on Biostatistics (expected: week 4 by the module reading) and `Slides/CAI6734_Week3_….pdf`
  on Applied (expected: week 3 by the week word, into `Weeks/Week 03/`); and what a loose
  file gets (expected: no card at all — a sort job, whose card carries no alternative —
  though `named_week_reading` reads its name).
- Every course's week slots and the form's default for its meeting day (expected: as M27
  measured — Sept 8 → Fundamentals week 3, Sept 9 → Design Studio week 3, Sept 10 →
  Biostatistics week 4; Applied asks).
- Which open deadlines fall inside the strip's seven days today, Sept 8 (expected: four —
  Biostatistics' Quiz 1, overdue since Sept 3; Fundamentals' Homework 1, overdue since
  Sept 7; Design Studio's Problem Statement + AI Pitch, Sept 9; Biostatistics' Homework 1,
  Sept 13), and which Canvas tracks (expected: none — no open row carries an assignment
  id; the Canvas proposals for `Homework #1` and the Problem Statement wait unapproved
  under titles of their own).
- What `set_deadline_status` writes and emits (expected: one `UPDATE` of `status`, one
  `ui.set_deadline_status` audit row carrying the id and the status, one `deadlines` hub
  push, which invalidates the deadlines and classes queries — nothing else).
- The date against the lectures: Sept 8 has just begun, so no recording can exist.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — The chip

The frontend-design skill is read first. In `DeadlineStrip.tsx` each chip becomes a button
carrying the tab's own control: a small ring at the chip's edge, the row's checkbox at chip
scale, so the two read as one control in two places. The click calls
`setDeadlineStatus(id, true)` and holds the chip busy until the deadlines query has
refetched, so the chip never lags the click; the chip then reads done — the ring filled with
a check, the title struck and muted, `done` in place of the due label — and stays in the
strip while the dashboard is open, because the strip keeps the ids it completed and shows a
deadline that is open or one of those. A second click calls `setDeadlineStatus(id, false)`
and the chip reads open again. The button's accessible name is the tab's — `Mark <title>
done`, `Reopen <title>` — and the tooltip names the class and, on a tracked row, `from
Canvas`. Nothing under `src-tauri/` changes: the command, its audit row and its push are the
tab's, and both surfaces read `["deadlines"]`, which that push invalidates. A deadline done
anywhere else — the tab, chat, a Canvas submission — leaves the strip as it does today,
since the strip did not complete it.

## Phase 2 — The week's Canvas files

A live sync on the installed build, with the owner at the sign-in window; the sign-in stores
the session, so the Sept 9 and 10 launches sync on their own. For each course: what the sync
staged, where Canvas keeps each file, what each card offers, and what any loose file got —
read off the queue, the `canvas.staged_file` audit rows and the sync report. Then the gap
the real files show is closed, in the shape M28's and M29's readings have: a card a click
approves, never a move and never a sort over a file Canvas placed, with `cargo test` pinning
it. Cards are approved only where acceptance needs them, since an approved PDF spawns its
extract.

## Phase 3 — The lectures

Where the recordings exist: each is filed through its form with the digest on; the Part I
guide is rebuilt from three notes with two decks and the notebook listed; Fundamentals',
Design Studio's and Biostatistics' Week 4 guides are built from their notes; §1 gets the
cost of the Part I guide beside the Week 3 guide. Where a recording does not exist, the
others are filed alone; where none does, nothing is spent, row 7 stays stale, and the
recording-dependent criteria wait. The Biostatistics Week 3 guide is rebuilt only if the
reader wants its manifest to match what it read.

SPEC §11, §12, §13 and §14 state the design; the notes get the measurements and the gotchas.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with whatever Phase 2 pins.
- Live, on the dev build: a fixture deadline added on a class's Deadlines tab, dated inside
  seven days, shows as a chip; one click marks it done — an `ui.set_deadline_status` audit
  row with status `done` — and the chip reads done and stays; a second click reopens it — a
  second row, `open` — and the tab's row agrees on both without a refresh; a chip marked
  done is gone when the dashboard is next opened; the fixture is deleted after, and no real
  deadline is touched.
- Live, after the sync: the week's Canvas files for all four courses sit in their inboxes
  as cards, each measured for whether it offers its week and why; the gap the real files
  showed is closed — the card that showed it offers `File under Week NN`, or the loose
  file has a card where it had a sort job.
- Where the recordings exist: each session is in its `Weeks/Week NN…` folder with a note
  and a session document; the Part I guide's manifest and prompt name three notes, two
  decks and the notebook; the other guides are built from their notes; each cost is in §1.

## Watch for

- **The budget.** Four digests and four guides, only where the recordings exist; a second
  run of a guide only if the first is wrong; the Week 3 guide rebuilt only if asked. An
  approved PDF costs its extract ($0.65 in M29); a loose Canvas file costs a sort job; no
  chat turn. Completing a deadline costs nothing.
- **A live sync and a recording link need the owner** — the sync at the sign-in window,
  and the four links asked for before the filing phase, whichever exist filed.
- **Never a real deadline.** The chip is verified on a fixture deadline added for the
  purpose and deleted after; Quiz 1 and Homework 1 are the reader's.
- **Never edit source material.** Files reach week folders through the queue; a fixture is
  a `.csv` in an inbox and leaves the tree before the milestone closes.
- **The installed app (ddd156c) runs beside the dev build** on one database; nothing under
  `src-tauri/` changes while a job runs in the dev build — a sync's follow-up sort job and
  an approved PDF's extract included.
- **The chip's done state is the dashboard's, not the row's.** It is the ids the strip
  completed this visit, and the data decides what shows: a chip whose deadline the tab
  reopened reads open, and one whose deadline was deleted is gone.
- **Canvas's placement stays the default.** Whatever Phase 2 builds is a second explicit
  route, or a card in a sort job's place — never a replacement of an observed destination.
