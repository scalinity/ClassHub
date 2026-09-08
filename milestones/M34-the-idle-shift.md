# M34 — The idle shift

**Read `SPEC.md` in full first**, then this file. §1 (what a session costs), §6 (the runner, its
models and its rate-limit events), §7 (the extract pipeline's automatic enqueue), §7.2 ("never on
a timer"), §8.1–§8.4 ("manual trigger"), §12 (the Job Center and Settings), §13 (plugins) and §15
("always user-triggered, never scheduled") are what this milestone touches. Two of those
sentences were written as cost lines and this milestone reverses them on purpose, with the costs
now measured and a budget in their place.

## Why

Everything that costs tokens waits for a click. The launch scan, the extract pipeline, one
Canvas sync a day and a sort job for loose files run on their own; every digest, every guide,
every exam and, until M33, every file move waited for the owner. The reason was cost, and the
cost is now a number: a digest is $2–4 over 9–16 minutes, a division guide $6–8 over 18–22, so a
full week of four lectures — four digests and four guides — is about forty dollars of
list-equivalent against rolling limits that reset by morning. Nine guides stand against
fifty-one divisions, and the semester master is stale, because a click is what each cost.

The concrete moment is a Tuesday: two courses meet, and by breakfast both sessions should be
distilled and their guides rebuilt, with nothing pressed. The shift runs when the owner has
stopped for the evening, holds the Mac awake while it works, stops on the first rate-limit
event, and says in the morning what it did. What it cannot do is wake a sleeping machine, so a
missed shift catches up at the next launch the way the Canvas sync already does.

Not built: the Today block that shows the shift's work on the dashboard (M37), recording
discovery (M35), briefs and pre-reads (M36), a master guide on a schedule (never), any change
to `pmset`.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database, the job logs and the
machine:

- The runner's week: every non-`self_check` job since Sept 1 with its kind, start hour, duration
  and (from its log's result event) cost — the shape the budget and the window are set from.
  Expected: digests and guides in the evening, 9–22 minutes each, none past midnight.
- `rate_limit_event` lines in the logs (`grep -l rate_limit_event logs/*.jsonl`): what the CLI
  emits, when, and what the runner's handler does with it today (expected: surfaces it as
  progress text and nothing else).
- Idle time on this machine: `ioreg -c IOHIDSystem | awk '/HIDIdleTime/'` — that it reads, its
  unit (nanoseconds) and its value after a minute untouched.
- `/usr/bin/caffeinate` present; `pmset -g` for the system sleep setting on power (the honest
  limit: the shift keeps an awake Mac awake and does nothing for one asleep).
- What the first shift would find tonight: applied contributions whose corpus note is missing;
  session documents without a `.hints.json`; stale or absent guides for divisions with sources
  whose meeting this week is past — the list, with its expected cost.
- The self-check: rows inserted per launch since M15's daily gate (expected: one per launch,
  restored or not).
- The settings the runner reads at spawn (`job_model`, `job_effort`, `job_concurrency`) and
  where.

The findings go in the notes; nothing is changed until they are written down.

## Phase 1 — Tiers and the meter

**A model and effort per kind.** `settings.rs` reads `job_model.<kind>` and `job_effort.<kind>`
at spawn, falling back to the global pair; Settings shows one row per kind with `Default` or a
picked model and effort. The light tier (Sonnet-class at `medium`) is the suggested default for
`sort_proposal`, `syllabus_scan` and M35's and M36's small kinds; the global pair stays Opus at
`xhigh` and every synthesis kind inherits it unless changed. Each setter audits before and after
values like the existing ones.

