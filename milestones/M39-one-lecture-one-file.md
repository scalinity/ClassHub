# M39 — One lecture, one file

**Read `SPEC.md` in full first**, then this file. §7.1 (lecture ingestion), §8.5 (the filing
decision), §9 (the chat agent, its tools and its context) and §10 (propose-and-confirm) are what
this milestone touches. It follows M38, which gave chat the write tools; this is the milestone
that makes the lecture ones usable by something that cannot see the screen.

## Why

Chat session #6 of 2026-09-16 filed two lectures. The record of it is the whole case, and every
number below is read from the live database rather than recalled:

Asked to distil a lecture it had just filed, the model re-ran `add_lecture` with the same Zoom
link, because `add_lecture` is the only lecture tool it has. `digest_lecture` exists — a command
in `lib.rs` behind the Lectures section's own button, calling `lectures::enqueue_digest` — and
is not among `tools::definitions()`. `lectures::add_with` never overwrites, deliberately
(a Tuesday holds two lectures, M26), so the re-file landed as
`Weeks/Week 04 — Data Quality and Preparation/2026-09-15 — Lecture (2).md`, byte-identical to
the first, and the digest queued against the copy. Job #366 has since succeeded: the corpus
note, the hints sidecar and the cards sidecar for Fundamentals' Week 4 are all named
`2026-09-15 — Lecture (2)`, and the transcript the week was filed under has no distillation at
all.

The digest did not start on its own. The Add lecture form defaults the checkbox on
(`AddLecture.tsx`); the tool defaults it off (`tools.rs`) and `chat_system.md` says to leave it
off — although M38's own brief specified `add_lecture` "with the digest on unless told
otherwise". Two doors to one action, disagreeing, and the one that cannot see the form is the
one that guessed wrong. Filing a recording and wanting nothing made of it is the rare case.

The cleanup then failed silently. `propose_file_moves` answers with `from → to` lines and no
ids, so `approve_move` was called with `1` — an id from months ago, long resolved. It answered
"already approved — it is no longer waiting", and the turn reported the duplicate cleared.
Proposal #66 is still `pending` as this is written, both files are still on disk, and the
destination it holds (`Weeks/_duplicates/`) is not a declared week, so approving it now would
run `refile_lecture` into a folder no division reads and take Week 4's corpus note with it —
destroying what job #366 spent an Opus run producing. An id the model invented, a tool that
answered about a different row, and an outcome asserted without a read.

Then the dates. For Fundamentals the model inferred 2026-09-15 from the course's Tuesday
meeting and the dates already filed, and filed without saying so. For Applied it refused the
same inference and asked for the date four times across four turns, each an API round, then
apologised for the inference that had been right and adopted the opposite rule. The owner's
correction is the design: **the date is inferred from what the app knows and stated, never
asked for.** The app knows more than the model was given — `units::week_slots` carries each
week's published date, `nearest_week` resolves one, the classes carry their meeting day, and
the filed lectures carry theirs. Applied declares three Parts over week ranges and no dates
(§8.5), so its week cannot be read and must be asked — but its date can be inferred from its
meeting day and what is filed, and the two questions were run together as one blocking ask.

And what is offered has to be what exists. The turn offered to "propose removal" of a file:
nothing in the tool surface deletes, `propose_file_moves` only moves. It offered to amend a
wrong date by re-filing, warning of another duplicate: `lectures::refile_lecture` carries a
transcript with its session document, its corpus note, its hints and its contribution row, and
is reachable only from a Finder move or an approved proposal. It said the digest model could
not be chosen: `settings::spawn_options_in` takes a per-kind model and effort, so
`lecture_digest` runs on whatever Settings says.

In the panel itself, one pasted Zoom link scrolled the whole conversation sideways: the message
list is `overflow-y-auto`, which makes the other axis `auto` too, and nothing broke the long
token.

