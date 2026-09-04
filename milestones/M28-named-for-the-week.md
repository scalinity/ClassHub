# M28 — Named for the week

**Read `SPEC.md` in full first**, then this file. §1 (what one real session costs, and how
each course names its divisions), §7 (the walk), §7.2 (Canvas placement outranks a sort),
§8.5 (a division's sources), §10 (the confirm queue and the by-name proposal) and §12 (the
Materials tree) are what this milestone touches.

## Why

M27 filed Biostatistics' Week 3 lecture and built its guide from the one note, and the job
read six Week 3 files no manifest names: the two readings Canvas filed under `Reading
Material/`, the three files under `Coding Material/Week 3 Coding Material/`, and
`Slides/Biostatistics_Module3_Slides_class.pptx`. A corrected reading leaves the guide
fresh — M24's finding again, on a week-numbered course. The by-name action reaches the two
readings, whose rows offer `FILE UNDER WEEK 03`, and nothing else: the coding material
carries its week on the folder, not on its files, and the deck names Module 3.

Two readings close the gap, both the same explicit, one-click proposal M27 built and never
an automatic sort over files Canvas placed:

- **A folder named for a week files its contents.** The walk marks a folder's week the way
  it marks a file's, except the week folders under `Weeks/`, which are where filing lands.
  The folder's row offers `FILE UNDER WEEK 03` when the course declares the week, the folder
  is not under its week folder and it holds a file; the click proposes one card per file,
  destination `Weeks/<week folder>/<folder name>/<path inside>`, every destination validated
  before any card is written. The folder keeps its name under the week folder — it is the
  professor's grouping, and a division counts a file under its week folder at any depth
  (§8.5) — and the emptied folder stays, as the emptied `Slides/` did in M27.
- **A module is a week where the course says so.** Biostatistics names its decks
  `Biostatistics_ModuleN_Slides_class.pptx`, keeps one Canvas `Module N` page per week whose
  text opens "this week", and declares weeks and no module; Fundamentals' Canvas pages read
  the same way (Module 3 is Biomedical Data Foundations, its Week 3). So a course whose
  divisions are weeks and that declares no numbered module reads `Module N` in a file's
  name as week N — `units::modules_read_as_weeks`, decided from `units` — and the file's
  row offers `FILE UNDER WEEK 03`. The card's reason says which reading it is. It stays a
  click and a card because the reading is not universal: Design Studio's `Module 2` page
  (HiPerGator and NaviGator) may span its weeks 2 and 3, so a wrong reading there costs a
  dismissal, never a move. A folder named for a module is a module's folder — the tree
  already offers a guide over it (§8.3) — and is not filed by this reading.

Not built: a widening of a division's sources to files named for a week, a folder-shaped
proposal (the queue stays one card per file, and approval per file is the ordinary move),
and any reading of a Canvas page's prose.

The Sept 8–10 lectures are the other half: Fundamentals' Week 3 (unit 25), Applied's Week 3
feeding Part I (unit 37; row 7 stays stale until it lands and the guide is rebuilt from
three notes with two decks and the notebook listed), Design Studio's Week 3 (unit 42, its
first lecture and first guide) and Biostatistics' Week 4 (unit 9). Every recording that
exists goes through the form, the digest and the guide, and §1 records the cost of the
Part I guide rebuilt from three notes beside M27's Week 3 guide.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code
as it stands:

- What each Structure row offers today: `unit_context` for units 25, 42 and 9 (expected:
  refused — nothing filed, nothing distilled — so the rows offer nothing), for unit 37
  (expected: five entries, two notes, row 7 stale) and for unit 8 (expected: a one-entry
  manifest, the note alone, row 9 fresh).
- Which files outside `Weeks/` carry a week on a folder or a module in a name, across all
  four trees (expected: Biostatistics only — `Coding Material/Week 3 Coding Material/`
  with three files and `Module 1/Reading Material/Week 1/` with one on a folder; five
  decks naming `Module1`, `Module2` or `Module3` under `Slides/`, `Module 1/Slides/` and
  `Module 2/Slides/`).
- What the Week 3 guide's manifest names (expected: the transcript) against what job 311
  read (expected: the note and six extracts — the two readings, the three coding files and
  the Module 3 deck).
- What `week_in_name` and `label_number` say about each of those names (expected: the
  coding folder reads 3 from both; the deck reads nothing from either; `Module 1` reads 1
  from `label_number` only; `Week 1` reads 1 from both).
- What `week_slots` and `nearest_week` give the four forms for Sept 8, 9 and 10 (expected:
  units 25, 42 and 9, and Applied asking with week 3 feeding Part I).

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — A name's week, read once

