# ClassHub — Implementation Notes

Session-to-session log, one section per milestone. `SPEC.md` remains the source of truth;
these notes capture decisions, conventions, and gotchas future sessions need but the spec
doesn't cover.

## M1 — Scaffold + dashboard (2026-08-22)

### What exists now

- Tauri v2 + React 19 + Vite + TypeScript strict + Tailwind v4 + shadcn (new CLI v4.19,
  radix base, nova preset).
- SQLite at `~/Library/Application Support/com.danny.classhub/classhub.db`. Migration
  `src-tauri/migrations/0001_init.sql` embeds the full SPEC §5 schema plus class/meeting
  seed data. Migrations are tracked via `PRAGMA user_version` and each runs in its own
  transaction (`src-tauri/src/db.rs`).
- The `aibhs_root` setting is seeded from Rust with `INSERT OR IGNORE` (not in the SQL
  migration) because the expanded home path can't be known in static SQL.
- One Tauri command: `list_classes` — classes + meetings + `folderPresent` (checks
  `<aibhs_root>/<folder_name>` on disk). All payloads use serde camelCase.
- Dashboard renders 4 accent cards. Next-meeting math is client-side in
  `src/lib/schedule.ts`: ISO weekdays 1=Mon..7=Sun, a meeting today rolls to next week
  once its end time passes, in-session is detected between start and end.
- TanStack Query owns all async server state. No `useEffect` anywhere (workspace rule).
- Verified in both themes via screenshots; all four AIBHS class folders discovered.

### Design system (keep consistent in future UI milestones)

- Identity: native-macOS "schedule console". System SF Pro (`font-sans`) for UI text;
  SF Mono (`font-mono`) for everything temporal — dates, times, countdown chips,
  eyebrows. Keep this register split: if it's a time, a date, or a status code, it's mono.
- Tokens live in `src/index.css`. Neutral cool-gray surfaces; the four class accents are
  `--class-blue/orange/green/amber` with separate light/dark values, exposed to Tailwind
  as `color-class-*`.
- Dark mode is media-based (`prefers-color-scheme`) with no toggle; `@custom-variant dark`
  uses the block form wrapping `@media`.
- Per-card accent pattern: the card sets `--accent` inline from the class color; children
  style with `bg-(--accent)`, `text-(--accent)`, `bg-(--accent)/12`. Reuse this for any
  future per-class theming (workspace header, badges, etc.).
- Signature element: the M T W T F weekday tick row on each card, meeting days lit in the
  class accent. The countdown chip inverts to solid accent while a class is in session.
- Card meeting block is two lines (time + chip, then room + credits) — a single line
  truncates at card width.
- Window chrome: overlay title bar with hidden title; a fixed `data-tauri-drag-region`
  strip (h-9) at the top keeps the window draggable; content clears the traffic lights
  with top padding.

### Gotchas

- `create-tauri-app` refuses a non-empty directory: scaffold into a subfolder, then move
  contents up.
- shadcn CLI v4.19 changed flags (`-b radix -p nova`; old `--base-color` is gone) and its
  registry call needs full network access. The nova preset adds
  `@fontsource-variable/geist` — removed; Geist is the templated shadcn look and the
  design uses system fonts (no CDN, nothing bundled).
- Agent-run `npm run tauri dev` redirects the cargo target dir to a per-sandbox cache
  path — harmless, but a Cursor restart (e.g. granting macOS permissions) kills the
  background dev process tree. The DB persists regardless.
- Vite dev does not type-check; TypeScript errors surface through IDE diagnostics only
  (`tsc` must not be run per workspace rules).

## M2 — Class workspace (2026-08-22)

### What exists now

- **Scanner** (`src-tauri/src/scanner.rs`): recursive walk of a class folder producing
  both the `files`-table sync (upsert added/changed, delete removed, in one transaction)
  and the nested tree payload for the UI in a single pass. sha256 is skipped when
  size+mtime match the indexed row (hash reused). Kind mapping per SPEC §5
  (`pptx|pdf|rmd|r|html|md|other`, case-insensitive extensions; `htm` counts as html).
- **Exclusions**: app-managed dirs (`Study Guides/`, `Notes/`, `_Inbox/`) are skipped at
  class-folder top level only — deliberate, so a user's module can legitimately contain
  e.g. a `Notes` subfolder. Hidden entries (`.classhub`, `.DS_Store`) and symlinks are
  skipped at every depth.
- **Scan triggers** (SPEC §7 step 1): launch (Rust `setup` scans all classes, missing
  folders skipped with a stderr note), window focus (TanStack Query's default
  `refetchOnWindowFocus` — no code), and the RESCAN button (`refetch()`).
