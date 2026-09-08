# M35 — What Canvas knows

**Read `SPEC.md` in full first**, then this file. §1 (the Canvas and Zoom facts), §7.1 (how a
recording reaches the app), §7.2 (what a sync reads), §10 (the queue), §11 (deadlines and their
readers) and §12 (Notices, Lectures) are what this milestone touches. Phase 0 is a probe in the
shape of M13's and M22's: measure first, record in §1, build only what has content.

## Why

The owner is in the loop for every recording: paste the link, pick the date, pick the week. The
recordings for these courses live behind the Zoom tool in Canvas's course navigation, an LTI
launch into Zoom's own page, and every real capture so far — four, on UF cloud recordings — was
served to an anonymous GET with no sign-in. If the signed-in Canvas window can reach that list,
the last human step in the Tuesday chain goes away; if it cannot, the form can at least be
pre-filled and the app can ask.

Announcements are a record with no unread state and no reading. "Today's Office Hours
Postponed" and "Project Milestone Assignment 1 Posted" sit as inert text, and Applied
Generative AI has no dated item anywhere — its syllabus carries no dates and its Canvas syllabus
body is a link to that PDF — because its professor communicates by announcement, which nothing
reads. And a Canvas assignment carries a description the sync never keeps, which M36's briefs
need.

Not built: Today (M37), the briefs (M36), any reading of a Canvas Page's prose for dates, any
credential, any write to Canvas or Zoom.

## Phase 0 — Probe

Live, with the owner at the sign-in window, and recorded in SPEC §1 with timings the way the
modules finding and the Zoom capture were:

- For each course, from inside the signed-in Canvas page: `/api/v1/courses/:id/tabs` and
  `/api/v1/courses/:id/external_tools` — whether a Zoom tool exists, its id and launch URL.
- `GET /api/v1/courses/:id/external_tools/sessionless_launch?id=<tool>` — the launch URL it
  returns; the hidden window navigated to it; where it lands (expected: an `applications.zoom.us`
  page), whether a further sign-in is asked, and how long until the recordings list renders.
