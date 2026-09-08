# M38 — Chat that ranks

**Read `SPEC.md` in full first**, then this file. §1 (no embeddings API), §9 (the chat agent, its
tools and its context) and §13 are what this milestone touches. It is independent of the
calendar and can run earlier if chat becomes the daily surface; it is last because the
milestones before it are what the calendar needs first.

## Why

`search_material` returns ripgrep's own order — directory, then line — cut at the first eighty
lines, so the model reads whichever files sort first, and it is the one subprocess in the
codebase spawned without a bound. Each tool round replays the whole conversation with only the
system block and the tool schemas cached, which the notes record as the largest cost of a
multi-round turn; a 429 or a 529 ends the turn; nothing compacts a long session; the always-on
overview spends most of its text on a list of twenty-five deadlines. A citation opens a text
file and not a script or a CSV, and an `HH:MM` in an answer is dead text. And chat can propose a
move but not approve one, cannot run a sort or a syllabus scan, cannot batch-approve deadlines,
cannot add a lecture or start the shift, and cannot undo.

Anthropic has no embeddings API (§1), and FTS5 is the ranked search that needs none: SQLite's
own, over text the extract pipeline already writes, with BM25 and snippets.

Not built: a vector index, a second retrieval model, streaming tool inputs, a rename or delete
of a conversation.

## Phase 0 — Measure

Without spending a token beyond the API's own accounting, against a `.backup` copy of the live
database and the chat sessions' stored usage:

- `SELECT sqlite_compileoption_used('ENABLE_FTS5')` through the app's own connection (the bundled
  build is expected to say yes; if not, the plan changes to a build flag before anything else).
- For the stored sessions, per round: input tokens, cache-write and cache-read tokens, from the
  usage the API returned (stored, or reconstructed from the message sizes) — the shape of the
  saving.
- Three real questions from the stored sessions run through today's `search_material` on the
  backup's tree: hits, files, lines returned versus total, and whether the file that answers
  the question is among the first eighty lines.
- The overview's byte size by section (expected: the deadline list is most of it).
- `chat_max_tokens`'s current value and shape.

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — Ranked search

Migration `0019`: `material_fts` as an FTS5 virtual table over `(class_id UNINDEXED, rel_path
UNINDEXED, kind UNINDEXED, content)`, filled at extract finalize for every extract, on write for
corpus notes, sidecars' text, notes and the markdown twins of guides, briefs and session
documents, and rebuilt in full by a `rebuild_search_index` command and at the first launch after
the migration. A duplicate (M33) and a Canvas page mirror are indexed once each.

`search_material` queries FTS with `bm25()` and `snippet()`, returns the top twenty hits as
`relpath: snippet` with the matched line's number where the content carries lines, and the
header names the total; a query FTS cannot parse, or one the model marks as a regex, falls back
to ripgrep through `jobs::wait_bounded` with a twenty-second limit — the one spawn in the
codebase that had no bound. The scope stays the four folders per class.

## Phase 2 — Cheaper, sturdier turns

A cache breakpoint on the last message of each round beside the two that exist, so the
conversation's prefix is a hit on rounds two onward; a 429 or 529 retried three times with
backoff before the turn ends in an error; a session past a threshold of turns compacted by
dropping the bodies of tool results older than the last four rounds while every text and
thinking block stays, so the API's continuity rule holds; the overview's open-deadline list
trimmed to the next seven days and a count of the rest, the `Flagged` line (M32) and the
threads staying; `chat_max_tokens` stored as `chat_max_tokens.<model id>` with a bare number.

Citations open `.py`, `.R` and `.csv` files in the viewer like text; an `HH:MM` beside a
transcript's path in an answer becomes a link that opens the transcript at that anchor, through
the viewer's heading ids from M32.

## Phase 3 — Chat runs the pipeline

New tools in `tools.rs`, each on the audited path its button uses and each answering with what
changed: `approve_move(id)` and `dismiss_move(id)`, `approve_all_moves(class)`, `run_sort(class)`,
`run_syllabus_scan(class, target)`, `approve_deadlines(ids | class)`, `add_lecture(class,
source, date, week?)` with the digest on unless told otherwise, `run_shift()`, `undo_last()`,
and `list_hints(class, since?)` over `lecture_hints`. The write-tool names live in one place: a
`chat_tool_names` command the sidebar reads, so the literal array in `src/lib/chat.ts` goes. The
system prompt's write policy says an approval is the reader asking and a move that came from
chat's own proposal still needs the reader's word.

SPEC §9 states the search, the caching, the compaction and the tools.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for the FTS query builder (a
  phrase, a term list, a query that falls back), the snippet formatting, the retry decision (a
  429 retries, a 400 does not), the compaction (tool-result bodies dropped, text and thinking
  kept), and the tool-name list served to the sidebar.
- On the dev build: a search for a phrase from a corpus note returns that note first with a
  snippet; the same query with a regex marker falls back to ripgrep and returns within the
  bound; the index rebuilds from `Settings`.
- A five-round question's usage shows cache reads on rounds two onward larger than its cache
  writes; a compacted session still answers a follow-up that refers to an earlier turn's text.
- "Approve the pending card for `<file>`" approves it with the audit row and the notice;
  "add the lecture at `<link>` for Tuesday" files it (with the digest off, for the budget);
  "undo that" reverses the last reversible row; an `HH:MM` in an answer opens the transcript at
  its heading; a `.csv` citation opens the viewer.
- SPEC §9 and §13 state the design; §14's box is ticked; the notes carry the measurements, what
  was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** Chat turns spend API credits: the acceptance turns above, about a dollar. No
  synthesis; the lecture added by chat runs with the digest off.
- **FTS5 must be compiled in**; Phase 0 decides before anything is written.
- **The compaction must keep thinking blocks intact** for the API's continuity rule; drop only
  tool-result bodies, and only old ones.
- **The read-only deny list is unchanged**; the new tools are chat's, running in the app, not a
  job's.
- **A tool that moves or files writes its audit row first**, so the guard and `undo_last` both
  see it.
- **The index is derived**: losing it costs a rebuild, never data; the rebuild runs on a
  background thread with the connection released between files.