Not built: a delete tool (nothing in this app deletes a reader's file, and M39 does not start);
a second capture path; a model selector inside chat; date inference for a course that declares
neither dates nor a meeting day.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database and the tree as it
stands:

- The duplicate's whole footprint: both transcripts' sizes and hashes, the `lecture_contributions`
  row for each, the corpus note, hints and cards paths, job #366's scope, and which of the two
  the `files` index calls a duplicate (§7). This decides the recovery, which is one-off and not
  a phase: the un-digested original is the one that can go, and a Finder rename of the survivor
  is refiled by the scan's own move detection (`lectures::lecture_left`) with its documents
  intact — no second Opus run.
- `move_proposals` #66's status and destination, and every other `pending` row, so nothing else
  is waiting on an id nobody read.
- What session #6 cost: its rows in `chat_messages`, the rounds spent on the four date asks, and
  what the two filings and the one digest spent on the subscription.
- `week_slots` for each of the four classes: which publish dates, which publish ranges, what
  `nearest_week` answers for 2026-09-15 in each.
- The settings rows for `job_model.lecture_digest` and `job_effort.lecture_digest`, present or
  absent, so the prompt names the real place.

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — A lecture is filed once, and distilled where it lies

`digest_lecture(class, path)` in `tools.rs`, over `lectures::enqueue_digest`: the transcript's
own path, the job id, and the refusal `enqueue_digest` already carries when one is queued or
running for that transcript. Its result names the file it read, so a digest against the wrong
copy is visible in the chip rather than in the corpus a week later.

`add_lecture` refuses a second filing for a class and date that already hold a transcript,
naming what is there, whether it is distilled, and the two ways past: `digest_lecture` for the
one already filed, or a `title` for a genuinely second session that day (M26). The refusal is
the tool's, not the prompt's, because a prompt cannot be relied on to hold a door shut.

The digest defaults on, as the form does and as M38 specified. The tool's description and
`chat_system.md` say what it costs and that filing without a session document is the exception
asked for by name.

## Phase 2 — The date and the week the app already knows

`date` becomes optional. Unset, it resolves the way the form's default does: the course's
published date for the week where there is one, else the most recent meeting day on or before
today for a course that publishes its meeting day — and the result says which date was used and
where it came from, so a wrong one is corrected rather than discovered. A course that gives
neither is the only case that asks.

`lecture_weeks(class, date?)` as a read tool, over the command of the same name: every week the
class can file into, its folder, the division it counts toward, its published date where it has
one, whether a lecture is already filed in it, and which week the date resolves to. This is what
a course of Parts over week ranges needs — the model asked which week and guessed from the
overview, where the answer was a list it could not reach.

`add_lecture` with no resolvable week refuses with that listing rather than routing silently to
`_Inbox/` for the sorter. The inbox route stays for the form and the shift, where a human sees
the outcome panel say so.

## Phase 3 — A proposal you can approve

`propose_file_moves` returns each proposal's id beside its `from → to`. `approve_move` and
`dismiss_move` echo the class and the paths of the row they resolved, so an id from another
conversation reads as wrong at the moment it is used. An id for a row that is not `pending`
answers with its status and its paths rather than a bare "already approved".

`refile_lecture(class, from, to)` in `tools.rs`, over `lectures::refile_lecture`: the correction
affordance for a lecture filed under the wrong date or week, carrying the session document, the
corpus note, the hints and the contribution row, and refusing a destination outside `Weeks/`
where the transcript would lose them. It is a write, not a proposal — the transcript is the
app's own file, filed by the app, and the reader who says "that was the 8th, not the 15th" has
asked for the move.

## Phase 4 — What the chat says it can do

`chat_system.md` gains, each with its reason:

- The date policy: infer it and say so; never ask for what the schedule and the filed lectures
  already answer. A course's published week date is read from the overview, never counted
  forward from Week 1 (§8.5).
- One ask, not four: where something genuinely must be asked, everything needed goes in the
  same question.
- Nothing deletes. A duplicate or a misplaced file is moved, by proposal, or left.
- An id is read, never guessed: `approve_move` takes an id from the overview or from the
  proposal that made it, in this conversation.
- Where a request cannot be honoured as asked, say so and stop before writing. Do not
  approximate it with a different call.
- Where a setting is the answer, name the screen: the job model and effort per kind live in
  Settings, which is what chooses the model a digest runs on.
- Say what was done, from what the tool answered — never an outcome inferred from a call that
  did not report it.

SPEC §9 states the tools and the date policy; §7.1 states that a chat filing resolves its own
date; §10 states that a proposal answers with its id.

## Phase 5 — The panel holds its width

Done ahead of the brief, kept here so acceptance covers it. Long addresses wrap instead of
scrolling the conversation sideways (the question, the answer prose, the streaming tail, the
thinking block and both tool-chip blocks), and an address renders as its host and as much path
as fits — `ufl.zoom.us/rec/share/…` — with the whole of it on hover and on the click, in the
question a reader typed as in an answer. `renderAnswer`'s label swap applies only where the
label is the address itself, so a link the model wrote in words keeps its words.

## Acceptance

- `cargo test` passes, with new tests for the same-date refusal (a second filing refused, one
  with a title allowed), the date resolution (a published week date, a meeting-day inference, a
  course that gives neither), `lecture_weeks`' listing for a dated course and a ranged one, and
  the proposal result carrying its ids.
- `npx tsc --noEmit` clean.
- On the dev build, in one conversation: "here is the recording for `<link>`" files it with the
  date stated and the session document queued, and no second file appears when the digest is
  asked for again; the same link a second time is refused by name; a filing for Applied answers
  with its weeks rather than the inbox; a proposal's own id approves it, and an id from another
  class reads as wrong.
- The duplicate from session #6 is recovered per Phase 0, with Week 4's corpus note, hints and
  cards intact and no second digest run.
- SPEC §7.1, §9 and §10 state the design; §14's box is ticked; the notes carry the Phase 0
  measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** The acceptance conversation spends API credits and one digest on the
  subscription. One digest, not three: the refusal is what is being tested, and a refused call
  spends nothing.
- **A filing is synchronous on the chat thread** under the one-per-class claim; the refusal has
  to come before the capture, not after, or a Zoom window opens for a file that will not be
  written.
- **The date inference is stated, never silent.** A wrong date filed quietly is worse than a
  question, and the whole policy rests on the result saying which date it used.
- **`refile_lecture` out of `Weeks/` drops what the transcript carries** — the session row, the
  contribution, the hints and the note (`lecture_left`). The tool refuses it rather than
  explaining it afterwards.
- **Proposal #66 must not be approved as it stands**: its destination is not a declared week, so
  approving it would take Week 4's corpus note with the file.
- **The write-tool list is served from Rust** (`chat_tool_names`); two new write tools have to
  reach the sidebar's chips through it, not through a copy.
