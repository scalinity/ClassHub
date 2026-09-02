# M17 — Proposals that tell the truth

**Read `SPEC.md` in full first**, then this file. §6 (the write-scope check), §10
(drop-to-sort), §11 (deadline proposals) and §7.2 (Canvas file placement) are what this
milestone touches.

## Why

Four things the app says about pending work are false or invisible, and each was found in the
live database on 2026-09-02:

- Eleven deadline proposals had waited since Aug 27 with no sign of them on the dashboard.
  Fundamentals' seven homeworks and its capstone were among them, and Homework 1 was due Sep 7.
- Three Canvas move proposals pointed at inbox files that had already been sorted into the
  tree; the card badge counted them, and approving one would have failed.
- Approving three sort moves while an extract job ran demoted that job as out of contract:
  the fingerprint diff counted the app's own moves as the job's writes. For an extract that
  cost a retry; for a master guide it would have thrown away half an hour.
- Canvas filed slides under "Lecture Slides" for a course whose material is organized by
  Module; the destination was rejected by hand and the files sorted by content instead.

## Phase 1 — Deadline proposals on the card

`db::ClassCard` gains `pending_deadline_proposals` (rows in `deadline_proposals` with
`status='pending'`). The card shows `N PROPOSED` in the badge cluster beside `N TO SORT`, the
same accent chip. `tools::overview_text` prints the count on its header line beside the
move-proposal count.

## Phase 2 — Past-dated proposals

A proposal whose `due_at` is before today renders its date in the muted register with a `PAST`
tag, and `ADD ALL` leaves it in the queue. It stays individually addable, because a past date
can still be a real deadline entered late. `approve_deadline_proposals` takes the ids the UI
sends, so the filter is client-side, and the queue's line says what `ADD ALL` skipped.

## Phase 3 — Proposals whose file is gone

`sorter::sort_state` and `sorter::pending_count` both list the inbox; add one pass before
either answers. A pending proposal whose `source_rel_path` is not a file on disk is resolved
`dismissed` with an audit row (`sort.proposal_vanished`, carrying the row). The badge and the
queue then agree with the disk. A Canvas re-sync already matches files by name and size against
the tree, so a vanished proposal is never re-proposed.

## Phase 4 — The write-scope check ignores the app's own moves

`jobs.rs` fingerprints sources before a spawn and compares after (SPEC §6). Between those two
reads the app itself may move files: an approved sort or Canvas move (`sort.move` audit rows
carry `from` and `to`), a drop or Canvas download staging into `_Inbox/`, a lecture filing
into `Weeks/`. None of those is the job's doing.

The compare consults the audit log for the job's window, `started_at` to now: a path that
appears as the `from` or `to` of an app-recorded move is excluded from `touched`. Any app
write into a source area that is not yet audited gains an audit row in this phase, since the
audit log is now what tells the guard "that was us". Extend the existing fingerprint test: a
rename during the window with a matching audit row is not a demotion; the same rename without
one still is.

## Phase 5 — Disagreeing with Canvas's folder

A Canvas card gains `SORT BY CONTENT`: it enqueues a sort job for that one file, and for that
explicit request the sort's destination replaces the Canvas one (`sorter.rs:712` keeps refusing
the automatic case). The card then re-renders as a sort proposal with the Canvas folder named
in its reasoning, so the professor's placement stays visible. Explicit, not heuristic: the rule
that Canvas's placement outranks a content guess stays for every file nobody asked about.

## Acceptance

- Fundamentals' card shows `8 PROPOSED` before its queue is approved and nothing after.
- A proposal dated before today shows `PAST`; `ADD ALL` skips it; `ADD DEADLINE` on its card
  still works.
- Move a proposed inbox file away in Finder and reopen the workspace: the card is gone, the
  badge is right, an audit row exists.
- Approve a sort move during a running extract: the job finishes `succeeded`.
- `SORT BY CONTENT` on a Canvas card yields a sort proposal for the same file; approving it
  moves the file to the sort's destination.
- `cargo test` covers the fingerprint exclusion; `npx tsc --noEmit` passes.

## Watch for

- **The guard must still catch a job that moves a source.** Exclusion is by audit row, never
  by "looks like a rename".
- **Dismissed is terminal per path** (Post-M9). A vanished proposal's file, if it reappears in
  the inbox, is a fresh drop.
- **`ADD ALL` is one backend call** (Post-M10). Keep it that way: filter the ids, not the
  endpoint.