- **Commands**: `scan_class` (syncs + returns tree), `reveal_in_finder`,
  `open_in_default_app`. Row actions run the opener plugin from Rust
  (`tauri_plugin_opener::reveal_item_in_dir` / `open_path`), so no new capability
  permissions were needed. `resolve_rel` rejects non-`Normal` path components
  (traversal) and empty rel_path resolves to the class folder itself (used by the
  empty state's "Open folder in Finder").
- **UI**: dashboard cards are now clickable (stretched invisible button, accent
  focus-visible outline, accent bar widens on hover); view switching is plain `useState`
  in `App.tsx` (no router). `ClassWorkspace` header reuses the card's 3px accent bar +
  mono meeting/room/credits eyebrow in the class accent. `FileTree` renders depth-0 dirs
  as module sections (accent chevron, mono `N FILES` count), nested levels behind
  hairline indent guides, file rows with kind icon + mono size, hover/focus-revealed
  row actions. `SCANNED h:mm AM` stamp comes from the query's `dataUpdatedAt`.
- A failed row action (file vanished since last scan) triggers a refetch instead of an
  error UI — the tree self-corrects.

### Verified

- Launch scan indexed exactly Biostatistics Module 1's 11 real files; stored sha256
  spot-checked against `shasum -a 256`. Adding a file in Finder → rescan picked it up
  (row + tree) within seconds; removing it → row deleted, count back to 11. Decoy files
  placed in all four app-managed dirs were excluded; `.DS_Store` never indexed.

### Gotchas

- **macOS 27 renamed the Accessibility pane**: scripted-control permissions now live
  under System Settings → Privacy & Security → Device Control and Data Access, and
  Cursor is granted (owner-verified). A `-1719` assistive-access failure mid-M2 turned
  out to be transient — if System Events errors, retest before assuming the grant is
  gone. `screencapture -x` works regardless. Scripted clicks: `tell process "classhub"
  to click at {x, y}` works only while the app is actually frontmost — verify focus
  first or the click lands on whatever owns the screen there.
- sha2 is 0.11 (not 0.10): same `Digest` API, but hash via a manual chunked
  `read`/`update` loop rather than `io::copy` (no reliance on the `std` Write impl).
- `files` upsert deliberately leaves `extract_*` columns untouched on content change —
  staleness in M4+ compares `extracted_sha256` vs current `sha256`.
- Tauri v2 auto-camelCases command args (`class_id` ↔ `classId`) and serde payloads use
  `rename_all = "camelCase"` as in M1 — keep both conventions.

## M3 — Claude Code job runner (2026-08-22)

### What exists now

- **Runner** (`src-tauri/src/jobs.rs`): `JobManager` in Tauri state — `VecDeque` queue +
  running map, max 2 concurrent, worker `std::thread`s. Spawns `~/.local/bin/claude -p`
  per SPEC §6: cwd = class folder, `--add-dir`, per-kind `--allowedTools`. **Every spawn
  `env_remove`s `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`** (§1).
- **Model policy (owner decision, 2026-08-22, supersedes SPEC §6's per-kind models):**
  every job runs `--model opus --effort xhigh` (Opus 5, extra-high effort) by default.
  The CLI's `--effort` accepts low|medium|high|xhigh|max. **Model selection and effort
  level must become user-configurable in Settings when it is built (M11)** — wire the
  Settings values into the spawn args in place of the `DEFAULT_MODEL`/`DEFAULT_EFFORT`
  constants in `jobs.rs`.
- **`--disallowedTools Bash,WebFetch,WebSearch` is passed on every spawn** in addition
  to `--allowedTools`. Verified on claude 2.1.237: allowedTools is additive and does
  NOT restrict tools the user's own permissive config allows (a probe scoped to
  `Read,Glob,Grep` happily ran Bash). Deny rules win; §6's "never allow" needs them.
- **Streaming**: raw stream-json lines → `<app-data>/logs/job-{id}.jsonl` (`log_path`
  column); parsed as `serde_json::Value`; condensed `ProgressEvent`s
  (status/text/tool/tool_result/retry/result/error) buffered in-memory per session
  (capped 2000/job) and emitted as `job://{id}/progress`. `session_id` captured from
  the init event. A bare `jobs-changed` event fires on every state transition.
- **Cancel**: child handle in `Arc<Mutex<Option<Child>>>`; cancel = flag + `kill()`,
  worker finalizes the row as `cancelled`. Queued jobs cancel by queue removal (no
  spawn). `startup_recovery()` marks rows left queued/running by a dead process as
  failed ("interrupted by app restart") before the first scan.
- **Self-check** (kind `self_check`, no tools, sonnet, cwd = app-data dir): verdict
  from the init event's `apiKeySource` — anything but `"none"` kills the child
  immediately (before any API call) and fails the check; `"none"` must then reach a
  successful result event. `rateLimitType` from `rate_limit_event` (`five_hour` =
  subscription rolling window) is recorded as evidence in the summary. State lives on
  `JobManager`; `auth-check` event + `get_auth_check`/`run_auth_check` commands.
- **Commands**: `list_jobs` (last 50, LEFT JOIN classes for name/color),
  `run_test_job` (M3 throwaway `probe` kind), `cancel_job`, `get_job_events`
  (snapshot for late subscribers), `get_auth_check`, `run_auth_check`.
- **Frontend store** (`src/lib/jobs.ts`): module-level external store consumed via
  `useSyncExternalStore` (push-based Tauri events don't fit TanStack Query; still no
  `useEffect`). Listens to `jobs-changed`/`auth-check` globally and
  `job://{id}/progress` per active job; a per-job seq-keyed map dedupes the
  live-listener vs `get_job_events` backfill race (listener attaches first). A 1s
  interval ticks `nowSec` only while a job is active (elapsed labels).
- **Job Center** (`src/components/JobCenter.tsx`): bottom-center pill — mono register,
  pulsing accent dot + ticking mm:ss while running, `✕ JOB FAILED` after an idle
  failure — expanding to a floating panel: rows with per-class `--accent`, status
  glyphs, cancel `✕` on active rows; output pane renders condensed lines with glyph
  prefixes (`·` status, `»` tool, `←` tool result, `↻` retry, `✓`/`✕` outcome),
  stick-to-bottom scrolling via ref callback + `onScroll` (no effects).
  `AuthWarning`: blocking `alertdialog` overlay when the check fails, with Re-run.
- **TEST JOB** button in the class workspace header is the M3 throwaway trigger —
  remove when M4 lands real job actions.

### Verified

- Probe on Biostatistics end-to-end: live streamed init/tool/result events in the
  panel, succeeded with `TOTAL: 13 files`, `session_id` stored, raw log persisted.
- Mid-stream cancel: child killed (`pgrep` count 0 after), row `cancelled`; a queued
  job cancelled before ever spawning. Three probes at once → 2 RUNNING + 1 QUEUED.
- Stripping proof: entire dev session ran with `ANTHROPIC_API_KEY=sk-ant-dummy-not-real`
  in the app's parent env; self-checks still reported `apiKeySource none` +
  `five_hour` window. Negative test (env_remove temporarily disabled): init showed
  `apiKeySource ANTHROPIC_API_KEY`, child killed pre-API-call, blocking overlay
  appeared, Re-run check requeued a fresh self-check.
- Light mode not re-screenshotted this session (owner was actively using the machine;
  no system-appearance flipping). Job Center uses only M1 tokens, no new colors.

### Gotchas

- `--allowedTools` is additive, not restrictive — always pass `--disallowedTools`
  for least privilege (see above).
- An invalid `ANTHROPIC_API_KEY` makes the CLI retry 401s up to 10× with exponential
  backoff (minutes of grinding); the self-check's early kill avoids this. Regular
  jobs surface `api_retry` progress events so stalls are visible.
- Stream order is not guaranteed: `rate_limit_event` arrived before init in one run
  and after it in another. The parser assumes nothing about ordering.
- `total_cost_usd` in result events is notional under subscription auth — nothing is
  billed; don't surface it as money.
- vite watches `.env` and restarts the dev server when it changes. The repo `.env`
  (owner-added, git-ignored) is never read by the app — the chat key goes into the
  Keychain in M7.
- A re-run self-check queues behind running jobs (max-2); fine today, revisit
  priority when long M5/M6 jobs exist.

## M4 — Ingestion & extraction (2026-08-22)

### What exists now

- **Pipeline** (`src-tauri/src/extract.rs`): `spawn_pipeline(app, class_id)` runs on a
  background thread after *every* scan (launch scan in `lib.rs::scan_and_extract_all`,
  and the `scan_class` command). Staleness = `extracted_sha256 IS NULL OR != sha256`
  per file; a no-change scan finds nothing stale and does zero work / zero tokens.
  Runs are serialized by a global `PIPELINE_LOCK`, and a class with a queued/running
  extract job is skipped entirely (guard queried from the jobs table).
- **Routing** by kind: `rmd|r|md` (+ `.txt` extension on kind `other`) → local copy
  with newline normalization; `html` → dependency-free local tag-strip (drops
  script/style/noscript payloads + comments, decodes entities incl. numeric, keeps
  `<pre>` indentation, collapses blank runs); `pdf`/`pptx` → batched claude job.
  Extract path = `.classhub/extracts/<rel_path>.md` (SPEC §4 mirror rule);
  `extract_rel_path` is stored class-relative including that prefix.
- **PPTX→PDF** (SPEC §7 step 2): `soffice --headless --convert-to pdf` into the
  extracts mirror dir, then rename `<stem>.pdf` → `<name>.pptx.pdf` (soffice names
  output after the stem). A `<name>.pptx.pdf.sha256` sidecar records the source hash;
  conversion is skipped when the PDF exists and the sidecar matches — verified: the
  re-extract run reused the PDF with no soffice spawn.
- **Extract job**: one job per class per run covering all stale PDFs. Prompt template
  `src-tauri/prompts/extract.md` (`{files}` placeholder, `include_str!` + replace):
  faithful/complete markdown, structure/tables/LaTeX/code, every figure described in
  brackets, `.pptx.pdf` treated as slide decks (`## Slide N` sections), DONE/FAILED
  reply lines. The batch manifest rides as `QueuedJob.payload` (JSON, in-memory only —
  restart-safe because startup_recovery fails interrupted jobs).
- **Record keeping** (§7 step 4): local extracts update `extract_rel_path` /
  `extracted_at` / `extracted_sha256` immediately; claude batches are finalized by
  `extract::finalize_job` which runs **before** the job row leaves `running`
  (jobs.rs), so a scan can never observe "no active job + stale columns" mid-window
  and double-enqueue. Missing/empty outputs are left stale for the next scan; the
  recorded `extracted_sha256` is the enqueue-time hash, so a source edited mid-job
  stays stale and self-heals.
- **Staleness groundwork** (§7 step 5): `extract::ManifestEntry`,
  `current_manifest(conn, class_id, scope)` (`'master'` = whole class, else module
  rel-path prefix) and `manifest_is_stale(stored_json, current)` (set comparison,
  unparseable = stale) are ready for M5 badges — currently `#[allow(dead_code)]`.
- **Job Center**: tool_result rows now label text lines *and* images
  (`1 line · 57 images`), and carry the actual result text (trimmed, 4000-char cap)
  in a new optional `detail` field on `ProgressEvent`, rendered behind a native
  `<details>` disclosure (chevron rotates, hairline-indented `<pre>`). PDF reads
  return rendered page images with ~no text, which is why the old label said
  "1 lines" perpetually.
- The M3 TEST JOB button, `run_test_job` command, probe kind, and store action are
  gone; extract jobs are the real trigger.

### Verified

- Real Biostatistics Module 1: 9 text/html files extracted locally (zero tokens,
  spot-checked: entities decoded, R code intact), pptx converted (2.2 MB PDF +
  sidecar), one batched job extracted both PDFs — the 57-slide deck produced
  per-slide sections with 39 bracketed figure descriptions grounded in the rendered
  pages; live streaming observed in the Job Center. Relaunch with no source changes:
  all four classes logged `nothing stale — zero work`, jobs table shows no extract
  job after the batch (only a startup self_check) — zero tokens spent.

### Gotchas

- **poppler is a hard requirement** (`brew install poppler`): the claude CLI renders
  PDF pages via `pdftoppm`. Without it, Read silently degrades to text-only and
  figure "descriptions" are fabricated from prior knowledge, not observed — the
  first extracts had to be invalidated and redone after installing it. SPEC §2
  doesn't list it (checkbox-only edit rule); treat it as an M4 prerequisite anyway.
- LibreOffice via brew needed a `brew update` first (cask DSL newer than local brew),
  and Gatekeeper SIGKILLs the quarantined binary on first run (exit 137 + "Not
  Opened" dialog) — fixed with `xattr -dr com.apple.quarantine`.
- soffice's `-env:UserInstallation=` URL must be percent-encoded: the app-data
  profile dir lives under `~/Library/Application Support/…` and the raw space
  aborts soffice with a UNO RuntimeException (SIGABRT). Profile dir keeps headless
  runs independent of any open LibreOffice GUI.
- The tauri dev watcher rebuild **kills the app mid-job and orphans the claude
  child** (it keeps running unsupervised). Don't save `src-tauri/` files while an
  extract job is running; frontend saves are safe (Vite HMR only).
- Owner instruction (2026-08-22): generated user-facing documents (module/master
  guides, practice exams) are design surfaces. Any session authoring their prompt
  templates must read the frontend-design skill and bake its guidance into the
  template's design contract — not just into app chrome.
