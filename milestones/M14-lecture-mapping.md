# M14 — Lecture content mapping

**Read `SPEC.md` in full first**, then this file. §4 (`Weeks/`, `.classhub/corpus/`), §5
(`lecture_contributions`), §7.1 (transcripts) and §8.5 (unit corpus) are what this milestone
implements. M13 should be done first — this needs a `units` list — but it does not need Canvas
specifically, only *a* unit list from whatever source.

## Why

A three-hour lecture does not respect the course's divisions. A Week 2 lecture covers the tail
of one unit for ninety minutes and then moves into the next. ClassHub stores a lecture by *when
it happened* and synthesizes by *what it is about*, and today those are the same axis — a
folder is both the file's home and the guide's scope. So that lecture either pollutes the first
unit's guide with the second's material or is missing from the second entirely.

This milestone separates the two axes and builds the map between them.

## Phase 1 — Transcripts file into `Weeks/`

M12 files transcripts to `<Class>/<Module>/Transcripts/`. Change to
`<Class>/Weeks/Week NN — <topic>/<date> — <title>.md` (SPEC §4). `lectures.rs` owns this;
`TRANSCRIPTS_DIR` in `db.rs` is replaced by the weeks layout.

Week resolution comes from `units` where the course's schedule is known — **never from
arithmetic**. Fundamentals runs Week 13 on Nov 17 and Week 14 on Dec 1, so `(date − start)/7`
is off by one for the rest of the semester (SPEC §1). Where no schedule exists, the Add lecture
form asks, defaulting to the nearest week by date and letting it be corrected.

The week folder's `— <topic>` suffix comes from the unit name when there is one; a bare
`Week 03` is fine when there is not.

`prompts/sort.md` currently tells the sorter to route a transcript into a `Transcripts/` folder
inside a module. Update it: a transcript goes to its **week**, and the content-routing guidance
that was there for modules now belongs to §8.5's mapping pass, not to the sorter. The sorter
decides *when*, the digest decides *what about*.

## Phase 2 — Mapping, folded into `lecture_digest`

The digest job already reads the whole transcript once. Doing the mapping separately pays for
that read twice, so it goes in the same pass: the prompt gains the class's unit list, and the
output contract gains a `segments` array beside the existing `title`/`relPathHtml`/`relPathMd`:

```json
[{"startAnchor":"00:00","endAnchor":"01:22","unit":"Module 1","confidence":"high",
  "summary":"Decision trees, bagging, random forests — Module 1 as scheduled"},
 {"startAnchor":"01:22","endAnchor":"02:55","unit":"Module 2","confidence":"medium",
  "summary":"Moved early into SVMs and margins — Module 2 material"}]
```

The prompt must say plainly that a lecture commonly spans two units, that the scheduled unit is
a prior and not an answer, and that a boundary it is unsure of should be reported `medium` or
`low` rather than guessed confidently.

**Anchor → line resolution is Rust's job**, not the model's. `transcripts.rs` wrote the
`## HH:MM` headings, so it can find them: scan for the heading at or before `startAnchor` and
at or after `endAnchor`, and take those line numbers. **Snap outward.** A widened span costs a
paragraph of overlap; a narrowed one severs a sentence, and a guide built from half a sentence
is wrong in a way nothing downstream can detect. Unit-test this against a transcript with
irregular anchor spacing.

Record into `lecture_contributions` (SPEC §5). `high` confidence gets `status='applied'`;
`medium` and `low` get `status='pending'` and surface in the confirm queue beside file moves.

## Phase 3 — Corpus notes

Each applied span is distilled once into `.classhub/corpus/<unit>/<date> — <topic>.md`: the
high-yield content of that span, every point carrying its `HH:MM` anchor back to the transcript.

This is what keeps the cost sane — the expensive read is once per lecture, not once per guide
per lecture — and it makes the corpus inspectable, so what a guide drew on can be read rather
than inferred.

Add `.classhub/corpus/` to `tools.rs::search_material`'s scope, beside `EXTRACTS_DIR`, so chat
retrieves it.

Fold the distillation into the same digest pass if the output stays manageable; a second job
kind is justified only if one prompt doing session document + segments + per-unit notes starts
producing worse work than two would.

## Phase 4 — Guides consume the map

`guides.rs` builds a source list per scope. For a unit scope it becomes: files under the unit's
folder when it has one · files Canvas attributed to it (M13) · its corpus notes, each listed
with the transcript path and the raw span's line range:

```
- .classhub/corpus/Module 2/2026-09-10 — Attention.md
  raw span: Weeks/Week 03 — Transformers/2026-09-10 — Lecture.md lines 412–1180 (01:22–02:55)
```

The prompt tells the job to work from the corpus note and open the raw span only when it needs
the professor's exact words — so the line range is an affordance, not the default read.

`extract.rs::current_manifest` must union contributing transcripts into a unit scope's manifest,
or a unit guide never goes stale when its lecture changes.

## Phase 5 — UI

Show a lecture's split on its row in the Lectures listing — which units it fed, with the time
ranges. Pending (low-confidence) allocations appear as confirm cards. An applied allocation
must be correctable: reassigning a span to a different unit is the escape hatch when the model
put a boundary in the wrong place.

Read the frontend-design skill first (SPEC §12).

## Acceptance

- A lecture that starts in one unit and moves into the next contributes to **both** guides,
  each drawing only its own span, and neither guide contains the other's material.
- The split is visible on the lecture row and can be corrected.
- A low-confidence boundary lands in the confirm queue rather than applying silently.
- Chat answers a question about that lecture's second half citing the corpus note for the
  correct unit.
- Regenerating a unit guide after editing the transcript picks up the change — the manifest
  union works.
- `cargo test` covers anchor → line resolution including outward snapping and irregular
  spacing; `npx tsc --noEmit` passes.

## Watch for

- **Snap outward, never inward.** This is the one place a quiet bug corrupts study material.
- **A lecture with one unit is the common case.** Do not over-fit to the split case and
  produce two spans where the model should have produced one.
- **Do not split the transcript file.** One lecture stays one readable artifact; the map is
  metadata. Splitting is lossy and cannot be revised when units change.
- **Re-running a digest must replace that lecture's contributions, not add to them** — hence
  the `UNIQUE(class_id, rel_path, unit_id, start_ms)` in SPEC §5.
