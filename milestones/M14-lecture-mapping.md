# M14 — Lectures into weeks, and the unit corpus

**Read `SPEC.md` in full first**, then this file. §4 (`Weeks/`, `.classhub/corpus/`), §5
(`lecture_contributions`), §7.1 (transcripts), §8.4 (`lecture_digest`) and §8.5 (unit corpus)
are what this milestone implements. M13 is done — this needs a `units` list, and the syllabus
scan supplies one for all four courses.

## Why

A lecture is stored by when it happened; a guide is scoped by what it is about. Something has
to join the two, and `lecture_contributions` is that join.

For these four courses the join is the calendar. Each meets **once a week**, and each declares
divisions no finer than a week:

| Class | Meeting | Divisions |
| --- | --- | --- |
| CAI5720 Fundamentals | Tue 16:05–19:05 | 14 weeks |
| CAI5724 Design Studio | Wed 17:10–18:00 | 15 weeks |
| CAI5731 Biostatistics | Thu 11:45–13:40 | 17 weeks |
| CAI6734 Applied Generative AI | Tue 11:45–14:45 | 3 Parts — `(Weeks 1-8)`, `(Weeks 9-12)`, `(Weeks 13-16)` |

So a meeting sits inside exactly one division, and filing the transcript into its week already
decides which. There is nothing to infer.

**This milestone deliberately does not segment a lecture across units.** No division is finer
than a meeting, so every span a segmenter produced beyond the first would be an error — and a
wrong boundary is the one mistake in this pipeline that corrupts study material quietly instead
of failing visibly. Building the machinery would create that failure mode for no gain. If a
course ever declares divisions finer than its meetings, `lecture_contributions` already has the
span columns to hold them; adding the inference then is a smaller change than removing a
mis-tuned one now.

## Phase 1 — Transcripts file into `Weeks/`

M12 files transcripts to `<Class>/<Module>/Transcripts/`. Change to
`<Class>/Weeks/Week NN — <topic>/<date> — <title>.md` (SPEC §4). `lectures.rs` owns this;
`TRANSCRIPTS_DIR` in `db.rs:28` is replaced by the weeks layout — see `lectures.rs:99` and
`lectures.rs:465`, which both build and parse the old path.

Week resolution comes from `units`, **never from arithmetic**. Fundamentals runs Week 13 on
Nov 17 and Week 14 on Dec 1, so `(date − start)/7` is off by one for the rest of the semester
(SPEC §1). Where no schedule exists, the Add lecture form asks, defaulting to the nearest week
by date and letting it be corrected.

The week folder's `— <topic>` suffix comes from the unit name when there is one; a bare
`Week 03` is fine when there is not.

`prompts/sort.md` currently tells the sorter to route a transcript into a `Transcripts/` folder
inside a module. Update it: a transcript goes to its week.

## Phase 2 — The contribution

Once a transcript is filed, write its `lecture_contributions` row. One row per lecture, whole
transcript, `status='applied'` — the filing decision is the user's own, so there is nothing to
confirm. `start_ms`/`end_ms` and the line bounds cover the full transcript.

The unit is the week's own unit, except for **Applied Generative AI**, which declares no week
units at all — only three Parts whose names carry their week ranges. Resolve `Week NN` to the
Part whose range contains `NN` by parsing `(Weeks A-B)` from the unit name. This is the only
non-trivial mapping in the milestone; unit-test the parse, including a name with no range.

A course with neither a week unit nor a containing Part gets no contribution, and the lecture
still files and still gets its session document — it simply feeds no unit guide yet.

Refiling a lecture to a different week must move its contribution rather than add a second.
`UNIQUE(class_id, rel_path, unit_id, start_ms)` is in SPEC §5 for this; re-running a digest
replaces that lecture's rows.

## Phase 3 — Corpus notes

Each lecture is distilled once into `.classhub/corpus/<unit>/<date> — <topic>.md`: the
high-yield content, every point carrying its `HH:MM` anchor back to the transcript. This is what
keeps the cost sane — the expensive read is once per lecture, not once per guide per lecture —
and it makes the corpus inspectable, so what a guide drew on can be read rather than inferred.

Fold this into the `lecture_digest` pass, which already reads the whole transcript once. Doing
it separately pays for that read twice. A second job kind is justified only if one prompt doing
session document plus corpus note starts producing worse work than two would.

Add `.classhub/corpus/` to `tools.rs::search_material`'s scope, beside `EXTRACTS_DIR`, so chat
retrieves it.

## Phase 4 — Guides consume the corpus

`guides.rs` builds a source list per scope. For a unit scope it becomes: files under the unit's
folder when it has one · files Canvas attributed to it (M13) · its corpus notes, each listed
with the transcript path so the job can open the professor's exact words when the distillation
is not enough:

```
- .classhub/corpus/Week 03 — Transformers/2026-09-10 — Attention.md
  transcript: Weeks/Week 03 — Transformers/2026-09-10 — Lecture.md
```

The prompt tells the job to work from the corpus note and open the transcript only when it needs
the exact wording — so the path is an affordance, not the default read.

`extract.rs::current_manifest` must union contributing transcripts into a unit scope's manifest,
or a unit guide never goes stale when its lecture changes. This is the requirement most likely
to be missed, because nothing visibly breaks when it is — the guide just quietly stops updating.

## Phase 5 — UI

Show on a lecture's row in the Lectures listing which unit it feeds. The correction affordance
is refiling it to a different week, which moves the contribution with it — there is no separate
span to reassign.

Read the frontend-design skill first (SPEC §12).

## Acceptance

- A filed lecture reaches its unit's guide through a corpus note, and the guide cites it.
- Applied Generative AI's lectures resolve to the right Part from the week range in its name.
- Regenerating a unit guide after editing the transcript picks up the change — the manifest
  union works.
- Chat answers a question about the lecture citing the corpus note.
- Refiling a lecture to another week moves its contribution rather than duplicating it.
- `cargo test` covers the `(Weeks A-B)` parse and week resolution from `units`;
  `npx tsc --noEmit` passes.

## Watch for

- **Do not split a transcript file.** One lecture stays one readable artifact; the map is
  metadata, and splitting cannot be revised when units change.
- **Do not add span segmentation.** See Why. If it looks necessary, the course structure changed
  — check `units` before writing a prompt.
- **Week numbers never come from arithmetic** (SPEC §1).
- **Re-running a digest replaces that lecture's contributions, not adds to them.**
- **There are no lectures yet.** No `Transcripts/` or `Weeks/` directory exists in
  `~/Documents/AIBHS` as of 2026-08-27, and the tree holds 2/4/33/3 files per class. Phase 1
  can be built and unit-tested without one; Phases 3–5 need a real transcript, so ask for one
  rather than verifying against an invented lecture.