`units::module_in_name` reads `Module N` at a word boundary the way `week_in_name` reads a
week, over one shared scan; `units::named_week(name, modules_are_weeks)` is the week a
name carries — the week word first, else the module read as a week where the course reads
it so; `units::modules_read_as_weeks` is true when the course declares week rows and no
numbered module row. The walk takes the course's reading and sets `TreeNode.week` on a
file through it, and on a folder whose name carries a week — never on `Weeks/`'s own
children. Tested: the module shapes (`Biostatistics_Module3_Slides_class.pptx` is 3,
`Module 1` is 1, `Modules 1-3` and `Modular arithmetic.pdf` nothing); a week-numbered
course reads modules as weeks and a Part-numbered one does not; the walk marks the coding
folder's week and not the week folder's own.

## Phase 2 — A folder's files, and a module's week, by proposal

`week_filing` takes a folder: it refuses `Weeks/`, a week folder, an inbox folder and one
holding no file; walks the folder's files by the scanner's rule (no dot-entries, no
symlinks); validates every destination `Weeks/<week folder>/<folder name>/<path inside>`;
then upserts one by-name card per file with a reason naming the folder and the division.
For a file it resolves the week through `named_week` with the course's reading and the
reason says which reading it was. Tested: a folder yields one card per file at the nested
destination and nothing when one destination collides; the week folder itself and an
empty folder are refused; a module-named deck is proposed on a week-numbered course with
the module reason and refused on a Part-numbered one.

## Phase 3 — The folder's row

`DirNode` offers `FILE UNDER WEEK NN` in a hover cluster, the file row's register, when
the folder carries a week the course declares, sits outside its week folder and holds a
file; the tooltip names the file count, the destination and the division. The row reads
`PROPOSED — SEE INBOX` when every file under it holds a pending card, and offers the
click again while some do not. `TreeNode.week`'s comment says what it now carries.

## Phase 4 — Live, and what the run teaches

On the dev build: Biostatistics' tree offers `FILE UNDER WEEK 03` on the coding folder and
on the Module 3 deck beside the two readings; `FILE UNDER WEEK 01` and `02` on the other
module-named decks and on `Module 1/Reading Material/Week 1/`; nothing on `Module 1/`,
`Module 2/`, `Weeks/` or a week folder. The other three trees offer nothing new. The four
Week 3 clicks put six `BY NAME` cards in the queue; each is approved; the six files sit
under `Weeks/Week 03 — Data Exploration, Processing, and Quality/`, the coding files under
their folder, each with its extract moved and no job spawned; `unit_context(3, 8)` names
seven entries and row 9 reads `STALE — RESYNTHESIZE`, which is the truth and is left so —
the guide already read those files, and a rebuild is outside the budget. Weeks 1 and 2's
material stays where Canvas put it: those rows are the reader's, one click at a time.

Where the recordings exist: each is filed through its form with the digest on; the Part I
guide is rebuilt from three notes with two decks and the notebook listed; Fundamentals',
Design Studio's and Biostatistics' Week 4 guides are built from their notes; §1 gets the
cost of the Part I guide beside the Week 3 guide. Where a recording does not exist, the
others are filed alone; where none does, nothing is spent, row 7 stays stale, and the
recording-dependent criteria wait.

SPEC §1, §7, §8.5, §10, §12, §13 and §14 state the design; the notes get the measurements
and the gotchas.

## Acceptance

- `cargo test`: `module_in_name` reads the shapes above; a week-numbered course reads
  modules as weeks and a Part-numbered one does not; the walk marks a folder's week and
  not a week folder's; a folder click yields one card per file at the nested destination,
  all or none; the week folder and an empty folder are refused; a module-named deck is
  proposed with the module reason on a week-numbered course and refused on a Part course.
- Live, on the dev build: the Biostatistics rows offer as Phase 4 says; four clicks yield
  six cards; the six approvals put the files under the Week 3 folder with their extracts,
  the coding files under their own folder, no job spawned; row 9 reads stale and
  `unit_context(3, 8)` names seven entries.
- Where the recordings exist: each session is in its `Weeks/Week NN…` folder with a note
  and a session document; the Part I guide's manifest and prompt name three notes, two
  decks and the notebook; the other guides are built from their notes; each cost is in §1.

## Watch for

- **The budget.** Four digests and four guides, only where the recordings exist; a second
  run of a guide only if the first is wrong; no rebuild of the Week 3 guide. No chat turn.
- **A recording link needs the owner** signed in to the capture window; ask for the four
  links before the filing phase and file whichever exist.
- **Never edit source material.** The moves are verified through the queue, the app's own
  move; fixtures are `.csv`, never `.md` under `Weeks/`.
- **The installed app (a6aa6cc) runs beside the dev build** on one database; nothing under
  `src-tauri/` changes while a job runs. Every extract is current, so the launch scan
  enqueues nothing.
- **The reading is the course's, not the file's.** `modules_read_as_weeks` is decided from
  `units`, so a syllabus rescan that records a numbered module turns the reading off for
  that course; a pending card it already wrote stays a card.
- **All or none.** A folder click validates every destination before writing any card, so
  a collision names the file and writes nothing.
- **A week folder is where filing lands.** `Weeks/Week 03 — …` carries a week in its name
  and must offer nothing; the walk leaves its `week` unset and `week_filing` refuses it.
