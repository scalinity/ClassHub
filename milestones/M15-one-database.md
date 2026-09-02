# M15 — One database for every build

**Read `SPEC.md` in full first**, then this file. §3 (the data directory), §6 (the job
runner) and §13 (one database for every build) are what this milestone finishes.

## Why

The app that runs the semester is the installed bundle in `/Applications`, and the app that
gets worked on is `npm run tauri dev`. From 2026-08-27 to 2026-09-02 they kept two databases:
a commit renamed the data folder from Tauri's default to `ClassHub`, and the installed build,
still resolving the default name, started a fresh database in the empty folder it found there.
Deadlines approved in one never reached the other, and three files the installed build sorted
into the tree left three Canvas proposals in the other database pointing at an inbox that no
longer held them.

The merge is done (IMPLEMENTATION_NOTES, 2026-09-02): one database at Tauri's own
`app_data_dir()`, in WAL mode, and `lib.rs::data_dir` resolves there for every build. What
remains is making two processes on that database safe, and making "the installed app is
current" a one-command step so the two builds stop drifting in code as well as in data.

## Phase 1 — Job rows own their process

`jobs::startup_recovery` fails every `queued`/`running` row on launch, on the theory that a
row in that state belongs to a process that died. With two processes on one database that is
false: launching a dev build while the installed app runs an extract marks that extract failed
in the table while it keeps running, and `extract::spawn_pipeline`'s guard (`extract.rs:224`)
then sees no active job and enqueues a second extract for the same files — soffice and a
Claude batch, twice.

Migration `0009_job_owner.sql`: `jobs.owner_pid INTEGER`. `enqueue` stamps the current
process id; recovery fails only rows whose owner is gone (`libc::kill(pid, 0)` — libc is
already in the tree through tauri). A `NULL` owner is a row from a build that predates the
column and is treated as orphaned, which is the old behaviour. `pump` and every
duplicate-active guard (`guides.rs:577`, `sorter.rs:241`, `deadlines.rs:347`,
`extract.rs:224`) already read the table, so they become correct across processes the moment
the rows stop lying.

Cancel stays per-process: a job's child handle lives in the process that spawned it.
`cancel_job` on a row another process owns returns an error saying so, rather than silently
doing nothing.

## Phase 2 — The install step

Add `scripts/install-app.sh`: quit the running `ClassHub.app` if any, `npm run tauri build`,
`ditto` the bundle into `/Applications`, relaunch. Expose it as `npm run install-app`. The
session protocol in SPEC §14 gains one line: after a milestone is accepted and committed, run
it — the installed app is the one the semester runs on, and a milestone that only exists in
`target/debug` has not shipped.

This script is the one place a production build is run. SPEC §13's "never run production
builds in sessions" stands for the agent; the script is for the owner.

## Phase 3 — The self-check, once a day

`enqueue_self_check` runs on every launch (`lib.rs`, in `setup`). It has run more than two
hundred times. Skip it when a `self_check` succeeded within the last 24 hours; keep the manual
re-run behind the auth warning unconditional. The guarantee it gives (SPEC §1: subscription
auth, never the API key) is about the environment, which does not change between launches on
the same day.

## Acceptance

- With the installed app running a job, launching `npm run tauri dev` leaves the row
  `running`; it finishes `succeeded` and both processes list it that way.
- Killing a process mid-job and relaunching fails exactly that process's rows, nothing else's.
- Two processes, one class, one changed file: exactly one extract job.
- `npm run install-app` leaves `/Applications/ClassHub.app` on the current commit, opening the
  same database (`lsof` shows one `classhub.db` path for both processes).
- Two launches in one day produce one `self_check` row.
- `cargo test` covers the recovery predicate; `npx tsc --noEmit` passes.

## Watch for

- **The Aug 25 build in `/Applications` is still there until Phase 2 runs.** It ignores
  `owner_pid` (named-column inserts leave it `NULL`) and its recovery still fails every active
  row on launch. Run the install step as the first thing after Phase 1 lands.
- **Never fail a row by `status` alone again.** Any future recovery or cleanup must ask who
  owns the row.
- **WAL files ride beside the database.** A copy of `classhub.db` without `classhub.db-wal`
  can be missing the last transactions; back up with `sqlite3 … ".backup"` or copy all three.
