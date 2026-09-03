# M27 — The week of Sept 8

**Read `SPEC.md` in full first**, then this file. §1 (what one real session costs, and the
four courses' structures), §7.1 (filing, and what a course with no weeks does), §7.2 (what a
scan writes), §8.5 (a division's sources), §10 (the confirm queue) and §12 (the Materials
tree) are what this milestone touches.

## Why

The week of Sept 8 is the first in which all four courses meet with the installed app on
the current tree. M26 left the two Tuesday lectures unfiled because the recordings did not
exist: Applied Generative AI's Week 3 session feeds Part I (unit 37), which holds two notes,
the Week 01 deck and the Week 02 notebook and whose guide (row 7) has read stale since M25;
Fundamentals' Week 3 — Biomedical Data Foundations (unit 25) has an empty folder under
`Weeks/`. Design Studio meets Wednesday: the database holds fifteen dated weeks for it,
written by its syllabus scan, so its Sept 9 session resolves to Week 3 — HiPerGator and
NaviGator II (unit 42) the way Fundamentals' does — §1's table, which says it publishes no
divisions, is what the scan corrected. Biostatistics meets Thursday with dated weeks and
has never filed a lecture; its Week 3 — Data Exploration, Processing, and Quality (unit 8)
is Sept 3, and Sept 10 is Week 4 (unit 9). So every course has a week to file into, and the
question the kickoff asked — what a course with no declared divisions does with a lecture —
has no course today. What it does have is a route: the form sends the session to `_Inbox/`
and a sort job runs over a prompt that tells the sorter to leave it there, which spends a
capture and a job to reach nothing. The decision is that the form says so before the
capture and refuses, naming the syllabus scan as the way out; the inbox route stays for a
course with weeks whose session was left unpicked.

Every recording that exists goes through the form, the digest and the guide — the Part I
guide rebuilt from three notes with the Week 01 deck and the Week 02 notebook listed in its
prompt for the first time, Fundamentals' and Biostatistics' guides from their notes — and §1
records what a guide costs when its files are listed rather than found.

Two leftovers close here. `week_slots` logs `units: … claims week 14 …` on every call —
seven lines per RESCAN — since Post-M25 made the skip say so; a claim is decided when the
divisions are written, so that is where it is said, once, and the listing reads silently.
And Applied's Week 2 deck sits under `Slides/`, where Canvas filed it, outside the week
folders Part I reads: its name carries the week, and the way into Part I is a proposal into
`Weeks/Week 02/` through the confirm queue, asked for from the file's row — not a widening
of a division's sources to files named for a week, and not an automatic sort, which §7.2
keeps off a file Canvas placed.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code
as it stands:

- What each Add lecture form offers for its course's meeting day: `week_slots` and
  `nearest_week` for Fundamentals on Sept 8 (expected: Week 3, unit 25), Applied on Sept 8
  (expected: no default, week 3 feeding Part I), Design Studio on Sept 9 (expected: Week 3,
  unit 42, dated), Biostatistics on Sept 3 and Sept 10 (expected: Weeks 3 and 4).
- What the sorter would propose for a Design Studio transcript in `_Inbox/`: the `{weeks}`
  block its prompt carries (expected: fifteen dated week folders, not the no-schedule line).
- `files_block` and the corpus block for `unit:37` (expected: the deck and the notebook
  listed, two notes) and `unit_context` for `unit:25`, `unit:42` and `unit:8` (expected:
  refused — nothing filed, nothing distilled).
