# M19 — Chat knows what the app knows

**Read `SPEC.md` in full first**, then this file. §9 (the chat agent), §8.1–8.3 (guides and
practice) and §12 (the workspace) are what this milestone touches.

## Why

The chat's picture of the hub stopped at M11. `tools::overview_text` lists depth-0 folders as
"modules" (M13 established that a folder is not a division), knows nothing of `units`,
lectures, corpus notes or pending deadline proposals, and `trigger_synthesis` can only target a
folder or `master`. It does not know which class is open, so every question asked from a
workspace has to name the class. And practice exams, which the spec put behind a button as
well as behind chat, exist only behind chat.

## Phase 1 — The overview

Rewrite the per-class block of `overview_text`:

- the course's divisions by count and kind, the current one (M18), and in detailed mode the
  list with dates;
- lectures: filed transcripts per division, which are distilled, which session documents exist;
- guides: every scope `list_guides` returns, unit guides included, with staleness;
- pending proposals of both kinds with ids, so the model can say what is waiting and where;
- folders named as folders.

Keep the compact form short: it rides every request as system context.

## Phase 2 — Division-scoped triggers

`trigger_synthesis` accepts a division name as `scope` and routes to
`guides::synthesize_unit`; `generate_practice` accepts one too, and a unit-scoped practice exam
draws on the same sources a unit guide does (`synthesis_context` with the unit id). The tool
descriptions name both forms; the system prompt's synthesis paragraph follows.

## Phase 3 — The open class rides with the question

`send_chat` gains `class_id: Option<i64>`; the frontend passes the open workspace's class (the
drop-target mechanism in `sorter.ts` already tracks it). The system prompt gains one line: the
class being looked at, and that a question naming no class means this one. Starters draw from
that class instead of a random one. Nothing is persisted; it is context for the turn.

## Phase 4 — Practice exams from the workspace

A `PRACTICE EXAM` action in the guide cluster on folder rows, unit rows and the master strip,
backed by a `generate_practice` Tauri command that reuses `guides::generate_practice`. Same
duplicate-active guard as guides. The PRACTICE EXAMS section already lists what lands.

## Acceptance

- From the Biostatistics workspace, "what's this week about?" answers from the overview
  without naming the class.
- "Make a practice exam for Week 3" queues a unit-scoped practice job visible in the Job
  Center; the button on Module 1's row queues a folder-scoped one.
- The chat can say which deadline proposals are waiting, by title and class.
- `cargo test` covers the scope parser for the two triggers; `npx tsc --noEmit` passes.

## Watch for

- **The compact overview is paid for on every turn.** Measure its token count before and
  after; the detailed form is where lists go.
- **Chat still never moves files** and never syncs Canvas. The write policy in
  `chat_system.md` is unchanged.
