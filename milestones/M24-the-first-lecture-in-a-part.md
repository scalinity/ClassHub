# M24 — The first lecture in a Part

**Read `SPEC.md` in full first**, then this file. §7.1 (filing), §8.4 (the digest) and §8.5
(the week-to-Part join and the corpus) are what this milestone verifies.

## Why

M16 proved the recording-to-guide path once, on Fundamentals: a week-numbered course whose
syllabus dates every week, so the Add lecture form resolved Sept 1 to Week 2 on its own, the
transcript filed into a folder named for the week's topic, and the division that read the note
was the week itself. Applied Generative AI is the other shape. Its syllabus declares three
Parts spanning week ranges and publishes no dates (§1), so nothing can resolve a date to a
week and the form has to ask. Its Sept 1 session — the first real lecture since M23 put the
ranges on the rows — must file into a bare `Weeks/Week 02/`, join Part I through
`first_week`/`last_week`, distill into `.classhub/corpus/Part I- Deep Learning to Large
Language Models/`, and be built into one Part guide beside a second week's note. Nothing on
this path has seen its real input: `lecture_contributions` holds one row, Fundamentals', and
every Part-numbered branch — the range join, the unnamed week folder, a corpus folder holding
two weeks — was accepted on fixtures.

One thing is known before measuring. A corpus note is keyed by its division and its
transcript's name (§8.5). For a week-numbered course the division is the week folder, where
`unique_rel_path` already keeps two names apart; a Part spans eight week folders, so two
transcripts named `2026-09-01 — Lecture.md` under Week 02 and Week 03 derive one note path.
The second digest would write over the first note and both contribution rows would name it.
The refile refuses that collision (M16); the filing does not.

## Phase 0 — Measure

Without spending a token, record what the form, the filing and the digest do for Applied
Generative AI as they stand:

- **The form**, opened on the dev build for Sept 1: what the WEEK select offers (labels,
  default), what the line under it says, whether the digest checkbox is available, and where
  ADD LECTURE would send the transcript with no week picked.
- **The filing**, against a copy of the live database through `week_slots`, `slot_for_week`,
  `corpus_rel_path` and `unit_guide_rel_path`: the folder for week 2, the division it feeds,
  the corpus path and the guide path; then the same for a second transcript of the same name
  filed into week 3.
- **The digest prompt**: what `corpus_instruction` and `lecture_context` say for that filing.
- **The guide**: `current_manifest` for `unit:37`, and what `unit_context` refuses with while
  no note exists.

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — Ask for the week, and keep two notes apart

A course whose divisions name week ranges and no days has weeks to file into and no date to
measure them against. The form says so in the course's own terms — each week's option names
the Part it feeds — and asks outright; the line that offers the sorter is for a course with
no weeks at all. A filing that would derive a corpus path another transcript of the class
already holds is refused before anything is captured, naming the title field as the way out,
and `record_contribution` holds the same line for the sorter's path. Tested: a Part's two
same-named transcripts, a week-numbered course's `(2)` suffix, and a refile that lands on a
held note.

## Phase 2 — The two lectures

Ask for the Zoom links first. Each lecture goes through the Add lecture form exactly as it
will be used, and every state is seen in the app. Expect and record:

- **Sept 1 → Week 2**: `Weeks/Week 02/2026-09-01 — Lecture.md`, the contribution row on unit
  37, the note at the Part's corpus path, the Lectures row's badge naming Part I, the
  Structure row gaining `1 LECTURE` and SYNTHESIZE GUIDE.
- **The second lecture → Week 3**, the Sept 8 recording where it exists and the fixture
  transcript otherwise: a second row on unit 37, a second note beside the first,
  `2 LECTURES`.
- **The Part guide** from the two notes: `Study Guides/Part I- Deep Learning to Large
  Language Models.html`, fresh, citing both notes.
- **Staleness**: a third transcript filed under Week 4 with the digest off flips the Part
  guide to STALE with both notes untouched; removed and rescanned, the guide reads fresh
  again.

Every failure is fixed in this milestone, however small, and every real behaviour that
differs from SPEC's description is recorded there.

## Phase 3 — What the run teaches

SPEC §1 gets what a second UF recording served and what a Part guide built from two notes
cost; §7.1 and §8.5 get the Part-numbered filing and the collision rule; the notes get the
gotchas.

## Acceptance

- `cargo test`: two same-named transcripts in one Part are refused at filing and the first
  note is untouched; the same name in one week folder still takes a `(2)` suffix; a refile
  onto a held note is still refused.
- Live, on the dev build: the Add lecture form for Applied Generative AI offers sixteen weeks
  each naming its Part, defaults to none, and files Sept 1 into `Weeks/Week 02/` feeding
  Part I.
- Two lectures — the real one, plus the fixture if only one recording exists — sit in the
  tree, the corpus and their session documents; the Part I guide is built from both notes
  with nothing hand-edited; a third transcript filed under Week 4 makes it stale without
  touching either note.

## Watch for

- **The budget.** One digest per real lecture and one Part guide; a second guide only if the
  first is wrong. No chat turn is needed.
- **No default week from arithmetic.** The class meets on Tuesdays and Fundamentals dates its
  weeks, but a week number from a weekday and another course's start is exactly what §1
  forbids. The form asks.
- **A Zoom link needs the owner.** The capture window may ask for a UF sign-in this time; if
  the host published no caption, the media fallback runs Parakeet — time it.
- **The installed app is the Aug 25 build** and runs beside the dev build on one database;
  nothing under `src-tauri/` changes while a job runs.
- **The Week 2 inbox proposals** for this course — a deck and a notebook — are the owner's
  to approve; leave them.
