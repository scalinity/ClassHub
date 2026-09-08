# M32 — Honest guides

**Read `SPEC.md` in full first**, then this file. §6 (the write guard, the stream log), §7 step 5
(staleness), §8.1–§8.5 (what each document is built from and must contain), §9 (the overview),
§11 (the syllabus scan) and §12 (the workspace's sections and rows) are what this milestone
touches. It is the largest of the post-M31 milestones by design: every change here is to what a
guide reads and what it says, and one verification run per document kind covers all of them.

## Why

Three things, and they share one session because they share one set of files.

**A guide is only honest about what it read if the app records it.** `--add-dir` lets a job glob
and read any file under the class folder, so a job told one note has read five files no manifest
names (M24, M27, M28), and a corrected source can leave a guide reading fresh. The same blindness
fails a job for an edit that was not its own: job 307, an extract demoted because a file changed
in the tree while it ran, when the guard's only exclusion is the app's audited moves. The job's
own stream log at `logs/job-<id>.jsonl` records every `Read`, `Grep`, `Write` and `Edit` it made,
and `Bash` is denied to every kind, so that log is a complete account of what the job saw and
touched. Read it at finalize and both questions answer themselves.

**The highest-yield content in the pipeline is buried.** Every digest writes *Said out loud, not on
the slides* — emphasis, exam hints, corrections — and *Questions asked*, and is told not to smooth
over where the room got stuck. That lands as prose in one markdown file per session, and nothing
aggregates it: no section lists what the professor flagged, no guide receives it as a block, the
master has no `{corpus}` placeholder at all and derives its module roster from top-level folder
names (`Weeks, Syllabus` for two courses), the practice exam invents a rubric with no sight of the
grade weights the syllabus scan already stored, and a division guide sees one division. Sept 24 is
Biostatistics' Quiz 2, the first assessment in the program that is actually a test; two days before
it, one list of everything flagged since Quiz 1 is the concrete moment.

**Staleness is a boolean.** `Rewrite · sources changed` says nothing about what arrived, and a
rewrite is a second full read. The manifest diff can say `2 files added, 1 changed` for free, and
the rewrite can mark what is new.

Not built: any automatic run (M34), the cards' surfaces (M37), the chat tools (M38), a redistill
of every existing session (one proves the contract; the rest follow by hand or by the shift).

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as it
stands, and the job logs under the data directory:

- For every `guides` row, the job that wrote it (`jobs.scope` and `kind`, the newest succeeded
  row per scope) and its log: every `tool_use` of `Read` and every path a `Grep` result names,
  filtered to the class folder, diffed against `source_manifest`. Expected: the Biostatistics Week
  3 guide read files its manifest never named (M27 recorded five); the Part I guide's manifest
  names two notes, the deck and the notebook; each master read every extract.
- Job 307's log: its `Write` paths, and whether the path the guard flagged is among them
  (expected: no — the demotion was for the owner's edit).
- The four session documents' markdown twins: the item counts under *Said out loud, not on the
  slides* and *Questions asked* — the ledger's backfill size — and whether any line is already in
  a structured shape.
- What `guides.rs` derives as `{modules}` for the Biostatistics master today (expected: `Module 1,
  Module 2, Reading Material, Slides, Syllabus, Weeks`) beside its 17 `units` rows.
- The practice exams on disk (three) against `guides` rows for them (expected: none), and what
  `list_practice` returns for each.
- The syllabus extracts: whether any states objectives per week (`grep -i 'objective\|outcome'`
  under each class's `.classhub/extracts/`); expected: some do, in prose near each week's row.
- Which rows the delta would describe today: every guide's `stale` flag and the size of its diff
  (expected: the Week 3, Module 1 and Semester Master rows of Biostatistics stale over several
  entries each).
- The digest's write scope in `jobs.rs::allowed_tools`: the corpus folder's `Edit` patterns admit
  a sibling file in the same folder (expected: yes — the pattern is the folder).

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — The job's own log

In `jobs.rs`, a reader over a finished job's JSONL: the paths of every `Read` and the paths a
`Grep` result names (from the `tool_result` block), and separately the paths of every `Write` and
`Edit`, each made class-relative and dropped when it falls outside the class folder. `cargo test`
covers it on fixture lines, including a `Glob` (a listing, not a read — ignored), a `Read` outside
the class folder, and a `Grep` with no hits.

**The manifest records what was read.** At finalize for `module_guide`, `master_guide`, `practice`
and `lecture_digest`, the manifest becomes the union of the listed sources and the read paths,
each carrying its current hash: the index's `sha256` for a `files` row, the hash of the file on
disk for a path the index does not hold (a corpus note, a Canvas page mirror under
`.classhub/extracts/Canvas/`). A listed source the job never opened stays in the manifest — the
job was told about it, and a change to it is still a reason to rebuild — so the union only widens.
The set comparison in `extract.rs` is unchanged; what changes is what goes into the set.

**The guard reads the log.** The write-scope check keeps its fingerprint diff and its audit-row
exclusion, and adds one more: a changed path the job's log shows no `Write` or `Edit` to was not
the job's, and is dropped from the touched list. A path the log does show a write to, outside
`Study Guides/` and `.classhub/`, still fails the job. Tested on a fixture log with a `Write` to
one path and a changed second path.

## Phase 2 — The ledger

Migration `0014`: `lecture_hints(id, class_id, unit_id, contribution_id REFERENCES
lecture_contributions(id), kind TEXT, text TEXT, anchor TEXT NULL, created_at)`, `kind` one of
`emphasis | exam_hint | correction | confusion | action | thread`, `anchor` an `HH:MM` the
transcript's own anchors resolve. And `units.objectives TEXT NULL`, a JSON array of strings, for
Phase 3.

**The digest writes a sidecar.** `lecture_digest.md` gains a fourth output beside the corpus note,
at the derived path `<note path minus .md>.hints.json`: a JSON array of `{kind, text, anchor}`,
one item per point the session document already puts under *Said out loud, not on the slides*,
*Questions asked* (as `confusion` where the room got stuck, `action` where something was assigned
or dated) and, new, three to five `thread` items — what this session built on from earlier
sessions, drawn from `{previous}`, the class's earlier contribution summaries
(`lecture_contributions.summary`, newest first, capped) that `lectures.rs` now renders into the
prompt. An empty array is valid; a missing or malformed file fails the job the way a missing
markdown twin does (§8.4), because the file is the point. The job's `Edit` scope already covers
the corpus folder. `record_session` parses the sidecar, replaces the contribution's rows in one
transaction with the session row, and the note's rename-on-refile (§8.5) carries the sidecar with
it.

**And a cards sidecar**, `<note path minus .md>.cards.json`: `[{front, back, source, topic}]`, one
card per term introduced and per key point that stands alone as a question. `module_guide.md` and
`master_guide.md` write the same shape to `.classhub/cards/<guide file stem>.json`, one card per
self-test question and per glossary term, `source` the citation the entry already carries. Both
finalizers require the file and check it parses; nothing reads the cards until M37, which is why
the shape is settled here, while the prompts are open and one run verifies it.

**The section.** A `Flagged` section in the workspace between `Lectures` and `Practice exams`,
present while the class has a row: newest session first, each item its kind as a chip
(`Exam hint`, `Correction`, `Where the room got stuck`, `Builds on`…), its text, and its anchor as
a link that opens the transcript in the material viewer scrolled to that `## HH:MM` heading — the
document register gives those headings ids for it. The section link joins the sticky row. A
lecture with a session document but no sidecar reads `Not yet read for what was flagged` on its
row with `Distill again` beside it.

**Every document receives it.** `guides.rs` renders a `{hints}` block for a scope — a division's
rows, the master's all, a practice exam's scope's — into `module_guide.md`, `master_guide.md` and
`practice.md` under *What was said in the room*, each item with its kind, its session date and its
anchor, and one rule: these are the professor's own words about what matters; the ★ rail, the
self-test and the exam's weighting lean toward them, and an entry that draws on one cites its
anchor. The chat overview's compact form gains one line per class, `Flagged: N since <date>`; the
detailed form lists them, newest first, capped.

## Phase 3 — What the prompts know

**The master reads the course.** `{modules}` comes from `units` — the course's own names in
ordinal order, each with its start date and, for a Part, its week range — never from folder
names. A `{corpus}` block lists every applied contribution's note with its transcript path, the
way a division guide's does, and *Source material* says transcripts are read through their notes
and opened only where a note is not enough. The exclusive run, the chunked write and `--resume`
are untouched. SPEC §8.2's "full re-synthesis from all raw material" becomes what is then true:
every extract, every note, and transcripts through them.

**The rewrite marks what is new.** `list_guides` returns, beside `stale`, the diff — counts of
added, changed and removed entries and their names — from a diff function beside the set
comparison in `extract.rs`, tested. The row reads `Rewrite · 2 files added, 1 changed`. When a
guide is rewritten over an existing row, `guides.rs` renders `{changes}` — the added and changed
entries by name, and the previous `generated_at` as a date — and the prompt marks each entry that
draws on them with a small `New since Sep 3` chip in the yield rail, defined once in the design
contract. The previous guide is not fed back, so a rewrite cannot anchor on an earlier mistake;
a first write has no `{changes}` and no chips.

**Objectives.** `syllabus.md` reads, where the syllabus states them, each division's objectives
as a list beside its name, and `deadlines.rs` records them on the unit row with the divisions,
before the deadlines, as the weights are. A rescan replaces them; a scan that finds none leaves
the column. `guides.rs` renders `{objectives}` for a division (and, for the master, each division's
under its name), and the guide opens with them under *What this week set out to teach*, the ★
entries aligned to them where they exist.

**Practice exams get a row and a real rubric.** An exam is recorded in `guides` scoped
`practice:<rel path under Study Guides/>`, manifest from Phase 1, so it lists with a stale chip
like everything else and `list_practice` merges rows with the files that predate them. The
`Practice exam` button gains an optional `Focus on…` field beside it, carried as the `focus` the
chat tool already passes. `practice.md` receives `{assessment}`: the class's categories and
weights, the next open deadline of kind `quiz` or `exam` with its date, and the kinds of assessment
the syllabus names — and its rubric matches them instead of "realistic for the volume". Two lines
for M37, settled now while the prompt is open: every question carries a `data-topic` naming the
concept it tests, and the self-scoring panel, once totalled, posts
`{exam, results: [{question, topic, correct}]}` to `window.parent` — a sandboxed frame may — which
nothing receives until M37.

The digest checkbox's default and the sidecars go into §8.4, the honest manifest into §7 step 5,
the guard's rule into §6, the blocks into §8.1–§8.3 and §11, and the stale `LibreOffice is NOT yet
installed` line in §1 is corrected to what §2 already says.

## Phase 4 — Verify with the fewest runs

One run per document kind, each verifying every change to that kind at once, on Biostatistics,
whose Quiz 2 is the moment:

1. **Distill again** the Sept 3 session (or the Sept 10 one if it exists): the sidecars appear
   beside the note, the `Flagged` section lists the items with their anchors, an anchor opens the
   transcript at its heading, and the session document reads as before.
2. **Rewrite the Week 3 guide** (stale since M28): the prompt lists `{hints}`, `{objectives}` where
   the syllabus stated any, and `{changes}`; the document carries the professor's items and the
   `New since` chips; the manifest afterwards names every file the log shows it read; the cards
   file exists under `.classhub/cards/`.
3. **Write the semester master**: the header's semester map names the weeks by the course's own
   names, the run reads the notes and opens no transcript in full, and the cross-week section
   draws on the `thread` rows.
4. **A practice exam for Week 3 with `Focus on… missing data`**: the row exists with its manifest,
   the rubric names the class's weights and Quiz 2's date, the focus is honoured, and each
   question carries its `data-topic`.

Each cost goes in §1 beside the earlier measurements.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for the log reader (Read, Grep
  hits, an ignored Glob, an out-of-folder path), the manifest union (a read outside the listed
  sources joins; a listed source never read stays), the guard's rule (a changed path without a
  logged write is not the job's; one with a logged write outside the contract still fails), the
  manifest diff's counts and names, the sidecar parser (an empty array; a malformed file), the
  objectives parse on a fixture syllabus answer, and the master's roster rendered from `units`.
- The four runs above, each checked as listed, with the Week 3 guide's manifest naming every file
  its log shows it read.
- An edit of the owner's own during a running job — a fixture `.csv` added beside a source while
  an extract runs — leaves the job succeeded, and the guard's audit row names nothing.
- The `Flagged` section is absent for Design Studio, which has no lecture, and present for the
  three courses once their sessions are distilled again.
- SPEC §1, §6, §7, §8.1–§8.5, §9, §11, §12 and §13 state the design; §14's box is ticked; the notes
  carry Phase 0's measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** One digest, one division guide, one master and one practice exam — the four
  runs above, on Biostatistics — and a second run of any only if the first is wrong. Nothing
  else is distilled or rebuilt here; the remaining sessions get their sidecars by hand or by the
  shift (M34). No chat turn.
- **The master runs exclusively** and for twenty minutes or more; nothing under `src-tauri/`
  changes while any job runs in the dev build.
- **A redistill rewrites the session document pair.** That is expected; the corpus note and the
  contribution row are replaced under the same paths.
- **A Grep result's paths** come from the `tool_result` text the CLI streams, which is not
  truncated in the log; the reader must not trust a path outside the class folder.
- **`.classhub/cards/` is a new folder** inside the write contract; the guard's `JOB_WRITABLE`
  already admits `.classhub/`. The digest's cards sidecar sits in the corpus folder, which its
  `Edit` scope already names.
- **The `Flagged` anchor link depends on the viewer** giving `## HH:MM` headings ids; a transcript
  with no anchors (an untimed caption) shows the item without a link.
- **The installed app runs beside the dev build** on one database; the enqueue guards already
  keep one job per scope.
- **Never a real deadline, never source material edited.** The guard test's fixture is a `.csv`
  added beside a source and removed after.
