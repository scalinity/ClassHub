# M29 — The week on the card

**Read `SPEC.md` in full first**, then this file. §1 (what one real session costs, and how
each course names its divisions), §7.2 (a Canvas placement outranks a sort, and the two
explicit routes off it), §8.5 (a division's sources), §10 (the confirm queue and the by-name
proposal) and §12 (the inbox card) are what this milestone touches.

## Why

Each Canvas sync drops the week's deck under `Slides/` and the readings under `Reading
Material/` — Biostatistics and Applied — or, on Design Studio, under a folder Canvas names
`Week 1 - Introduction`. A division reads a file only under its week folder (§8.5), so a
file that carries its week in its name, or a module the course reads as a week, or that
Canvas keeps in a folder named for the week, reaches the week folder through two
approvals: the Canvas card's, and then the by-name card's from its row or its folder's row.
M28 counted the cost on Biostatistics' Week 3: nine Canvas approvals, then four clicks and
six more. The queue today holds Design Studio's three Canvas cards from an earlier sync,
and one of them — `Introduction.pdf`, filed by Canvas under `Week 1 - Introduction` — is
exactly that shape.

One reading closes the gap, and it is the reading the row already takes, offered a step
earlier:

- **The Canvas card offers the week folder as its alternative destination.** When the queue
  is read, each Canvas card whose destination names a week is given the destination the
  row would offer once the file landed where Canvas put it — the file's own name first,
  through `named_week_reading` with the course's reading (a week word, or a module where
  the course reads modules as weeks), landing at `Weeks/<week folder>/<name>`; else the
  first folder on Canvas's path whose name carries a week, landing under that folder's
  own name inside the week folder, the way a folder's row files its contents. The card
  keeps Canvas's destination and its reason; beside APPROVE it offers `FILE UNDER WEEK
  NN`, the row's own words, and the click approves the card to the alternative through
  the override the folder picker already uses. Nothing is stored: the alternative is
  derived from `units` on every read, so a rescan that records a module turns the module
  reading off, and a renamed week's folder follows its name. No sort runs, nothing moves
  without a click, and Canvas's placement stays the card's default — explicit, as §7.2's
  two routes are.

Not built: a sort over a Canvas file, an alternative on a sort, chat or by-name card, a
stored alternative, an approval that moves more than its one file, and any change to what
a division reads.

The Sept 8–10 lectures are the other half: Fundamentals' Week 3 (unit 25), Applied's Week 3
feeding Part I (unit 37; row 7 stays stale until it lands and the guide is rebuilt from
three notes with two decks and the notebook listed), Design Studio's Week 3 (unit 42, its
first lecture and first guide) and Biostatistics' Week 4 (unit 9). Every recording that
exists goes through the form, the digest and the guide, and §1 records the cost of the
Part I guide rebuilt from three notes beside M27's Week 3 guide.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code
as it stands:

- What each Structure row offers today: `unit_context` for units 25, 42, 9 and 40
  (expected: refused — nothing filed, nothing distilled — so the rows offer nothing), for
  unit 37 (expected: five entries, two notes, row 7 stale) and for unit 8 (expected: seven
  entries — the transcript and the six files M28 filed — one note, row 9 stale).
- What the queue holds for every class (expected: Design Studio's three Canvas cards
  25–27 from the Sept 3 sync, its inbox holding the three files; every other inbox empty,
  no other pending card).
- Which pending or future Canvas destinations name a file whose name carries a week, or a
  module the course reads as one, and which put the week on Canvas's folder instead
  (expected: none of the three pending file names; card 26's folder `Week 1 -
  Introduction`; every Biostatistics reading `Week N …` under `Reading Material/`, every
  Biostatistics deck `Biostatistics_ModuleN_…` under `Slides/` or `Lecture Slides/`, the
  Week 3 coding files under `Coding Material/Week 3 Coding Material/`, and Applied's decks
  `CAI6734_WeekN_…` under `Slides/`; Fundamentals has never staged a Canvas file).
- What `week_in_name`, `module_in_name` and `named_week_reading` say about each
  Biostatistics file Canvas has placed (expected: the readings read their week from both
  the week word and the reading; the decks read nothing from `week_in_name`, their module
  from `module_in_name`, and that module as a week from the reading because Biostatistics
  declares weeks and no module; the coding files and the Posit docx read nothing).
- The date against the lectures: Sept 8, 9 and 10 have not come, so no recording can exist.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — The alternative, derived

`sorter::week_alternative(slots, modules_are_weeks, dest_rel)` is pure: the file's
`named_week_reading` first, then the first segment of the destination's folder path that
`week_in_name` reads a week from; the week's slot from `units::week_slots`, or none when
the course declares no such week; none when the destination already sits under that week
folder. The file case lands at `Weeks/<week folder>/<name>` with the reason `week_filing`
words — "Its name carries Week 3." or the module sentence — and the folder case at
`Weeks/<week folder>/<segment>/<rest of the path>` with a reason naming Canvas's folder,
both closing "Under <week folder>, it counts among the sources of <division>." as the
by-name cards do; the wording is shared, not copied. `sort_state` carries a
`WeekAlternative { week, dest_rel_path, reasoning }` on each Canvas card that has one and
nothing on any other card. Tested: a week-named reading under `Reading Material/` on a
week course; a module-named deck on a week course, with the module reason, and none on a
Part course; a week the course lacks; a destination already under its week folder; a
file under a week-named Canvas folder, and one nested a folder deeper; the file's week
winning over its folder's; and the queue carrying the alternative on a Canvas card and
not on a sort card for a name that carries a week.

## Phase 2 — The card

On a Canvas card with an alternative, `FILE UNDER WEEK NN` sits beside APPROVE in the
action row, styled as the row's action is; its tooltip is the alternative's reason, and
the alternative's route shows under Canvas's so the reader sees both destinations before
choosing. The click approves the card with the alternative as the destination override,
the folder picker's path, so approval is the ordinary move with its audit row and the
file's index row, and the queue refetches. `MoveProposal` gains the optional
`alternative`; the frontend-design skill is read before the card changes.

## Phase 3 — Live, and what the run teaches

On the dev build beside the installed app: Design Studio's card 26 offers `FILE UNDER WEEK
01` toward `Weeks/Week 01 — Introduction, Overview of AI Design Project/Week 1 -
Introduction/Introduction.pdf`, and cards 25 and 27 offer nothing new. A fixture Canvas
card in the sync's own shape — a `.csv` named `Week 4 …` dropped into Biostatistics'
inbox with a `canvas` row toward `Reading Material/` — offers `FILE UNDER WEEK 04`; its
approval puts the file under `Weeks/Week 04 — Probability and Sampling Distributions/`
with an audit row and no job, and unit 9's row offers a guide until the fixture is
removed and the class rescanned. Card 26 is approved to its alternative: Design Studio's
first week folder holds the deck under Canvas's folder name, and unit 40's row offers a
guide, which is not built. Cards 25 and 27 stay the reader's.

Where the recordings exist: each is filed through its form with the digest on; the Part I
guide is rebuilt from three notes with two decks and the notebook listed; Fundamentals',
Design Studio's and Biostatistics' Week 4 guides are built from their notes; §1 gets the
cost of the Part I guide beside the Week 3 guide. Where a recording does not exist, the
others are filed alone; where none does, nothing is spent, row 7 stays stale, and the
recording-dependent criteria wait.

SPEC §7.2, §10, §12, §13 and §14 state the design; the notes get the measurements and the
gotchas.

## Acceptance

- `cargo test`: `week_alternative` reads the shapes above, all or none of them, and
  `sort_state` carries the alternative on a Canvas card alone.
- Live, on the dev build: card 26 offers `FILE UNDER WEEK 01` toward its folder's place
  under the Week 1 folder; the fixture card offers `FILE UNDER WEEK 04` and one approval
  puts its file under the Week 4 folder with an audit row and no job; cards 25 and 27
  offer nothing new; card 26's approval lands its deck under the Week 1 folder.
- Where the recordings exist: each session is in its `Weeks/Week NN…` folder with a note
  and a session document; the Part I guide's manifest and prompt name three notes, two
  decks and the notebook; the other guides are built from their notes; each cost is in §1.

## Watch for

- **The budget.** Four digests and four guides, only where the recordings exist; a second
  run of a guide only if the first is wrong; the Week 3 guide rebuilt only if the reader
  wants its manifest to match what it read. No chat turn.
- **A recording link needs the owner** signed in to the capture window; the four links
  are asked for before the filing phase, and whichever exist are filed.
- **Never edit source material.** The moves are the app's own, through the queue; the
  fixture is a `.csv` in the inbox, never a `.md` under `Weeks/`, and it leaves the tree
  before the milestone closes.
- **The installed app (33ce423) runs beside the dev build** on one database; nothing under
  `src-tauri/` changes while a job runs. A launch a day after the last sync syncs Canvas
  on its own if the stored session is live — what it drops into the inbox arrives as
  Canvas cards, which is the shape this milestone reads.
- **The reading is the course's, not the card's.** Nothing is stored on the row, so the
  alternative can appear and vanish with a rescan; a card approved to it is an ordinary
  approval, and the audit row names the destination it took.
- **Canvas's placement stays the default.** APPROVE still takes Canvas's folder; the
  alternative is a second explicit action, never a replacement of the destination.