**The meter.** `list_jobs` already carries durations; the Job Center's header gains `This week:
4 digests · 3 guides · 2 h 10 m`, counts and minutes of synthesis from the `jobs` table, and the
shift's remaining budget for tonight. Never dollars: M3 recorded that the CLI's cost figure is
notional under subscription auth and is not to be shown as money.

**The self-check inserts no row when the daily verdict stands** — the restored verdict lives in
the runner's memory — so the table stops filling with restarts.

## Phase 2 — The shift

A `shift.rs` beside `jobs.rs`. Migration `0016`: `shift_runs(id, started_at, finished_at,
trigger TEXT — idle|launch|manual, steps TEXT — JSON, jobs TEXT — JSON ids, stopped_by TEXT —
done|budget|rate_limit|paused|sign_in|error, owner_pid)`. Settings: `shift_enabled` (on),
`shift_start` (21:00), `shift_end` (06:00), `shift_idle_minutes` (20), `shift_guides_per_night`
(2), `shift_digests_per_night` (4), `shift_paused` (off), and `shift_in_dev_build` (off).

**When it runs.** A thread started at launch — the watchdog in `jobs.rs` is the precedent —
checks once a minute: enabled and not paused; inside the window; the machine idle at least the
threshold (`HIDIdleTime` through a bounded `ioreg`); no run started today; and this process is
the one that runs shifts — the installed app, unless `shift_in_dev_build` is on — recorded by
`owner_pid` on the run row so two builds on one database never both start one. At launch, a
missed window since the last run starts a catch-up run at once, trigger `launch`, the pause
toggle its only brake. `Run the shift now` in the Job Center and the tray is trigger `manual`.

**The plan**, in order, each step an existing path and each recorded on the run row with its
outcome:

1. **Sync Canvas** through the launch sync's quiet path; a sign-in needed ends the step, not the
   run, and is named in the summary.
2. **File** — M33's auto-filing runs inside the sync; nothing new here.
3. **Extract** — `spawn_pipeline` for every class, as the scan already does.
4. **Distill** every applied contribution whose corpus note is missing, then every session
   document without a `.hints.json`, oldest first, up to `shift_digests_per_night`.
5. **Rebuild** guides: every division with sources whose guide is stale or absent and whose
   meeting this week is in the past, oldest stale first, up to `shift_guides_per_night`; never
   the master.
6. Later milestones add steps here — recordings and announcements (M35), briefs, workbooks,
   pre-reads and note reviews (M36), a quiz before a quiz (M37) — each a function that lists
   its work and a cap.

Jobs go through the existing enqueue guards (one per kind and scope); the shift waits for each
job to settle before enqueuing the next so the caps are real and the concurrency setting stays
in charge. The runner's `rate_limit_event` handler sets a flag the shift reads between jobs
and, when the event says the limit is reached, the shift stops with `rate_limit` and leaves the
rest for tomorrow. While a run is under way, the app holds `/usr/bin/caffeinate -i -w <pid>` as
a child and ends it with the run.

**What it says.** The Job Center's pill reads `Shift · step 4 of 5` while it runs and the panel
lists the plan with each step's outcome; the last run's summary sits at the top of the panel
until the next. Two notifications through `tauri-plugin-notification`: `Shift finished · 2
sessions distilled, 1 guide rebuilt` and `A job failed · <kind> <scope>`, each a setting.

## Phase 3 — Always there

`tauri-plugin-autostart` registers the app as a login item behind a Settings toggle, off until
switched on. A tray item shows the next meeting and the shift's state, with `Open ClassHub`, `Run
the shift now`, `Pause tonight` and `Quit`; closing the window hides it instead of quitting, so
the shift's thread survives the window, and `Quit` is the tray's or ⌘Q.

SPEC §6 gains the shift and the per-kind pair; §7.2's "never on a timer" and "nothing here should
introduce a scheduler", §7's "guide synthesis is never automatic", §8.1–§8.4's "manual trigger"
and §15's "never automatic on ingest" become what is then true: a digest and a division guide
run on a click or in the shift, within its caps; the master runs only on a click; §12 gains the
tray, the meter and the settings; §13 the two plugins.

## Acceptance

- `npx tsc --noEmit` clean; `cargo test` passes, with new tests for the decision (window, idle,
  once a day, paused, another process's run), the plan's selection against `memory_db` fixtures
  (a contribution without a note is listed; a division whose meeting is tomorrow is not; the
  master never; the caps hold), the rate-limit stop, and the per-kind settings fallback.
- Live, on the dev build with `shift_in_dev_build` on, the window set to now and the idle
  threshold to zero: the shift runs its plan on whatever real work is pending — capped for the
  session at two digests and one guide — or, when nothing is pending, on one fixture `.md`
  lecture under a week folder with the guide cap at zero; the run row records the steps and the
  jobs; the Job Center shows the plan and its outcome; the notification fires; a second trigger
  the same night does nothing; `Pause tonight` stops the plan between jobs.
- `caffeinate` is a child of the app during the run and gone after.
- A sort job spawned with a per-kind override shows that model in its log's init event.
- The self-check inserts no row on a second launch the same day.
- The login item registers and unregisters from Settings; the tray's items work; closing the
  window leaves the process running.
- The installed app, once installed, runs the shift and the dev build does not.
- SPEC §6, §7, §7.2, §8, §12, §13 and §15 state the design; §14's box is ticked; the notes carry
  the measurements, what was built, what was verified, what was left, and the gotchas.

## Watch for

- **The budget.** The shift's live run spends real digests and guides where work is pending —
  that is the milestone — capped for acceptance at two digests and one guide, and a fixture
  digest only when nothing real is pending. No chat turn.
- **Two processes, one database.** Only one process runs shifts; the enqueue guards already
  dedupe jobs, but two shifts would fight over the caps. `owner_pid` on the run row is the lock;
  a row whose owner is gone is settled at launch like a job's.
- **Never edit under `src-tauri/` while shift jobs run in the dev build**, and remember the
  shift starts jobs on its own now: check the Job Center before touching the tree.
- **The machine asleep at the window** runs nothing; catch-up at launch covers it. `pmset` stays
  untouched.
- **A rate-limit event mid-job** lets the job finish; the shift stops before the next.
- **Synthesis shares limits with interactive Claude Code.** The window and the caps are the
  controls; the meter is what makes tonight's queue visible before starting a session.
- **The Canvas step needs a stored session**; a shift with none says so and goes on.
