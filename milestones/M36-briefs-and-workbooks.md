# M36 — Briefs and workbooks

**Read `SPEC.md` in full first**, then this file. §4 (the tree and what is source material), §6
(job kinds and their tool scopes), §8 (the documents; this milestone adds a §8.6 for the small
ones), §11 (deadlines), §12 (sections and rows) and §13 are what this milestone touches. The
shift (M34) and the assignment descriptions (M35) are what it stands on; where M35's probe left
a description empty, a brief reads the deadline's title and notes alone.

## Why

No course has a midterm. Averaged over the four, half the program's grade is projects and
presentations, a quarter is homework, and quizzes are under a tenth, in two courses. Every
document the app writes is exam-shaped, and nothing helps with a homework set, Design Studio's
weekly deliverable — one project, 60% of the grade, sixteen dated items from Sept 9 to Dec 2 —
or Applied's paper presentation. The calendar sets the moments: Fundamentals' Homework 2 on
Sept 21, Biostatistics' on Oct 4, a Design Studio item every Wednesday.

Two smaller documents share the shape and the session. Slides often post before a lecture
("Week 1 Slides Posted on Canvas"; Biostatistics' `Module 2 Slides, sharing before class`), and a
one-page pre-read before Thursday at 11:45 is worth more than the same page after. And a note
taken during a three-hour lecture is worth reading against the session document once the
digest lands — what the note has, what it missed, where the two disagree.

The line every one of these holds: **map the assignment to the material; never solve it.** The
same posture as the Canvas line — automate the student's own reading, not the student's work.

Not built: a grading of a draft, any write into `Project/`, a presentation kit on a schedule
(no date is known; it runs from a PDF's row), a brief for a quiz (the practice exam is the
quiz's document, and M37 gives it memory).

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as it
stands:

- Every open deadline of kind `assignment` or `project` per class with its date and whether it
  carries a description (from M35); for each, the divisions whose meetings fall between it and
  the previous assignment of its class — the brief's source window — with their notes, extracts
  and `lecture_hints` counts. Expected: Fundamentals' Homework 2 draws on Weeks 3 and 4.
- Design Studio's sixteen project items in order; where `AI Design Project Guidelines.pdf` sits
  (expected: still in `_Inbox/` with a Canvas card, or filed by M33's rule) and its extract's
  size once filed; the announcements that mention the project.
- The project category's weight in each class (expected: 30, 60, 30, 80 — every class
  qualifies) and the project-kind deadlines each has (expected: Design Studio sixteen,
  Biostatistics two, Fundamentals one, Applied none).
- Week folders holding a deck or reading and no transcript, today and for next week
  (the pre-read candidates).
- Notes whose title opens with a meeting's date (expected: none; the convention is set here).
- The light tier's cost for a small job, from M34's per-kind settings and M35's scans.

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — Homework briefs

A job kind `assignment_brief`, prompt `assignment_brief.md`, tools `Read,Glob,Grep,Write`,
written into `Study Guides/Briefs/<due date> — <title>.html` with a markdown twin for search.
Inputs: the class, the assignment's title, kind, due date and description, the objectives of the
divisions in its window, their corpus notes and extracts listed as a guide's are, and their
`lecture_hints` rendered as `{hints}`. Output: what the assignment asks, part by part; where
each part was taught, with `HH:MM` anchors and slide citations; the formulas and the R or Python
that apply, with the professor's corrections; the hints and pitfalls named in the room; what
the objectives say it is for. The prompt's hard constraint: describe where the answer lives,
never the answer; no worked solution to the assignment's own questions. Recorded in `guides`
scoped `brief:<deadline id>` with M32's honest manifest, stale when its sources change.

`Write the brief` on a deadline row of kind `assignment` or `project`, and `Read the brief` once
it exists. The shift gains a step after its guides: for each open assignment or project item due
within five days with no fresh brief, write one, up to a cap of two a night.

## Phase 2 — Project workbooks

A job kind `project_workbook`, prompt `project_workbook.md`, same tools, one document per class
at `Study Guides/Project Workbook.html` with a markdown twin, scoped `project` in `guides`.
Inputs: the class's project-kind deadlines in order with their descriptions and states; the
project's guidelines — every file whose name carries `project`, `guidelines`, `rubric` or
`proposal`, plus whatever the `Project material` picker (the syllabus scan's picker, reused)
was pointed at; the announcements and `lecture_hints` of kind `action` that mention the project;
and the class's `Project/` folder, which is the owner's own drafts — a user-owned folder like
any other, indexed by the scan, protected by the guard, and listed to the prompt as learner
work. Output: the milestones with dates and status; the rubric as the guidelines state it; what
the next item needs and what the drafts already have toward it; open questions to ask the
professor; a checklist for the week. A `Project` section in the workspace, present while the
class has a project-kind deadline: the next item, its date and `Write the workbook` / `Read the
workbook` / `Refresh · 1 item due`. The shift refreshes a stale workbook when an item is due
within seven days, one a night.

**The presentation kit.** A job kind `presentation_kit` from a PDF's row (`Presentation kit`
beside `Show in Finder`), writing `Study Guides/Presentations/<paper>.html`: the paper's claim in
one paragraph, talking points per figure with what the figure shows and what it does not, the
methods a questioner would probe, likely questions with the answers the paper supports, and a
one-slide summary. Manual only.

## Phase 3 — The pre-read

A job kind `pre_read` at the light tier, prompt `pre_read.md`, writing `Study Guides/Sessions/
<date> — Before class.html` with a markdown twin, scoped `preread:<unit id>`. Inputs: the week
folder's files with their extracts, the previous division's note and hints, and the objectives.
Output, one page: five things to know walking in; the terms; how it follows from last week;
three questions to listen for. The Lectures row for the meeting reads `Before class · Thu` with
`Read`. The shift writes one when the current or next division's week folder holds a deck or
reading and no transcript, one per course per night; the session document's landing removes the
pre-read's row and files, since it is superseded.

## Phase 4 — A note against the room

A job kind `notes_review` at the light tier, prompt `notes_review.md`, with `Read,Glob,Grep` and
`Edit` scoped to `Notes/` — a new scope in `jobs.rs::allowed_tools`, stated like the digest's.
A note whose title opens with a meeting's date (`2026-09-10 — Biostatistics in class`) is that
meeting's note; the editor offers today's date as a title. When the meeting's session document
exists and the note has no `## Against the room` heading, the job appends that section: what
the note has that the room did, what the room had that the note missed (with anchors), and where
the two disagree, with the session document's reading. The append is recorded like a save — the
note's previous content in the audit row — so it is one `Undo` away. The shift runs it after a
digest lands, for that meeting's note, one a night.

SPEC §4 names `Project/` as source material like any folder; §6 the four kinds and the `Notes/`
scope; a new §8.6 the small documents; §11 the brief's row action; §12 the `Project` section and
the rows.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for the brief's source window
  (the divisions between two assignments; a first assignment's window from the semester's
  start), the guidelines file match, the pre-read's candidate rule (a deck and no transcript;
  nothing when a transcript exists), the note-date convention and the `Against the room` guard
  against a second append.
- One run of each kind, on the dev build: a brief for the next open homework (Fundamentals'
  Homework 2 or Biostatistics' Homework 2, whichever is nearer) that maps every part to a
  source with anchors and contains no worked answer; the Design Studio workbook listing the
  sixteen items with the next one's needs from the guidelines; a pre-read for whichever week
  qualifies; a notes review on a fixture note dated for a distilled session, appended once and
  undoable. Each cost goes in §1.
- The shift's plan lists the four steps with their caps.
- The `Project` section shows for every class with a project-kind deadline and hides for none.
- SPEC §4, §6, §8.6, §11 and §12 state the design; §14's box is ticked; the notes carry the
  measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** One brief, one workbook, one pre-read, one notes review; the workbook at the
  global tier, the pre-read and the review at the light tier. No guide. No chat turn.
- **Never solve.** The brief's and the workbook's prompts carry the constraint as a hard rule and
  the reviewer of the run reads the output for it; a brief that works a problem is a failed run.
- **`Project/` is source material**: the guard fails a job that writes into it, and the scan
  indexes whatever the owner keeps there; a `.docx` draft extracts through LibreOffice as any
  does.
- **The `Notes/` Edit scope is new**; verify it both ways against the CLI as the digest's was —
  an append inside `Notes/` allowed, a write outside refused.
- **A brief for an assignment with no description** says what it lacks in its header rather than
  inventing the assignment.
- **Fixtures**: the notes review runs on a fixture note removed after; the brief and the workbook
  run on real deadlines and stay.
- **The installed app runs beside the dev build**; the shift's new steps run only where shifts
  run.