- Guide rows 4 and 7 (expected: fresh, stale).
- Which files outside `Weeks/` carry a week in their name (expected: Applied's deck, and
  Biostatistics' readings under `Reading Material/`).
- How many times the week-claimed line is logged across one workspace open and one RESCAN,
  on a dev build whose stderr goes to a file.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — A course with no weeks is told so before the capture

`resolve_filing` refuses a filing for a course with no week slots when no week was picked,
naming the syllabus scan as the way out; the form's help line says the same when the
schedule is empty and ADD LECTURE stays off. A course with slots and the session left
unpicked still goes to `_Inbox/` for the sorter. Tested: a class with no units is refused
before anything is fetched, and one with units and no week is not.

## Phase 2 — A claimed week is said once, where it is decided

`week_slots` reads silently. `units::week_claims` names each week a row loses to another,
and the two writers of divisions — the syllabus scan's `record_units` and the Canvas
sync's modules step — report it once after their batch, on stderr and in the job summary.
Tested: the Fundamentals shape (a `No class` row at ordinal 14 beside Week 14) yields one
claim from `week_claims` and none from `week_slots`.

## Phase 3 — A file named for a week reaches its week folder by a proposal

`units::week_in_name` reads a week out of a file name — `CAI6734_Week2_…` is 2,
`Week01_…` is 1, `Week 3 2017 Feder…` is 3, `Weekly…` and `class2` nothing — and
`TreeNode.week` carries it on a file. A file whose week the course declares and that is
not already under that week's folder offers `FILE UNDER WEEK 02` on its row; the click
runs `sorter::propose_week_filing`, which resolves the slot, builds the destination
`Weeks/<week folder>/<name>`, validates it as every proposal is, and upserts a pending
proposal with source `by_name` and a reason naming the division that reads the folder.
The queue renders the card with a `BY NAME` chip and the usual APPROVE / MOVE TO…;
approval is the ordinary move, so the extract travels with the file and nothing is
re-extracted. Tested: the tree marks the week on a file and not on a folder; the proposal
lands with the week folder as destination; a file already under its folder and a week the
course lacks are refused.

## Phase 4 — Live, and what the run teaches

On the dev build: each form is opened with its course's meeting day typed and dumped —
Fundamentals reads `Week 03 — Biomedical Data Foundations`, Applied asks with `Week 03 ·
Part I: …`, Design Studio reads `Week 03 — HiPerGator and NaviGator II`, Biostatistics reads
`Week 03 — Data Exploration, Processing, and Quality` for Sept 3 and Week 04 for Sept 10.
The Applied deck's row offers `FILE UNDER WEEK 02`; the card is approved, the deck is under
`Weeks/Week 02/` with its extract moved and no job spawned, and Part I's manifest names
it. A RESCAN logs no claims line.

Where the recordings exist: each is filed through its form with the digest on; the Part I
guide is rebuilt from three notes with the deck (now two decks) and the notebook listed in
its prompt; Fundamentals' Week 3 guide and Biostatistics' guide are built from their notes;
§1 gets the cost of each digest and each guide beside M24's Part I guide that found its
files instead of being told. Design Studio's Week 3 guide is a button away and outside the
budget. Where a recording does not exist, the others are filed alone; where none does,
nothing is spent, row 7 stays stale, and the recording-dependent criteria wait.

SPEC §1, §5, §7.1, §7.2, §8.5, §10, §12 and §13 state the design; the notes get the
measurements and the gotchas.

## Acceptance

- `cargo test`: a filing for a course with no weeks is refused before the fetch, and one
  with weeks and no week picked is not; `week_slots` logs nothing and `week_claims` names
  the Fundamentals shape's one claim; `week_in_name` reads the four shapes above; the walk
  marks a file's week; a by-name proposal lands with the week folder as its destination and
  is refused for a file already under it and for a week the course lacks.
- Live, on the dev build: the four forms resolve as Phase 4 says; the deck's row offers the
  action, the approved card moves it under `Weeks/Week 02/` with its extract and Part I's
  manifest names it; a RESCAN logs no claims line.
- Where the recordings exist: each session is in its `Weeks/Week NN…` folder with a note
  and a session document, the Part I guide's manifest and prompt name three notes, two decks
  and the notebook, and the Fundamentals and Biostatistics guides are built from their
  notes, with each cost in §1.

## Watch for

- **The budget.** Four digests and three guides, only where the recordings exist; a second
  run of a guide only if the first is wrong. No chat turn is needed: the deck's proposal
  comes from its row, not from chat.
- **A recording link needs the owner** signed in to the capture window; ask for the links
  before the filing phase and file whichever exist.
- **Never edit source material.** The by-name move is verified on the deck through the
  queue, which is the app's own move; fixtures are `.csv`, never `.md` under `Weeks/`.
- **The installed app (efc8505) runs beside the dev build** on one database; nothing under
  `src-tauri/` changes while a job runs. Every extract is current, so the launch scan
  enqueues nothing.
- **The action is explicit.** A file Canvas placed is out of an automatic sort's scope
  (§7.2); the by-name proposal exists only because a row was clicked, and a dismissal is
  terminal for the sorter but a click asks again.
- **The claim is reported at the write.** A RESCAN changes no division, so it says nothing;
  a syllabus rescan that keeps the shape says it once more.
