# M26 — A Tuesday's two lectures

**Read `SPEC.md` in full first**, then this file. §1 (what one real session costs), §7 step 1
(the scan), §7.1 (filing), §8.1 and §8.3 (where a guide is offered), §8.5 (a division's
sources, and what a scan settles) and §12 (the Materials tree) are what this milestone
touches.

## Why

Sept 8 is the first Tuesday since M25 made a week folder's material a division's source,
and two courses meet that day. Applied Generative AI's session is Week 3 and feeds Part I
(unit 37), which already holds two notes, the Week 01 deck and the Week 02 notebook; its
guide (row 7) has read stale since M25 widened its sources, waiting for exactly this
lecture, and a rebuild from three notes is the first guide whose prompt lists the deck and
the notebook rather than leaving the job to find them through `--add-dir`. Fundamentals'
session is Week 3 — Biomedical Data Foundations (unit 25), whose empty folder already sits
under `Weeks/`; its guide is built from one note and whatever the folder holds by then. The
form resolves Fundamentals' week from the course's dates and asks Applied for its week
outright, as M24 built it; nothing about the filing is new, and that is the point — the
path is proved on a day with two lectures rather than one, and §1 records what a guide
costs when its files are listed rather than found.

Two leftovers from M25 sit on the same path and close here. The Materials tree offers
SYNTHESIZE GUIDE and PRACTICE EXAM on every top-level folder, `Weeks` and `Slides` and
`Syllabus` included: that cluster is M5's, from when a top-level folder was a module, and
since M13 a folder is where material sits, not something the course declared (§7.2). A
guide over `Weeks` is a second guide over material the divisions already read; a guide over
`Slides` or `Syllabus` is a guide over a kind of file. The one folder in the tree that is a
unit of material — Biostatistics' `Module 1`, which holds M5's guide — is named the way a
division is, a kind and a number, which is the reading §7.2 already matches folders to
divisions by. And a scan that drops a vanished file's row leaves its extract in the mirror
under `.classhub/extracts/`: nothing points at it, chat's search still walks it, and a
fixture's extract folder from M25 is still there.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code
as it stands:

- What each Add lecture form offers for Sept 8: `week_slots` and `nearest_week` for
  Fundamentals (expected: Week 3 resolved from its date) and for Applied Generative AI
  (expected: no default, Week 03 feeding Part I).
- `files_block` and the corpus block for `unit:37`, and `unit_context` for `unit:25`
  (expected: a refusal, since its folder is empty and no lecture is filed).
- Guide row 7's state (expected: stale), and row 4's (expected: fresh).
- Which top-level folders carry the guide cluster, from the M25 workspace dump, and which
  files in the mirror have no row in the index.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — The tree offers a guide where a folder is a unit of material

`TreeNode` carries whether a folder is named as a division is — `units::label_number` on
its name, the same read §7.2 matches a folder to a unit by — and the Materials tree renders
the guide cluster on a top-level folder only then, or when a guide has already been built
for that folder, so nothing built becomes unreachable. `Module 1` and `Module 2` keep their
cluster; `Weeks`, `Slides`, `Syllabus`, `Coding Material` and `Reading Material` lose it and
keep their file count. The backend's folder scope stays as it is: chat's `trigger_synthesis`
naming a folder is the reader asking, the way SORT BY CONTENT is. Tested: the walk marks a
labelled folder and not a storage one.

## Phase 2 — A vanished file's extract goes with its row

When a scan deletes a vanished path's row, the five mirror entries a source can have
(`extract::MIRROR_SUFFIXES`) are removed after the commit, and the folders they emptied are
pruned up to the mirror root — the way a corpus folder emptied of its last note is. It
holds whether the file was deleted, moved in Finder (the new path is walked as new material
and re-extracted, locally for a transcript), or parked under an app-managed folder (an inbox
file was never indexed, and the sorter's move starts it fresh). A refused settle keeps its
row and its extract. The paths are built only from a plain class-relative path under the
mirror, the guard `corpus_note_path` set. Tested: a deleted deck leaves no extract, sidecar
or emptied mirror folder; a moved transcript leaves none at its old path; a refused settle
leaves both.

## Phase 3 — Live, and what the run teaches

On the dev build, both forms are opened with Sept 8 typed and dumped: Fundamentals' WEEK
reads `Week 03 — Biomedical Data Foundations`, Applied's asks with `Week 03 · Part I: …` on
the list. The Applied Materials tree shows `Slides`, `Syllabus` and `Weeks` without the
cluster; Biostatistics' `Module 1` still shows VIEW GUIDE. A `.csv` fixture dropped into
Fundamentals' empty Week 03 folder is extracted locally on RESCAN, deleted in Finder, and
RESCAN leaves no extract and no folder in the mirror — which also clears M25's leftover
folder, since the fixture's extract lands in it.

Where the Sept 8 recordings exist: each is filed through its form with the digest on, the
Part I guide is rebuilt from three notes with the deck and the notebook listed in its
prompt, and Fundamentals' Week 3 guide is built from its note and whatever sits in its
folder; §1 gets the cost of each digest and each guide, beside M24's Part I guide that found
its files instead of being told. Where one does not exist, the other is filed alone and the
listing verified on the guide it feeds; where neither does, nothing is spent, row 7 stays
stale until the lecture lands, and the recording-dependent criteria wait for it.

SPEC §7, §8.3, §8.5, §12 and §13 state the design; the notes get the measurements and the
gotchas.

## Acceptance

- `cargo test`: the walk marks a folder named as a division is and not `Weeks` or `Slides`;
  a deleted deck leaves no extract, sidecar or emptied mirror folder after a scan; a moved
  transcript leaves none at its old path; a refused settle keeps its row and its extract.
- Live, on the dev build: `Slides`, `Syllabus` and `Weeks` offer no guide and `Module 1`
  still does; a fixture deleted in Finder leaves no extract after RESCAN; the Fundamentals
  form resolves Sept 8 to Week 03 and the Applied form asks, naming Part I.
- Where the Sept 8 recordings exist: both sessions are in `Weeks/Week 03…` of their courses
  with notes and session documents, the Part I guide's manifest and prompt name three notes,
  the deck and the notebook, and Fundamentals' Week 3 guide is built from its note.

## Watch for

- **The budget.** Two digests and two guides, only where the recordings exist; a second run
  of either guide only if the first is wrong. No chat turn is needed.
- **A fixture in a week folder must not be a `.md`**: the Lectures listing takes any
  markdown under `Weeks/` for a transcript. A `.csv` extracts locally and spends nothing.
- **Never edit source material.** The extract rule is verified on a fixture, never on a
  deck or a transcript.
- **The installed app is the Aug 25 build** and runs beside the dev build on one database;
  nothing under `src-tauri/` changes while a job runs. The launch scan of any build enqueues
  whatever is stale in every class.
- **A guide already built for a folder stays on its row**: the gate is the label or the
  guide, never the label alone, so nothing built is orphaned in the tree.
- **The mirror is removed by path shape**: only a plain class-relative path under
  `.classhub/extracts/` is ever removed, and only the entries the pipeline itself writes.
