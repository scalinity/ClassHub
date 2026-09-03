# M25 — What a week's folder holds

**Read `SPEC.md` in full first**, then this file. §4 (what `Weeks/` is), §7 step 5
(staleness), §8.1 (a unit guide's inputs) and §8.5 (a unit guide's sources, and what a
transcript leaving `Weeks/` costs) are what this milestone changes.

## Why

M24 built the Part I guide from two corpus notes, and the guide cited the Week 01 deck and
the Week 02 notebook throughout — 48 and 37 citations anchor the two lectures, the rest
anchor the deck's slides and the notebook's sections. The job found both through
`--add-dir`, because the prompt's file listing was empty and its manifest named only the
two transcripts. A division's sources are its folder, Canvas attributions and its corpus
notes (§8.5); no division of these four courses has a folder, Canvas attributes nothing,
and the material filed under the week folders a division's weeks name is none of the three.
So a professor's corrected deck leaves the guide reading fresh, and a job is told about the
notes and left to discover the slides. The digest already reads the week folder
(`lecture_context`); the guide does not.

`Weeks/` settles scope for a lecture because the week it was filed under is the division it
belongs to (§4). The same is true of everything filed beside it: the sorter put the Week 2
notebook under `Weeks/Week 02/` because it is Week 2's material, and Week 2 feeds Part I.
The join a transcript already makes — its week folder's number, through `week_slots` — is
the join the rest of the folder should make too.

Two things follow from reading the folder as the division's. A division whose week folder
holds a deck and no distilled lecture can now be built from, so the row has to offer it
(§8.3: the action is offered exactly when a guide could be built). And what the index says
about a week folder has to be true: a transcript deleted in Finder keeps its contribution
row and its note today (`scan_class` never touches `lecture_contributions`), so the
division's guide keeps a source that is not there and the Lectures listing keeps a count.
M16's rule for a transcript that leaves `Weeks/` through a move — the row goes and the note
with it, because no division reads it any more — is the rule for one that leaves the tree.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as
it stands:

- `current_manifest` for `unit:37` (Part I) and for Fundamentals' `unit:24` (Week 2), and
  `files_block` for each — what the prompt would list.
- What guides rows 4 and 7 read as (fresh or stale), and what each reads as once the week
  folders count: row 7's manifest lacks the deck and the notebook the guide read, so it is
  expected to go stale on the widening alone; row 4's week folder holds only the
  transcript, so it is expected to stay fresh.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — The manifest and the listing widen

A unit scope's manifest takes every indexed file under a week folder whose number is one of
the division's weeks — the weeks `week_slots` maps to it, read off the folder name the way
`week_from_rel_path` reads a transcript's, so a week the syllabus later renames still
counts its folder. The union with the folder and the contributions stays, deduplicated.
`files_block` is driven by the manifest, so the deck and the notebook are listed with their
extracts and the contributing transcripts stay listed apart through their notes; the
refusal in `unit_context` names the week folders among what a division builds from.

The Structure row gates SYNTHESIZE GUIDE and PRACTICE EXAM on what the backend would
accept: `UnitInfo` carries how many files a guide would read beside its notes, and the row
offers the actions when that or the distilled count is above zero. Tested: a Part's manifest
covers its own weeks' folders and not the next Part's, a week-numbered course's covers its
own folder alone, a deck's hash change flips the manifest, the listing names the deck and
not the transcript, and the count.

## Phase 2 — A transcript that leaves the tree

`scan_class` finds the paths the index held that the walk did not. For each that a
contribution row or a session row names: a scanned file the index did not hold with the
same hash is the transcript moved in Finder, and it is refiled through `refile_lecture` —
the correction affordance §8.5 describes, without a proposal — with a refusal logged and
the row left as it was; otherwise the transcript is gone, and `lecture_left` clears the
session row with its two documents, the contribution row, and the note. Decided inside the
scan's transaction, each lecture on its own savepoint, and applied after the commit, the way
the sorter applies a refile. The scan reports whether it changed the index — a file added,
changed or removed, a lecture forgotten or refiled — and the command pushes an `index` hub
change so staleness, the Structure counts, the Lectures listing and the guides refetch
without a second scan; today a rescan that adds a file beside a transcript moves no badge
until a job settles. Tested against a real scratch folder: a deleted transcript leaves no
row, note, session row or document; a moved one keeps its note, relocated when the Part
changes; a second scan changes nothing.

## Phase 3 — Live, and what the run teaches

The Sept 8 lecture is next Tuesday's. If a recording exists, it is filed into Week 3 with
the digest on, and the Part I guide is rebuilt from three notes with the deck and the
notebook listed. If it does not, nothing is spent on the subscription: the Part I row reads
stale on the widening and stays so until that lecture lands and the guide is rebuilt — the
moment M24 named for it — and the manifest change is verified on Fundamentals' Week 2
guide, which stays fresh, goes stale when a file is dropped beside its transcript, and is
fresh again when the file is removed. The listing is read off the prompt the probe builds
for `unit:37`. The forget rule is verified on a fixture transcript filed into Week 04 with
a hand-written note and a hand-inserted session row, deleted in Finder and rescanned; the
move rule on the same fixture moved from Week 04 to Week 09.

SPEC §4, §7.2, §8.1 and §8.5 get the widened sources and the two scan rules; §13 gets the
tests; the notes get the measurements and the gotchas.

## Acceptance

- `cargo test`: a Part's manifest names the files under its own weeks' folders and not the
  next Part's, and goes stale when one changes; a week-numbered course's names its own
  folder's; the prompt lists the deck and not the transcript; the row's count matches; a
  transcript deleted in Finder leaves no contribution row, note, session row or document
  after a scan, and one moved in Finder is refiled with its note.
- Live, on the dev build: the Part I manifest names the deck and the notebook; a Fundamentals
  week guide stays fresh, goes stale on a file added beside its transcript, and is fresh
  again on its removal; a fixture transcript deleted in Finder leaves no row after RESCAN;
  the Part I guide rebuilt from three notes lists both files, where the Sept 8 recording
  exists.

## Watch for

- **The budget.** One digest and one Part guide, and only where the Sept 8 recording exists;
  a second guide only if the first is wrong. No chat turn is needed.
- **A fixture in a week folder must not be a `.md`**: the Lectures listing takes any
  markdown under `Weeks/` for a transcript. A `.csv` extracts locally and spends nothing.
- **Never edit the deck.** Staleness is verified by a file added beside it, not by a change
  to source material.
- **The installed app is the Aug 25 build** and runs beside the dev build on one database;
  nothing under `src-tauri/` changes while a job runs. The launch scan of any build enqueues
  whatever is stale in every class.
- **A scan forgets on evidence, not on absence of a folder**: a missing class folder fails
  the scan before anything is compared, and a walk error fails it too, so the only way a
  transcript vanishes from a scan is a delete or a move. The tree is local; no iCloud
  placeholders exist in it.