- What that page exposes: the requests it makes for its recordings list (its own API under the
  LTI origin) and what one recording carries — title, date, duration, a share URL — and whether
  the share URL is the shape the existing capture reads (`ccUrl` from the player's store).
- For last week's four meetings: whether each has a recording there, and whether the existing
  capture path, given its share URL, returns the caption without a window shown.
- The nine announcements by eye: which carry a date, an instruction, or a change — the
  extraction's expected yield — and the shape of `/assignments` responses: whether `description`
  rides along and its stripped size.

If the tool is not there, or the page needs a sign-in the hidden window cannot pass, or the
list is not readable: **stop that half and report**, as M13 did — record the finding, and
Phase 1 becomes its fallback. Never scrape Zoom's rendered panel (§7.1: it is a recycle-scroller);
never store a Zoom credential; the window allowlist widens to the LTI origin for this window
only.

## Phase 1 — Recordings

**If the list is readable.** `canvas.rs` gains `list_recordings(course)` — the in-page read on the
LTI page, the same `eval_with_callback` shape, returning `{title, recorded_at, share_url}` — and
migration `0017` a `recordings(id, class_id, share_url UNIQUE, recorded_at, title, rel_path NULL,
status TEXT — new|filed|skipped|failed, seen_at)` table so nothing is captured twice.
`lectures.rs` files a discovered recording as the form would: date `recorded_at`, week resolved
from `units` for a dated course; for a course whose weeks carry no dates, into the week after
the latest filed week when exactly one meeting day has passed since that lecture's date — one
meeting is one week for these courses, read from `meetings`, never from arithmetic on the
calendar — and otherwise left `new` for the form, which Today (M37) will list and the Lectures
section lists now under `Recordings found`. The capture window opens hidden and is shown only
when Zoom asks for a sign-in, the Canvas pattern; the digest follows as the form's checkbox
would. A recording dated on a day the course did not meet is `skipped` and named.

A `Find recordings` action in the Lectures section runs it by hand; the shift (M34) gains a step
after the sync: for each course, list, and capture what is `new` and dated on a meeting day, up
to its digest cap.

**If it is not.** The Add lecture form opens with the last meeting's date and its week filled in
and accepts a dragged URL, and the Lectures section reads `No transcript for Tue, Sep 8` for a
meeting that passed with nothing filed. The shift step is not built.

## Phase 2 — Announcements become actions

A light-tier job kind `announcement_scan` — read-only tools, the syllabus scan's deny list, strict
JSON — over a class's announcements the sync has not yet read this way: input their titles,
bodies and dates with today's date and the class's existing deadlines; output one entry per
announcement, `{canvas_id, deadlines: [{title, kind, due_at, notes}], todos: [text],
changes: [text]}`. The sync enqueues it after reading announcements, as it enqueues a sort job
for loose files. Dated items become `deadline_proposals` with source `announcement` — a model's
reading, so a card, merged on (title, day) with the other readers' as today; `todos` and
`changes` land in `announcement_actions(id, announcement_id, kind TEXT — todo|change, text,
done INTEGER, created_at)` and show under the notice with a checkbox, the section's first
control. A re-run over an announcement already read writes nothing. `deadline_proposals.source`
admits `announcement`; the card's chip says so.

## Phase 3 — What an assignment says

`deadlines.description TEXT NULL` and `deadline_proposals.description TEXT NULL` in the same
migration: Canvas's assignment description stripped to text through the extractor's stripper,
refreshed on every sync for a tracked row, capped. The Deadlines row expands to show it beneath
the notes, and M36's briefs read it.

SPEC §1 records the probe; §7.1 gains the discovered-recording source; §7.2 the recordings read,
the announcement scan and the description; §11 the third reader; §12 `Recordings found` and the
notice's actions.

## Acceptance

- SPEC §1 records the probe's findings for all four courses, with timings.
- `cargo test` passes, with new tests for the undated course's one-meeting rule (files after
  one meeting, waits after two, skips a non-meeting day), the recordings table's dedupe, the
  announcement scan's JSON parse (a malformed entry costs that announcement, never the scan) and
  the description's cap and strip.
- Where the list is readable: on the dev build, `Find recordings` lists last week's recordings;
  one is captured with no window shown and filed into its meeting's week with a note and a
  session document (the one digest of the milestone); a second run captures nothing new; the
  shift's plan lists the step.
- Where it is not: the form opens pre-filled for each course's last meeting, and the Lectures
  section names the meeting with no transcript.
- After a sync: the scan proposes at least the postponed office hour and the milestone
  assignment from the existing announcements, the proposals carry `announcement`, the to-do lines
  show under their notices and a checkbox marks one done; a second sync writes nothing new.
- The Fundamentals homework rows carry Canvas's description after a sync.
- SPEC §1, §7.1, §7.2, §11 and §12 state the design; §14's box is ticked; the notes carry the
  probe, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** One digest, for the one captured recording; four announcement scans at the
  light tier (about the cost of a syllabus scan each). No guide. No chat turn.
- **The LTI page is undocumented twice over** — Canvas's launch and Zoom's page — and either can
  change without notice. Each probe state is logged with its timing as `zoom.rs` logs its own,
  and a miss names the manual step. The file and media paths do not depend on it.
- **The window allowlist** admits the LTI origin only for the recordings window; the capture
  window's allowlist is unchanged; no cookie for Zoom is stored anywhere.
- **A live sync and a first LTI launch need the owner at the sign-in window**; a Duo round may
  be asked once.
- **An undated course's filing follows the one-meeting rule or waits**; nothing is filed by
  dividing a date.
- **A model's reading of an announcement stays a proposal**; only its to-do lines are written
  directly, and they are text on a notice, reversible by their checkbox.
- **The installed app runs beside the dev build**; the sync's mutex holds across the new step.
