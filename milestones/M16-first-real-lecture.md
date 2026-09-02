# M16 — The first real lecture

**Read `SPEC.md` in full first**, then this file. §7.1 (transcripts), §8.4 (session
documents) and §8.5 (the unit corpus) are what this milestone verifies.

## Why

M14 was accepted against fixtures: as of 2026-09-02 no `Weeks/` folder exists in
`~/Documents/AIBHS`, `lecture_contributions` is empty, and no `.classhub/corpus/` note has
ever been written. Seven class sessions have happened. No unit in any course has a folder
(SPEC §7.2), so every unit guide is built entirely from distilled lectures — the pipeline the
semester's guides depend on is the one part of the app that has never seen its real input.

The Zoom capture, on-device transcription, filing, digest and corpus paths were each written to
fail legibly. This milestone finds out what they actually do.

## Phase 0 — One real recording

Ask for one: the Zoom link for a session already held (Canvas lists cloud recordings under the
course's Zoom tab), or a `.vtt`/`.txt` caption track downloaded from it. Do not invent a
lecture. Prefer a Biostatistics or Fundamentals session, since those courses date their weeks
and the resolved week can be checked against the syllabus.

## Phase 1 — Add it, and fix what breaks

Run it through the Add lecture form exactly as it will be used. Expect and record:

- **Zoom link:** whether the store exposes `ccUrl` or `transcriptList` for this host's
  recordings, whether a passcode gate appears, and how long the read takes. If the host
  published no transcript, the media fallback runs Parakeet — time it.
- **Normalization:** speaker attribution, paragraph merging, `## HH:MM` anchors. Read the filed
  transcript and check it reads like a lecture, not a timing grid.
- **Filing:** `Weeks/Week NN — <topic>/<date> — Lecture.md`, the week matching the syllabus,
  and the `lecture_contributions` row with `status='applied'`.
- **Digest:** `Study Guides/Sessions/<date> — <topic>.html` and `.md`, the corpus note under
  `.classhub/corpus/<unit>/`, every key point anchored.
- **Guide:** synthesize that week's unit guide; it must cite the corpus note and open the
  transcript only for exact wording.
- **Chat:** a question about the session cites the corpus note or the session markdown.
- **Refile:** move the transcript to the neighbouring week through a proposal and confirm the
  contribution moved rather than doubled.

Every failure is fixed in this milestone, however small, and every real behaviour that differs
from SPEC §7.1's description is recorded there.

## Phase 2 — What the run teaches

Update SPEC §1 with what was measured: Zoom's actual page shape for UF recordings, Parakeet's
real minutes-per-hour on this Mac, and the digest's real cost on a three-hour session.
IMPLEMENTATION_NOTES gets the gotchas.

## Acceptance

- One real session sits in the tree, in the corpus, in a session document, and in its unit's
  guide, with nothing hand-edited.
- Every step's UI state was seen in the app: running, filed, distilling, the feeds-unit badge,
  the guide going stale and then fresh.
- Anything that broke is fixed and tested; anything undocumented is documented.

## Watch for

- **Do not add span segmentation** (M14's rule). A lecture that seems to cover two weeks is a
  filing question, not a prompt question.
- **A three-hour transcript is a large prompt.** If the digest hits the output cap, the
  chunked-writing clause from the guide prompts is the fix, not a shorter document.
- **Zoom's page changes without notice.** A probe that finds nothing must say what it looked
  for; a dead end is the manual step, not a workaround deeper into Zoom.
