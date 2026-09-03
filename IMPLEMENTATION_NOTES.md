# ClassHub — Implementation Notes

Session-to-session log, one section per milestone. `SPEC.md` remains the source of truth;
these notes capture decisions, conventions, and gotchas future sessions need but the spec
doesn't cover.

## M1 — Scaffold + dashboard (2026-08-22)

### What exists now

- Tauri v2 + React 19 + Vite + TypeScript strict + Tailwind v4 + shadcn (new CLI v4.19,
  radix base, nova preset).
- SQLite at `~/Library/Application Support/ClassHub/classhub.db`. Migration
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

## M5 — Module study guide synthesis (2026-08-22)

### What exists now

- `prompts/module_guide.md` — the §8.1 contract plus a full design contract for
  the generated document (cool near-white paper, serif display over system sans,
  mono apparatus, class accent injected as oklch pairs mirroring `src/index.css`
  tokens, yield-rail signature element, hand-authored SVG style rules, MathML-only
  math, print stylesheet). Placeholders: `{class}` `{module}` `{output}`
  `{accent_light}` `{accent_dark}` `{generated_at}` `{files}` `{manifest}`.
- `guides.rs`: `synthesize_module` (rejects a duplicate active job for the same
  class+scope; captures the manifest at enqueue into the job payload),
  `finalize_job` (verifies a non-empty output file, then upserts `guides` — runs
  inside the job runner *before* the row leaves `running`, same ordering trick as
  extract), `list_guides` (staleness computed on demand via
  `extract::manifest_is_stale`), `stale_guide_count` (feeds `ClassCard.stale_guides`
  in `list_classes`), `read_guide`. Learner work is flagged by an `Edited Files`
  path component — a heuristic; revisit if Daniel's folder conventions change.
- `jobs.rs`: `module_guide` finalize branch **demotes success to failure** when no
  guide got recorded (unlike extract's log-and-continue) — a "succeeded" job with
  no visible guide would be a lie. New: every spawn now passes
  `--strict-mcp-config`; the user-level claude config was leaking MCP servers (web
  search, playwright) into jobs — `--disallowedTools` does not cover MCP tools.
  Verified post-fix: the init event's tool list has zero `mcp__` entries.
- `generated_at` display label is formatted by the frontend at enqueue time
  (std Rust can't format local time; chrono wasn't worth one label). The DB's
  `guides.generated_at` (unix, set at finalize) is the truth for the viewer chrome.
- Frontend: `lib/guides.ts`; FileTree module rows carry a guide cluster —
  `SYNTHESIZE GUIDE` → pulsing `SYNTHESIZING…` → `VIEW GUIDE` (+ hover-revealed
  resynthesize icon) or amber `STALE — RESYNTHESIZE`. Amber (`class-amber`) is the
  staleness status color at both levels; the dashboard card bottom line shows
  `GUIDE STALE` / `N GUIDES STALE`. The guides query is keyed on
  `[classId, settledGuideJobs, tree dataUpdatedAt]` — no polling, no useEffect;
  the finalize-before-status ordering guarantees the refetch sees the new row.
  `GuideViewer` is a full-window overlay: sandboxed iframe (`sandbox=""`,
  `srcDoc`), drag-region header clearing the traffic lights, generated stamp,
  stale chip, `OPEN IN BROWSER` / `SHOW IN FINDER` (M2 opener commands), Escape or
  autofocused close button. Job Center rows now append the scope to the class name.

### Verified

- Real Biostatistics Module 1 end-to-end: 29-minute opus/xhigh job, 54 turns,
  live streaming in the Job Center (read all 11 extracts including learner work,
  finished with self-verification greps over its own output). Guide: 178 KB,
  all six sections, 8 hand-authored SVGs, 20 quiz answers behind `<details>`,
  12 MathML blocks, 130 citation chips, footer with generated stamp + full
  source manifest.
- Offline contract: loaded in Chromium, `performance.getEntriesByType('resource')`
  is empty; zero `<script>`/`<link>`/`<img>`. The only `http` strings are course
  URLs inside `<pre><code>` (verbatim class R code, never fetched).
- Print: `@media print` emulation renders white/black with print-legible accent,
  figures `break-inside: avoid`, SVGs intact.
- Staleness round-trip: probe file added to Module 1 → RESCAN → module chip +
  card badge both amber; the probe was auto-extracted locally (zero tokens) and
  **no job of any kind was enqueued** (no auto-resynthesis, SPEC §12). Probe
  removed → rescan → both badges cleared.

### Gotchas

- `touch` never flips staleness — manifests compare sha256 sets, so verification
  must add, edit, or remove real content.
- TanStack's `refetchOnWindowFocus` did not fire in the Tauri webview when the
  window was re-activated during verification; the RESCAN button is the reliable
  path after out-of-app file changes. If it bites, drive invalidation from a
  native Tauri focus event.
- The `sandbox=""` iframe ignores synthetic keyboard scrolling (automation
  artifact — trackpad scrolls fine); `<details>` disclosures are native HTML and
  need no scripts, verified in the Chromium load.
- Light-mode screenshots skipped again (owner active on the machine); M5 adds no
  new color tokens — stale chips reuse the existing `class-amber` pair.
- Owner instruction (2026-08-22): the learner is "Daniel" (not "Danny")
  everywhere in prose — SPEC.md prose was updated accordingly (sanctioned
  exception to the checkbox-only rule).

## M6 — Semester master synthesis (2026-08-22)

### What exists now

- `prompts/master_guide.md`: seven-section anatomy — Key concepts (grouped by
  module, yield rail), **Cross-module threads** (the master-only section; with a
  single module it must open with one honest sentence and trace intra-module
  through-lines instead — never invent modules), Diagrams (semester concept map
  required), Formula & code reference, Worked examples, Self-test quiz (≥16),
  Glossary. Same document register as module guides, capstone execution: serif
  display, shared yield rail, thread-device module chips, semester map in the
  header. Two owner-driven contract clauses: **dual register** (every key concept
  stated technically first, then re-explained in plain English) and a **writing
  strategy** section (below).
- **Chunked writing contract**: first Write ends with a literal `<!-- CONTINUE -->`
  before `</body></html>`; each Edit replaces the marker with ≤20–30 KB of markup
  plus the marker; the final Edit removes it; Grep-verify no marker remains. Added
  after run 1 lost two entire ~64K-token single-Write attempts to the CLI's
  per-response output cap (nothing ever reached disk).
- Migration `0002_job_payload.sql`: `jobs.payload TEXT` — the finalize payload now
  survives an app restart, so a master stranded by a crash can still resume.
- `jobs.rs`: **exclusive scheduling** in `pump` — a `master_guide` at the queue
  front waits for running jobs to drain; while one runs (`RunningJob.exclusive`)
  nothing else starts. Master spawns add `--include-partial-messages`;
  `stream_event` content blocks for Write/Edit are tracked into sparse `phase`
  progress events (one per 16 KB written). `--resume <session>` is passed when the
  queued job carries `resume_session`. Guide finalization generalized to both
  guide kinds.
- `guides.rs`: `synthesize_master` (module list derived from the current
  manifest) and `resume_master` (failed master + recorded `session_id` + persisted
  payload required; re-enqueues with `RESUME_PROMPT`). `RESUME_PROMPT` carries the
  chunked-write instruction itself, because a resumed session never re-reads the
  template.
- Frontend: `MasterGuideStrip` between workspace header and materials — states:
  idle (`GENERATE SEMESTER MASTER`), queued (`WAITING FOR QUEUE TO DRAIN`),
  running (phase rail ORIENT→READ→COMPOSE→VERIFY, elapsed clock, ~KB counter),
  failed (`RESUME` + `START OVER` + error line), stale, fresh (regenerate +
  `VIEW GUIDE`). Viewer header says `SEMESTER MASTER`; Job Center renders `phase`
  events with a ◆ and drops the redundant `master` scope suffix.

### Verified

- Real Biostatistics master, end to end, with the most authentic failure drill
  possible: the dev app was killed twice mid-job (see gotcha below), stranding
  `running` rows. `startup_recovery` failed them on relaunch; session id and
  payload survived via migration 0002; RESUME (once scripted, once clicked by the
  owner) continued the **same** claude session `ff9e8270` from the 123 KB partial
  file to `DONE` — session ids stay stable across resume chains.
- Final document: 227 KB, all seven sections in order, threads section opens with
  the honest single-module sentence, 22 `<details>` answers, 55 MathML blocks,
  8+ SVGs, no `<!-- CONTINUE -->` remnant, ends `</html>`. `guides` upsert with
  `scope='master'`; strip flipped to fresh (`GENERATED AUG 22 AT 3:43 PM`);
  renders in the viewer.
- Chunked writing: zero `max_tokens` truncations post-fix (run 1: two 64 K-token
  write attempts lost); the file grew on disk in ~10–22 KB steps, which also makes
  mid-write kills cheap to resume.
- Offline: the only `http` strings are verbatim course `read.csv("https://tinyurl…")`
  lines inside `<pre><code>` (same acceptable class as M5). `@media print` present
  (force-light, break-inside avoidance).

### Gotchas

- **The CLI caps one response at ~64 K output tokens.** A single Write of a
  guide-sized document hits the cap, is discarded, and the model retries the same
  way — an expensive loop that never touches disk. Any prompt contracting a large
  output file must mandate incremental writes. `module_guide.md` predates this
  clause and should gain it when next touched.
- A resumed session never re-reads the prompt template; mid-flight instruction
  changes must ride `RESUME_PROMPT`.
- **Cursor-managed background terminals are not a safe home for the dev app**: two
  teardowns mid-job (no exit footer, no crash report, no panic — the harness just
  stopped the shell), each killing app + claude child. The dev app now runs in the
  owner's own terminal. Related: launching `tauri dev` inside the Cursor sandbox
  produces a windowless zombie — the process lives but cannot reach the window
  server or write the DB outside the workspace.
- `pkill -f "tauri"` also matches macOS's *cenTAURI* system daemons
  (`AppleCentauri*`, root-owned, so the signal fails — but keep patterns tight).

## Post-M6 — Interactivity contract, in-app viewer, live streaming (2026-08-22)

### Contract change (owner decision, SPEC §8.1 edited)

- Generated guides now ship **inline vanilla JS**: interactivity is load-bearing,
  not garnish. Both templates mandate ≥3 genuinely interactive devices (sliders
  driving inline SVG, tooltip readouts, comparison toggles, scored quiz over the
  `<details>` fallback) via ONE `<script>` before `</body>` — no frameworks and
  no network/storage APIs (fetch/XHR/WebSocket/localStorage/cookies banned), so
  the offline contract holds; the offline check is now "no network calls in the
  script", not "zero `<script>`". Docs must stay readable with scripts off and in
  print (static default states required).
- Same pass added to both templates: a distillation bar (selection over
  transcription — run 2 came out 181 KB vs run 1's 227 KB with a scripting layer
  added), a visual mandate (sections 01–06 each carry ≥1 inline visual), an
  overflow guard (only `<pre>` scrolls; chips wrap between chips), and
  `module_guide.md` gained the chunked-writing clause (closing the M6 gotcha).
- `GuideViewer` iframe: `sandbox=""` → `sandbox="allow-scripts"` — **without**
  `allow-same-origin`, so guide scripts run in an opaque origin with no path to
  the app's IPC or storage.

### In-app viewer + live streaming

- `FileViewer.tsx`: full-screen reading room for class materials (html/md/rmd/r
  via `VIEWABLE_KINDS`; filenames in the tree are click-to-view). Backed by
  `read_class_file` (path-validated, 8 MB cap). Markdown renders through `marked`
  into the shared document shell; class HTML notebooks get `allow-scripts`.
- Live mode (WATCH LIVE on the master strip): polls the output file every 3 s and
  rewrites the frame in place preserving scroll. Gated on freshness — until the
  run's first Write lands, the file on disk is still the *previous* generation,
  so the view holds a waiting state until `<!-- CONTINUE -->` appears (or the
  phase hits VERIFY, covering the post-final-Edit window). Header mirrors the
  real stream phase instead of claiming COMPOSING during reads.
- Live source tail (SOURCE drawer): `partial_json` fragments are best-effort
  unescaped (carry buffer for split escapes), kept in an 8 KB rolling buffer per
  job, emitted as `job://{id}/tail` events with `get_job_tail` backfill.
- Window dragging: `data-tauri-drag-region` only fires when the mousedown target
  IS the marked element — any child under the cursor silently defeats it.
  Replaced with explicit `startDragging()` on mousedown (`lib/window.ts`) on the
  top strip and viewer headers; `core:window:allow-start-dragging` added
  explicitly to the capability.

### Gotchas

- **Claude CLI ≥2.1.40 does not stream tool inputs by default.** Even with
  `--include-partial-messages`, the API assembles each Write/Edit input
  server-side and releases its `partial_json` deltas in one burst at block
  completion — the "live" feed jumps a whole chunk at a time. Fix:
  `CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING=1` in the spawn env (set on
  master jobs; documented, works with subscription auth).
- Run 2 (job 70) verified the full new contract end to end: 1 inline script,
  slider + concept-map hover readouts + scored quiz + toggles, 29 `<details>`,
  11 SVGs, zero forbidden APIs, no external refs, clean `</html>`, `guides`
  upsert fresh.

## M8 — Chat write-actions (2026-08-22)

### What exists now

- **Nine write tools** in `tools.rs` beside the four read tools (SPEC §9):
  `upsert_deadline` / `complete_deadline` / `delete_deadline`,
  `upsert_grade_category` / `add_grade_item`, `write_note`,
  `trigger_synthesis`, `generate_practice`, `propose_file_moves`. Thirteen
  schemas ride every loop round (~2k tokens) — one `json!` array still fits
  the loop fine. `MAX_TOOL_ROUNDS` 8 → 12: a real turn reads before it writes.
- **Tool execution moved off the held DB lock.** `tools::execute(app, name,
  input, &ToolCtx)` — each tool takes the connection for exactly its window.
  The M7 shape (execute inside the caller's `with_conn`) deadlocks the moment
  a write tool enqueues a job, because `jobs::enqueue` re-enters the same
  non-reentrant mutex. `ToolCtx` carries `today` (display) and `today_iso`
  (YYYY-MM-DD), both client-formatted; `send_chat` gained `todayIso`.
- **Reversible or auditable, per tool** (the `audit_log` table from migration
  0001 finally earns its keep — no M9 retrofit needed):
  - Deadlines and grades are reversible through their own tools; every write
    also lands an `audit_log` row (`chat.*` actions), and `delete_deadline`'s
    payload carries the entire deleted row.
  - `write_note` parks the replaced content in the audit payload on overwrite
    — a chat write can never silently destroy a note.
  - Synthesis/practice triggers: the jobs table is already the record.
  - `propose_file_moves` is inert by construction: rows in `move_proposals`
    (migration 0003), status `pending`, `source='chat'`, `confidence` NULL
    (sort_proposal jobs fill it in M9). One pending proposal per source file —
    a re-proposal updates the row instead of stacking. Validation: source
    exists on disk, both paths AIBHS-root-relative in the same class, dest not
    app-managed, dest not already existing. Paths are stored class-relative
    (the `files.rel_path` convention).
- **Practice pipeline** (SPEC §8.3): `prompts/practice.md` is an exam-paper
  contract in the guide document family — points ledger as the signature
  (header rubric table, per-question `PTS` tags, sticky self-scoring strip,
  countdown timer), solutions behind `<details>` with citation chips and
  partial-credit notes, citations only inside solutions, chunked-writing
  clause, same one-script/offline constraints. `guides::generate_practice`
  names the file `Practice/<scope> — YYYY-MM-DD.html` (numeric suffix on a
  same-day collision), `finalize_practice` demotes success-with-no-file to
  failure (guides pattern). **No practice table** — the directory listing is
  the record (`list_practice`); dated artifacts have no staleness by design.
- `notes.rs`: `write_note` (title→filename sanitization, 1 MB cap),
  `list_notes`; `list_dir_files` shared with the practice listing. Commands
  `list_notes` / `list_practice` feed the workspace sections.
- **`hub-changed` push**: emitted per successful write with `{area}`;
  `src/lib/query.ts` now owns the shared `QueryClient` and maps areas to
  invalidations (deadlines/grades → classes; notes → notes; proposals →
  nothing until M9). This is the whole no-manual-refresh mechanism.
- **Overview upgrades**: deadlines carry `[#id]` (what amend/complete/delete
  address); the detailed overview adds a per-class Grades line (weights sum
  with ≠100 warning, computed weighted grade per SPEC §11 — renormalized over
  categories that have items); the snapshot header counts pending move
  proposals.
- `chat_system.md`: the write-policy section replaces the read-only one — act
  when asked and recap, ids come from the overview, job triggers only on a
  clear request and reported as queued (never done), moves are proposals only.
- **UI**: class cards carry a nearest-deadline line (accent `DUE <date>` +
  muted title; overdue open deadlines included deliberately — an overdue line
  is the most urgent thing on the card). Workspace gains PRACTICE EXAMS and
  NOTES sections (hidden while empty; `ManagedRow` mirrors FileRow; the
  practice viewer passes `allow-scripts`, so the exam's timer and scoring run
  inside the sandbox). Chat write chips carry ✎ instead of »; settings and
  empty-state copy updated; two write-flavored starters added.

### Verified (through the real chat loop, driven via AX automation)

- **Deadline**: created → card flipped to `DUE SEP 3 11:59 PM · Problem Set 1`
  live; then one three-write turn amended it (audit has before/after), marked
  it done, and deleted it (audit keeps the full row). Deadlines table empty
  again; audit rows 1/7/8/9 tell the story.
- **Note**: a search→read→write turn produced a genuinely grounded quick
  reference — real slide citations, the ★EXAM median-position trap pulled from
  the master guide — and the NOTES section appeared with no refresh.
- **Grades**: three categories + Quiz 1 9/10 through chat; weights line and
  the computed 90.0% both correct. Test rows removed after verification
  (nothing displays grades until M11; the audit rows remain).
- **Synthesis**: chat queued module_guide job 103 — Job Center row and the
  module chip (`SYNTHESIZING…`) live; the model volunteered that the guide was
  fresh but honored the explicit ask. Cancelled from the panel (child killed)
  since completing a redundant resynthesis had no value.
- **Practice**: job 102 end-to-end — 625 s, 19 turns, 71 KB exam: 19
  questions / 100 pts / 90 min, rubric table, per-question PTS tags, sticky
  score strip with 90:00 countdown, MC click feedback, 27 solution citation
  chips; 1 script, 1 style, 0 external refs, 0 forbidden APIs, no CONTINUE
  remnant, clean `</html>`. Questions ground in the real notebooks (the
  Florida county `var`/`sd` output). Renders and runs in the sandboxed viewer.
- **Proposal**: one pending `move_proposals` row (source='chat'), file
  untouched on disk, honest "nothing has moved" answer. Deliberately left in
  the queue as the M9 fixture — M9's acceptance requires chat-proposed moves
  to surface there.

### Gotchas

- **Subscription auth runs on a long-lived setup token**, exported as
  `CLAUDE_CODE_OAUTH_TOKEN` in `~/.zshrc` — the browser-login cloud creds
  expire (and had expired). Spawns keep that var (only `ANTHROPIC_API_KEY` /
  `ANTHROPIC_AUTH_TOKEN` are stripped), and the init event still reports
  `apiKeySource none` + `five_hour`, so the M3 self-check accepts it
  unchanged. Consequence: launch the dev app from an environment that has the
  token — a non-interactive shell does not source `~/.zshrc`, which is exactly
  how the expired-cloud-creds 401 surfaced this session.
- Two tauri dev instances collide on vite port 1420 (both projects hard-code
  it). Coexist without touching repo files:
  `npm run tauri dev -- --config '{"build":{"devUrl":"http://localhost:1425","beforeDevCommand":"npm run dev -- --port 1425"}}'`.
- The connection mutex is not reentrant — never execute a tool while holding
  the guard if it can reach `jobs::enqueue`.
- `json!({...})` borrows interpolated variables (`to_value(&expr)`), so values
  stay usable after building an audit payload — no clones needed.
- Driving the app via AX (for verification): synthetic `click at` coordinates
  do nothing in the WKWebView; `perform action "AXPress"` works. An
  aria-label surfaces as AXDescription, but a button named by its text content
  surfaces as AXTitle (HTML `title` attr → AXHelp) — search both. `result` is
  a reserved word in AppleScript. ⌘J only lands in the chat input when the
  panel freshly mounts (autofocus); after AX presses elsewhere, toggle the
  panel closed and open again before pasting.

## M9 — Drop-to-sort (2026-08-22)

### What exists now

- **`sorter.rs`** owns the whole SPEC §10 flow. Staging (`stage_files`): dropped
  paths are COPIED into `<Class>/_Inbox/` (name collisions get ` (2)` suffixes,
  dropped folders are skipped and reported, originals untouched), then a
  `sort_proposal` job is auto-enqueued unless one is already queued/running for
  the class. `run_sort_job` is the manual trigger (retry after a failure, files
  left in the inbox). The prompt (`prompts/sort.md`) embeds the inbox listing +
  a walked class tree (scanner exclusions mirrored; folders always listed, file
  lines capped at 200) and contracts a bare JSON array per §10.
- **Job plumbing**: `jobs::Outcome::Succeeded` now carries the untruncated
  `result_text` beside the display summary — the 4000-char summary cap would
  corrupt the JSON contract. `sorter::finalize_job` runs in the runner before
  the row leaves `running` (guides pattern): slices the outermost `[...]`
  (tolerates fences/prose), validates each entry (source must exist under
  `_Inbox/`, dest not app-managed/dot/existing/self, confidence normalized to
  high|medium|low or NULL), upserts with the one-pending-per-source rule chat
  uses, and a zero-recorded result demotes the job to failure.
- **Resolution** (`resolve_proposal`): approve = `create_dir_all` the dest
  parents → same-volume `fs::rename` → index update → `audit_log` row
  (`sort.move`, payload: proposalId/classId/from/to/proposedBy/confidence) →
  row `approved` (+ `dest_rel_path` rewritten when the picker overrode it).
  Dismiss just parks the row `dismissed`; the file stays put. Nothing else in
  the app touches files.
- **Index update semantics**: an indexed source (chat proposals) keeps its
  `files` row — `rel_path` rewritten and the extract artifacts (`.md`, and
  `.pdf`/`.pdf.sha256` conversion sidecars) renamed along the SPEC §4 mirror
  rule, so a move costs zero re-extraction. An inbox file was never indexed
  (`_Inbox` is scan-excluded), so approve inserts a fresh hashed row and the
  next scan auto-extracts it as new material.
- **Frontend**: `lib/sorter.ts` — module-level `onDragDropEvent` listener
  (note: the API is `onDragDropEvent`, not `onDragDrop`); the open workspace is
  the drop target via a module variable App sets during render; an external
  store drives the drag-over overlay (pointer-events-none, dashed accent
  frame). `InboxQueue.tsx` renders between the master strip and MATERIALS and
  self-hides when empty: one card per pending proposal (chat rows in the same
  queue) + rows for unproposed inbox files (`AWAITING PROPOSAL` while a job
  runs, `SORT INBOX` header action otherwise, failure line with RETRY).
- **Card anatomy**: filename · chip (`HIGH` accent / `MEDIUM` amber / `LOW`
  outlined / `VIA CHAT` muted — NULL confidence renders no chip) · the route
  line (mono `source dir → dest path` where destination folders that don't
  exist yet are dash-underlined in the accent + a `NEW FOLDER` tag — new-ness
  computed client-side against the tree's dirs) · reasoning · APPROVE /
  MOVE TO… / LEAVE IN INBOX (or DISMISS for non-inbox sources). The picker
  lists existing tree dirs and approves directly with `dest_override`; busy
  state holds until the refetch removes the card.
- **Wiring**: `hub-changed` gained a `files` area. `proposals` invalidates
  `sortState` + `classes` (card badge `N TO SORT` = pending proposals +
  unproposed inbox files, computed by `sorter::pending_count` in
  `list_classes`); `files` (emitted on approve) invalidates `classTree`, which
  rescans and refreshes guide staleness through the tree-keyed guides query.
- Chat surface updated: `propose_file_moves` result text and
  `chat_system.md` now point at the workspace inbox queue instead of "arrives
  in an upcoming milestone".

### Verified (real native drag-drop, end to end)

- Dropped a mixed pair (a `week3_lecture_slides.pptx` copy of the real deck +
  an unrelated 1-page parking-permit PDF) onto the Biostatistics workspace via
  a synthesized native drag: both copied into `_Inbox/`, originals untouched,
  job 105 auto-enqueued, overlay + `PROPOSING DESTINATIONS…` + `AWAITING
  PROPOSAL` states all live. Proposals were genuinely grounded: the pptx →
  `Module 1/Slides/` at medium ("no evidence of a Module 2 yet" — judged by
  name, pptx unreadable), the PDF skimmed and recognized as non-course
  material → class root at low confidence.
- All three resolutions exercised: the pending M8 chat proposal (NULL
  confidence, `VIA CHAT`) approved — `Week 1/` created, PDF moved, `files`
  rel_path + extract_rel_path rewritten, extract .md physically moved, **no
  extract job spawned** (zero tokens); the parking PDF approved through the
  MOVE TO… picker into `Module 1/Reading Material/` (override recorded on the
  row, fresh index row, auto-extracted by job 106 covering only that file);
  the pptx left in inbox (row `dismissed`, file stayed). Two `sort.move`
  audit rows carry full payloads. Badge walked 1 → 3 → 1 → 0 live; the queue
  section self-hid once empty. Test files were then removed from the tree
  (fixture cleanup); the Week 1 move is real and stays.
- Moving a file flips module + master guides stale by design — manifests
  compare `{rel_path, sha256}` sets, and the badges did exactly that.
  Light-mode screenshots skipped again (no new tokens; all chips reuse
  existing accent/amber/muted pairs).

### Gotchas

- **cliclick cannot drag**: each invocation is a fresh process, so a
  `dd:`/`m:`/`du:` split across invocations posts `mouseMoved` instead of
  `leftMouseDragged`, and even single-invocation `-w` drags never started a
  native drag session here. What works: JXA with the ObjC bridge posting
  CGEvents directly — mouseDown, a few 1-px threshold-crossing drags, ~40
  interpolated dragged events (30 ms apart), a hover pause, mouseUp
  (`scratchpad drag.js` pattern). Clicks and AXPress remain fine for buttons.
- Desktop icons don't render on this Mac (hidden desktop items), so a drag
  must be sourced from a Finder window — list view, row coordinates read via
  AX (`text field` values inside `entire contents of window 1`). The screen is
  1168×755 logical and the app fills it; shrink the app window first to expose
  a drag source, and note `Finder select` re-opens a browser window every
  time it runs.
- A staged file can miss an already-running sort job (the prompt embeds the
  inbox at enqueue time). Deliberate: the file shows as `AWAITING PROPOSAL`
  until the job settles, then `SORT INBOX` covers it; finalize skips entries
  whose source vanished meanwhile, so approve-during-job races are safe.

## M10 — Schedule + deadlines (2026-08-22)

### What exists now

- **`deadlines.rs`** — UI-side deadline CRUD mirroring the chat tools exactly:
  the same `valid_due_at`/kind rules (now `pub(crate)` in tools.rs, one
  validation for every surface), audit rows under `ui.*` actions (amend
  carries before/after, delete carries the full row — recoverable, so the UI
  has no confirm prompt), and `hub-changed {area:"deadlines"}` on every
  write. `list_deadlines` returns every class's deadlines joined with
  name/color, due-soonest first; the dashboard strip, the workspace list and
  the card line all filter the one `["deadlines"]` query client-side.
- **Syllabus scan** (SPEC §11). Migration `0004_deadline_proposals.sql` is
  the pending-proposal store: job `result_text` is not persisted, so
  proposals live in a table between job finalize and confirmation (the
  `move_proposals` answer), surviving restarts. `finalize_job` runs in the
  job runner before the row leaves `running` and consumes the **untruncated**
  `result_text` (the sort_proposal pattern; the 4000-char summary cap would
  corrupt the JSON). Parsing reuses `sorter::parse_entries` (now
  `pub(crate)`); entries validate per-entry (malformed costs itself), dedupe
  on (lower(title), due date) within the batch, against existing deadlines of
  any status, and one-pending-per-key. Unlike sort, an empty array and
  all-already-recorded are honest successes with honest summaries — only
  unparseable or all-invalid output demotes the job to failure.
  `resolve_proposal` re-checks for a duplicate at approve time (one may have
  been added since the scan), inserts with `source='syllabus'`, audits
  `syllabus.insert_deadline`. `run_scan` covers a chosen file
  (`scanner::resolve_rel`; the prompt notes the `.classhub/extracts` fallback
  for `.pptx`) or the whole folder (`sorter::walk_tree`, now `pub(crate)`);
  the class's existing deadlines ride the prompt so the model skips them, and
  a duplicate-active guard matches the sort job's. `prompts/syllabus.md`
  contracts the bare JSON array (title/kind/due_at/notes), date-bearing items
  only, dates resolved against the semester, `[]` legitimate.
- `jobs.rs`: `enqueue_syllabus` (scope = the file's rel path, NULL for whole
  folder) and the `syllabus_scan` finalize branch. `db.rs`: `ClassCard`
  gained `final_exam_start` for the countdown chips.
- **`WeekSchedule.tsx`** — the dashboard grid (SPEC §11): the card tick row
  grown into a time-true instrument. Axis = the meetings' span widened to
  whole hours at 26px/hour; even-hour hairline rules with mono gutter labels;
  Mon–Fri columns with meetings as accent slabs (3px bar, name, time when the
  block is tall enough) placed by real start/duration; today's column washed
  with a dotted header; an in-session slab inverts to solid accent (the card
  chip inversion). Header carries the next-class chip (min (daysUntil,
  startTime) across classes; solid inversion while in session); below, exam
  chips (`FINAL IN N DAYS` + start + class) skip already-past exams.
- `DeadlineStrip.tsx` — open deadlines with `daysUntil < 7` (overdue
  included, first, in destructive) as bordered chips with the class accent
  dot; honest empty line otherwise. `Deadlines.tsx` — the workspace section
  between the inbox queue and MATERIALS: rows (done-toggle circle, weekday
  due label, kind chip, `VIA SYLLABUS`/`VIA CHAT` source tags,
  hover-revealed edit/delete), done rows behind an `N DONE` disclosure,
  inline create/edit form (native date/time inputs, mono register), the scan
  picker (WHOLE CLASS FOLDER + tree files), and the proposal confirm cards —
  a card leaves the queue the moment the backend confirms its resolution
  (`resolvedIds`), with `ADD ALL` looping sequentially. `query.ts`:
  `deadlines` invalidates `["deadlines"]` + `["classes"]`; new `syllabus`
  area invalidates `["syllabusProposals"]`. `schedule.ts` gained
  `daysUntil`/`dueDayLabel`/`todayIso` (all parse date parts — never
  `new Date("YYYY-MM-DD")`); `classes.ts` exports `CLASS_ACCENTS`.

### Verified

- Grid vs §5 seed data: exact — Tue carries both slabs (Applied 11:45–2:45
  amber, Fundamentals 4:05–7:05 blue), Wed the 50-min Studio slab (name
  only — too short for a time line), Thu Biostats green, Mon/Fri empty; hour
  labels 12 PM–6 PM; exam chip `FINAL IN 107 DAYS · DEC 7 8:00 PM`
  (2026-08-22 → 2026-12-07 = 107). Saturday session: no today column and the
  NEXT chip correctly points at TUE 11:45 AM Applied Gen AI.
- Full CRUD through the real UI (AX-driven): created `Problem Set 1`
  (2026-08-26T23:59) — the dashboard strip chip and the card's `DUE AUG 26
  11:59 PM` line both appeared with no manual refresh; edit added notes
  (audit has before/after); done → reopen round-trip; delete (audit keeps the
  full row). DB checked at every step.
- **Real syllabus, end to end**: the owner added the actual CAI 5731
  syllabus PDF mid-session (`Syllabus/`). A chosen-file scan (job 114)
  proposed 11 items — all five homeworks (`Due 09/13`…`11/15` resolved to
  2026 dates), all four quizzes with week dates, the project report
  (2026-12-03T23:59, Reading Days) and the oral presentation
  (2026-12-07T16:00) — grounded in the document, nothing invented, nothing
  inserted before confirmation. `ADD ALL 11` inserted every row with
  `source='syllabus'` plus 11 audit rows; the queue emptied live. The
  fabricated Problem Set 1 fixture was then deleted through the UI, leaving
  exactly the 11 real deadlines.
- The new PDF's auto-extract (job 113) ran concurrently with the scan —
  max-2 concurrency exercised; both succeeded.

### Gotchas

- **WebKit `<input type="date">` under AX automation**: it surfaces as
  `AXDateTimeArea` with no readable/settable value. Segments accept typed
  digits with arrow-key navigation between them, but clicking the field opens
  the calendar popover, which swallows Escape and hides the form's buttons
  from the AX search — click another field to dismiss it before pressing
  SAVE. First-click keystrokes landed unpredictably; per-segment typing after
  explicit segment selection is the reliable recipe.
- AppleScript string comparison is **case-insensitive by default**: an
  `ends with "DONE"` matcher pressed "Mark Quiz 1 done" and completed a real
  deadline (reopened through the UI toggle). Wrap matchers in
  `considering case`. Also `expanded` is a reserved AX property — as a
  variable name it throws "Access not allowed".
- The today-column highlight is code-only verified (Saturday session; the
  grid correctly shows no highlight on weekends) — the first weekday launch
  shows the wash + header dot. Light mode skipped again (owner active on the
  machine; M10 adds no new color tokens — accent/muted/destructive pairs all
  date from M1).
- The rebuild Keychain prompt (post-M9 gotcha) appeared once and was denied —
  harmless here; chat was not part of this milestone's verification.

## Post-M10 — Review fixes (2026-08-22)

A two-agent review of the M10 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 8 warnings,
and 12 suggestions; all were addressed as individual commits. What future
sessions should know:

- **A keyed form is load-bearing.** `DeadlineForm` seeds its fields in
  useState initializers; rendered without a `key`, switching edit targets
  reused the instance — old values, new id, wrong row overwritten on save.
  It is now keyed by target (`"new"` or the row id). Any future form that
  seeds state from props the same way needs the same key.
- **Every write+audit pair commits as one transaction** (save, status
  toggle, delete, proposal approval) — the audit row is the undo, so the
  destructive statement must never outlive it. `unchecked_transaction` on
  the shared connection is the established shape (sorter::record_move).
- **A dismissed syllabus proposal stays dismissed**: finalize skips keys
  with a `dismissed` row, and the prompt lists skipped items beside
  recorded deadlines. Adding the deadline by hand is the way back.
- **`valid_due_at` now checks the calendar** (month/day-in-month with leap
  years, hour/minute/second ranges), not just the shape — model-supplied
  scan dates made 2026-09-31 reachable, which rendered as a rolled-over
  day while sorting/deduping as the stored text.
- **ADD ALL is one backend call** (`approve_syllabus_proposals`): each
  card approves independently, skipped ones stay in the queue with their
  reasons surfaced, hub-changed fires once. The scan picker holds a busy
  state while the scan command is in flight (double-click guard).
- **Module homes**: `with_conn`/`emit_hub_change`/`now`/`audit` live in
  db.rs; `parse_entries` in jobs.rs; `walk_tree` in scanner.rs;
  `valid_due_at`/`DEADLINE_KINDS`/title+notes caps in deadlines.rs (the
  domain module — tools.rs imports them). `Task` joined the read-only
  deny list in jobs.rs, which is documented as THE security boundary for
  proposal jobs. `CLASS_ACCENTS` in lib/classes.ts is the only accent map.
- The M10 section above describes the pre-fix dedupe and resolution flow;
  this section supersedes it where they differ.
- Verified after the pass: clean rebuild (zero warnings), dashboard and
  workspace render unchanged, and the critical was re-tested live — edit
  Quiz 1, switch to edit Homework 1, form now shows Homework 1's values.
- The repo has no git remote; review fixes are local commits only.
- **The launch Keychain prompt is fixed at the root** (supersedes the M7
  and Post-M9 keychain gotchas). Cause, in two layers: (1) the dev binary
  was ad-hoc linker-signed — a new code identity every build, so no grant
  could ever persist; (2) the key item was created by the `security` CLI,
  whose items land in the Apple-tools protection partition, which macOS 27
  gates for non-Apple apps regardless of the `-A` ACL. Fixes: dev builds
  are re-signed with the stable Apple Development identity by
  `src-tauri/dev-sign.sh` (wired as the cargo runner in
  `src-tauri/.cargo/config.toml`; identifier com.danny.classhub); `save_key`
  writes through the keyring crate again — delete-then-add, because
  updating an item keeps the old creator's ACL — so the app owns its item;
  and `stored_key` re-owns a foreign-created item once per run, so a single
  password entry on the old item's next read completes the migration and
  every read after that is silent across rebuilds. Caveats: the runner
  signs without entitlements, so lldb cannot attach to the dev binary
  (get-task-allow is absent), and if the Apple Development cert ever
  expires, dev-sign.sh falls back to the ad-hoc build and the prompts
  return until the identity is renewed.

## Post-M9 — Review fixes (2026-08-22)

A two-agent review of the M9 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 15 warnings,
and 7 suggestions; all were addressed as individual commits (9daea3f..a0d87f8).
The highlights future sessions should know about:

- **Read-only job kinds now deny the write tools.** `--allowedTools` is
  additive against the user-level claude config (the M3 finding), so
  `sort_proposal`/`syllabus_scan` could still have used Write/Edit despite
  their Read,Glob,Grep allowlist. `jobs::disallowed_tools(kind)` gives those
  kinds a deny list including Write, Edit, MultiEdit, NotebookEdit. Any
  future read-only kind must be added to that match arm.
- **The approve path is atomic.** `record_move` wraps index update + audit +
  proposal resolution in one transaction (`unchecked_transaction` — the
  shared connection is behind `&`), clears a stale index row occupying the
  destination first, and on failure the rename and any moved extract
  artifacts are reversed. A relocated extract that didn't land clears the
  extract columns so the pipeline re-extracts instead of trusting a dangling
  path.
- **Dismissal is terminal, per path.** Dismissed inbox files stop counting
  toward the badge and are excluded from automatic sort runs (manual SORT
  INBOX includes them again); staging deletes stale dismissed rows for paths
  it just filled, so a re-dropped file is a fresh decision. The sort job's
  automatic runs also skip files with pending proposals — cards under review
  never get silently rewritten; after a sort job succeeds, a follow-up run
  covers files dropped while it was busy.
- Proposal JSON now parses per entry (one malformed entry costs itself, not
  the batch) and the array is located by a bracket followed by `{`/`]`, so
  bracketed filenames in surrounding prose can't break the slice. Staging is
  batch-tolerant the same way, and StageResult (folder skips, per-file
  failures) surfaces as a dismissable, class-scoped notice.
- Frontend: drop notices are scoped to their class; `setDropTarget` is a
  pure assignment (no store notification during App's render); the queue
  renders a neutral state while the tree query is in flight instead of
  claiming every folder is new; the picker preserves a proposed rename.
- The app-managed denylist lives once (`scanner::APP_MANAGED_DIRS`), both
  move validators reject dotted segments at every depth (the scanner hides
  those everywhere), and the queue command is `get_sort_state`.

### Gotchas

- **The Keychain prompt is back on every rebuild.** Despite the M7 `-A`
  ACL note, the rebuilt dev binary triggered the login-keychain password
  prompt on launch twice this session (macOS 27 appears to have tightened
  any-application ACLs for ad-hoc-signed binaries). Denying is safe — chat
  simply sees no key for that run — but the M7 remove-and-re-save advice
  needs revisiting whenever chat is next used from a fresh build.
- The AX tree needs a beat after the webview (re)loads: the first
  entire-contents button search after a rebuild reliably returned nothing;
  the retry a second later found everything.

## M7 — Chat sidebar, read-only (2026-08-22)

### Shape

- `chat.rs` owns the loop, `tools.rs` the four read tools, `prompts/chat_system.md`
  the identity and retrieval guidance, `prompts/chat_followups.md` the follow-up
  pass. Frontend: `lib/chat.ts` (external store, no `useEffect`) and
  `components/ChatSidebar.tsx` (⌘J overlay).
- Two API surfaces, deliberately: the job runner spends the Max subscription
  through the `claude` CLI, the chat spends a console key through `reqwest`. A
  rate-limited synthesis job therefore cannot take the chat down (SPEC §1).
- Blocking HTTP never runs on a Tauri command thread: every request goes through
  a plain `std::thread` (`on_worker`, and the answer thread in `send`).
  `list_chat_models` is `async` for the same reason — sync commands run on the
  runtime, and `reqwest::blocking` panics there.
- Tool summaries are derived from the first line of a tool's own output
  (`Outcome::ok`), so a chip rebuilt from persisted blocks reads exactly like the
  live one. Same idea in `itemsFromHistory`.

### Anthropic API, as of now

- `output_config.effort` — `low`/`medium`/`high`/`xhigh`/`max`, no beta header.
  Omitted means `high`. Settings exposes the ladder plus a "model default" that
  sends nothing, because older models reject the parameter outright.
- `thinking: {type: "adaptive", display: "summarized"}` streams `thinking_delta`
  then one `signature_delta`. The signature must go back **unchanged** with the
  turn or the tool loop loses its reasoning — so thinking blocks are persisted
  with everything else. Models that predate adaptive thinking 400 rather than
  ignore it; `rejects_thinking` catches the first refusal and resends plainly.
- `max_tokens` is required and covers thinking *plus* text. Hardcoding it caps
  effort, so the ceiling is read from `/v1/models` (`max_tokens` per model),
  remembered as `"<model id> <ceiling>"` in settings, and re-learned when the
  model changes. `FALLBACK_MAX_TOKENS` only applies when the list is silent.
- Follow-ups are one small non-streaming call on the last exchange only (not the
  whole conversation) asking for a JSON array of three. Best-effort: a failure
  is logged and costs nothing but the chips. They are **not** persisted —
  `chat_messages` has to stay Messages-API-shaped for replay.

### Keychain

- **A Keychain item's ACL is pinned to the binary that created it, and the dev
  binary is ad-hoc *linker-signed* — every `cargo build` changes its code hash,
  so macOS asks for the login password again.** "Always Allow" only pins the
  build about to be replaced. `save_key` therefore writes through
  `/usr/bin/security add-generic-password -A` (any application), which survives
  rebuilds. The trade is the protection of a `0600` file in `~`; stated in the
  Settings pane. An existing item must be removed and re-saved — the ACL cannot
  be changed after creation.

### Rendering

- **Math must be lifted out before markdown parses.** `_` is emphasis and `\` is
  an escape to a markdown parser, so `$\dfrac{\sum x_i}{n}$` cannot be recovered
  from the HTML afterwards. `liftMath` swaps each span for a placeholder, KaTeX
  renders it, and the HTML goes back in after parsing. The same pass matches code
  spans purely to skip them: `$` in an R snippet is a column selector.
- Streaming fade: the answer is split into settled markdown (whole blocks, never
  inside an open fence) and a `tail` of the chunks as they arrived. Each chunk is
  its own element, so React appends rather than re-mounts and settled text never
  re-fades. Re-parsing markdown per delta would restart every animation in the
  answer — that is the whole reason for the split.

## M11 — Notes + grades + settings + polish (2026-08-22)

### What exists now

- **`grades.rs`** is the grades domain module. The math moved out of tools.rs
  (`weighted_grade` / `weights_line` / `grades_line` / `trim_num`, now
  `pub(crate)`; tools.rs imports them), so chat, the UI commands, and the
  class card all compute the same number. `list_grades` returns categories
  with items, per-category percent (points-weighted), the weights sum, and
  the SPEC §11 current grade. UI CRUD (`save_category` / `delete_category` /
  `save_item` / `delete_item`) follows the deadlines shape exactly: one
  transaction per write+audit pair (`ui.*` actions, deletes carry the full
  rows — a category delete parks every item in its payload), category names
  unique per class case-insensitively because the chat tool addresses
  categories by name. Every write emits `hub-changed {area:"grades"}`, which
  now invalidates `["grades"]` and `["classes"]`; `ClassCard` gained
  `current_grade` (same `weighted_grade` call) rendered as an accent chip in
  the card's badge cluster.
- **`Grades.tsx`** renders the workspace GRADES section between Deadlines and
  Materials (SPEC §12's "tabs" have been stacked sections since the workspace
  took shape — the pattern holds). Ledger layout: mono weight column, name,
  per-category percent (or NO SCORES YET), items nested behind a hairline
  indent, hover-revealed add/edit/delete. The ≠100% warning is the amber mono
  line under the header; the computed grade sits in the header as
  `CURRENT N%`. Both forms are keyed by target (the M10 lesson).
- **Notes editor** (`NoteEditor.tsx`): full-screen overlay in the reading-room
  chrome — mono textarea beside a live preview rendered through FileViewer's
  now-exported `docShell` + `renderMarkdown`, so a note previews exactly as
  it reads. `notes::save_from_ui` shares the write path and audit shape with
  the chat tool (`ui.write_note`, replaced content parked in the payload).
  Closing saves anything unsaved — recoverability in place of a confirmation
  prompt — with one guarded case: untitled non-empty text holds the close
  (title it to keep it, clear it to discard) since that is the only close
  that could silently lose work. ⌘S saves in place; a first save fixes the
  title (the file name comes from it). On save the editor writes the note
  back into the `["classFile", classId, relPath]` query cache — reopening
  seeds from that cache, and a background refetch would land after the
  keyed editor already seeded its state. The `notes` hub area also
  invalidates `["classFile"]` for chat rewrites of a cached note. The NOTES
  section is now always visible (NEW NOTE action, honest empty line) and
  note rows open the editor instead of the read-only viewer.
- **`settings.rs`** owns app-level settings in the settings table:
  `job_model` (opus|sonnet|haiku aliases — the CLI resolves each to its
  current release), `job_effort` (the CLI ladder), `job_concurrency` (1–4),
  and the AIBHS root setter (`~/` expands, the folder must exist — it picks a
  library, never creates one; before/after audited). Defaults stay
  opus/xhigh/2. jobs.rs reads model+effort per spawn (a change applies to the
  next job to start, queued included) and concurrency at the top of `pump` —
  read **before** the manager lock, because `with_conn` must never nest
  inside it. `setting`/`set_setting` moved to db.rs; chat.rs imports them.
- **`Settings.tsx`**: the Settings view (gear beside the dashboard date;
  App.tsx now switches dashboard | settings | workspace and resets scroll per
  swap). Sections: LIBRARY (root path + change form, missing-folder error
  line), SYNTHESIS JOBS (model rows, effort ladder, concurrency chips —
  master exclusivity called out), CHAT. Chat's key/model/effort deliberately
  stay in the chat sidebar's own pane — that is where a mid-conversation
  change happens — so Settings shows their live status and links over via
  `openChatSettings()` instead of duplicating the controls. The Job Center
  header's concurrency label reads the configured value.
- Empty states pass: deadlines/grades/notes sections carry honest one-line
  invitations, materials keeps the M2 drop-target hero — an empty class
  (three of four today) reads as intentional top to bottom.

### Verified

- **Notes round-trip** through real UI typing (AX): the saved file on disk
  matches the typed markdown byte-for-byte (heading, bold, list, code span),
  reopening loads it into the keyed editor, the live preview tracks each
  keystroke, ⌘S stamps `SAVED h:mm PM`, close-saves append edits. Chat
  search: "search my notes for fundamentals-kickoff-checklist" ran
  search_material (1 hit) → read_material → answered with the note's items
  and a clickable citation of `…/Notes/Before the first class.md`.
- **Grade math** on the real CAI 5731 weighting (Assignments 50 / Quizzes 20
  / Project 30, from the syllabus extract's evaluation table): Quiz 1 9/10
  alone → CURRENT 90% (renormalized over the one graded category); adding
  Homework 1 8/10 → 82.9% ((50·0.8 + 20·0.9)/70). The card chip appeared
  live via the grades push. The amber warning read `WEIGHTS SUM 50% — 50%
  UNASSIGNED` mid-entry and cleared at 100. The two fixture scores were
  deleted through the UI (audit rows keep them); the three real categories
  stay, showing NO SCORES YET until the semester produces real ones.
- **Settings**: sonnet/low/1 chosen in the UI → rows in the settings table →
  intact after a watcher-triggered app restart → syllabus_scan job 125's
  init event reported `model claude-sonnet-5` (and finished honestly: all 11
  recorded deadlines rode the prompt, so it proposed nothing). Restored to
  opus/xhigh/2 afterwards. Concurrency reaches `pump` the same way (read per
  call); the Job Center label follows it.
- **Light mode, finally**: system appearance flipped briefly and restored —
  dashboard, cards, Biostatistics workspace, notes listing, the editor split
  (paper-white preview beside the source pane), and Settings all render
  correctly on the light tokens.

### Gotchas

- **`aria-pressed` turns a button into an AX checkbox.** The settings option
  rows and concurrency chips surface as `checkbox`, not `button` — AX
  automation matching only buttons finds nothing on the Settings screen.
- **A `sandbox=""` iframe hides `contentDocument` from its parent.** The live
  preview writes via `sandbox="allow-same-origin"` (still no scripts) — the
  FileViewer live-mode pattern. Changing `sandbox` on a mounted iframe does
  not re-apply to the loaded document; only a remount picks it up.
- react-refresh preserves hook state through an HMR edit of a mounted
  overlay, so a fix like the sandbox change needs a close/reopen to observe.
- This session ran concurrently with the dev-signing session (f47447b) —
  interleaved edits to the same files merged cleanly, and that commit's
  chat.rs companions (keyring-owned `save_key`, once-per-run re-own in
  `stored_key`) ride this milestone commit as its message says. The stable
  signing identity held up in practice: chat read the key across rebuilds
  with zero Keychain prompts all session.

## Post-M11 — Review fixes (2026-08-23)

A two-agent review of the M11 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 6 warnings
(plus 1 pre-existing), and 8 suggestions; all were addressed as individual
commits. What future sessions should know:

- **The Keychain re-own is once-ever and loud.** The delete→re-add window is
  the one moment the key exists only in process memory, so it is gated on a
  persistent `keychain_reowned` settings flag (not once per run), serialized
  behind a mutex so a concurrent `stored_key` can't observe the item
  mid-delete, retried, and a final failure returns an error instead of a
  stderr note. A UI save marks the item app-owned; deleting the key clears
  the flag. `stored_key`/`save_key`/`delete_key` all take the AppHandle now.
- **Note saves commit the audit row before the file write** (the reverse of
  the M8 shape): a stray audit row for a failed write is harmless, an
  overwrite whose previous version was never parked is not. The shared path
  (`notes::write_note`, action-name parameter) also refuses to overwrite an
  existing file it cannot read — non-UTF-8 or unreadable content must not be
  clobbered with `previousContent: null`.
- **The editor addresses files by the backend's canonical `rel_path`, never
  by title.** The sanitizer names the file (`Week 3: recap` lives at
  `Week 3- recap.md`), so `save_note` takes an optional `rel_path` targeting
  the exact file (single-component `Notes/` paths); the title names only new
  notes. Reveals use the same path.
- **Every settings write commits atomically with a before/after audit row**
  (`set_audited`, action name derived from the key), and every spawn-option
  fallback to defaults logs the substitution — a configured sonnet/low
  silently spawning opus/xhigh would land on the subscription window.
- **Raising job concurrency pokes the scheduler** (`jobs::poke` from the
  command, after the Db guard releases) so queued jobs start immediately.
- **Migration 0005** rebuilds both grade tables with CHECK constraints
  (weight 0–100, score ≥ 0, max_score > 0); SQLite can't add a CHECK in
  place, so the recipe is rename-originals → create+copy parent → create+copy
  child → drop child → drop parent, with the child's FK following the rename.
- The backend serves the allowed job model/effort ids
  (`AppSettings.jobModels/jobEfforts/maxConcurrency`); the UI keeps only
  display copy. `list_grades` derives its current grade from the rows it
  already read (same gate as `weighted_grade`); the note preview parse is
  memoized on content.
- **The shared document shell carries a CSP** (`default-src 'none'` plus
  inline styles and data: images): the frame sandbox blocks scripts but not
  fetches, so without it a note embedding a remote image still hit the
  network. Guides and class HTML notebooks have their own shells.
- **Known growth term, accepted:** every note save embeds the full replaced
  content in `audit_log` — that IS the undo mechanism, but a 1 MB note
  edited fifty times is 50 MB of database with no pruning path. Revisit only
  if the file ever gets noticeably large.
- The repo still has no git remote; review fixes are local commits only.

## Post-M11 — app-wide review pass (2026-08-25)

A five-agent read-only review over the whole codebase (~8.6k Rust, ~7.4k TS),
followed by fixes for every finding. What is worth carrying forward:

### The two real security holes were both "the guard exists elsewhere"

- **Answers rendered into the app document with no URL policy.** `marked` only
  runs `encodeURI` on an href, so `[x](javascript:…)` survived intact into a
  `dangerouslySetInnerHTML` in the window that holds the IPC bridge —
  confirmed against the installed marked 18.0.10 with the repo's own renderer
  config. Every *other* HTML surface was already sandboxed; this one was the
  exception. URL policy now lives in `src/lib/answer.ts`, the app window has a
  real CSP (`devCsp` carries the extra dev-server origins), and generated
  guides and class notebooks get a document-level CSP because they must keep
  `allow-scripts` to work.
- **`Task` was denied for read-only job kinds and allowed for write-capable
  ones**, with the reason ("a sub-agent is a path around the parent's tool
  scoping") written next to the list that omitted it.

### `--add-dir` grants read and write together

A synthesis job must read the whole class folder, so `Write`/`Edit` reach every
source file in it; the only thing aiming them at the contracted output path was
prose in the prompt, which is what a poisoned source document overrides. Sources
are now fingerprinted by (size, mtime) before the spawn and compared after — a
run that changed anything outside `Study Guides/` and `.classhub/extracts/` is
demoted to failed, records nothing, and its path list goes to `audit_log`.
`scanner::fingerprint_sources` deliberately skips exactly those two paths; the
test asserts both directions, since a false positive would demote every
successful run.

### Three ways a chat session could become permanently unusable

1. Out of tool budget, the loop dropped `tools` from the final request. The
   replayed history necessarily carries tool_use/tool_result blocks by then, and
   the API rejects that combination — so the guard meant to force an answer
   turned twelve rounds of paid retrieval into a 400. `tool_choice: none` is the
   supported way to say it.
2. `close_dangling_tool_uses` ran only after a run finished, and `send` inserted
   the new question *before* calling it — so the repair, which inspects the last
   row, saw a user row and did nothing. Order matters here.
3. STOP only set a flag, read between decoded lines; a stream that stopped
   delivering bytes parked the worker inside a read it could never finish, and
   the session stayed "still answering" for the life of the process.

### Gotchas found while fixing

- `ProgressEvent` is a DOM global. Dropping the type import from a component
  resolves to `lib.dom`'s version instead of erroring as undefined — the failure
  surfaces as a baffling structural type mismatch, not "cannot find name".
- Adding a `summary` key to a persisted `tool_result` block would ride back into
  the API on every replay, so rebuilt tool chips instead mirror `Outcome`'s
  derivation exactly (chars not UTF-16 units, trimmed, ellipsis on cut).
- The incremental fence-parity in `settle()` must return the item *unchanged*
  when the fence is still open: `text` has not advanced, so its parity has not
  either, and the slice gets recounted next time. Verified equivalent to the
  old full-rescan across chunk sizes and fence layouts before landing.
- Commands are dispatched inline on the IPC thread unless marked
  `#[tauri::command(async)]`; `async fn` is the opposite of what
  `reqwest::blocking` wants, which is how `list_chat_models` ended up parking a
  runtime worker on a network round trip.

### The CSP was verified, and it was wrong

The first version set `script-src 'self'` on the app window. A `srcdoc` iframe
**inherits the embedder's CSP**, and multiple policies intersect rather than
override — so that would have blocked the study guides' and class notebooks'
own inline scripts and rendered every guide inert, killing the §8.1
interactivity contract. Confirmed in a browser before it shipped: *"Executing
inline script violates the following Content Security Policy directive
'script-src 'self''"* at `about:srcdoc`.

`'unsafe-inline'` is therefore required in the window policy, which means the
CSP is **not** what closes the `javascript:` vector — `src/lib/answer.ts` is,
and that is the control to protect. What the policy still buys is exfiltration:
no external script origin, no external `connect-src`, no `object`, no base-tag
hijack, no form posting. The same probe confirmed the other half holds: with the
parent permissive, a guide's own `default-src 'none'` still blocked its
`fetch` (`script=RAN fetch=BLOCKED`), so `withDocumentCsp` does the job it
exists for.

Loading the real frontend under the production policy showed no violations —
Tailwind's injected stylesheet applied (249 rules) under
`style-src 'unsafe-inline'`, and KaTeX's fonts are all relative URLs, covered
by `font-src 'self'`. One deliberate consequence: a remote `https:` image in an
answer passes the renderer filter but is blocked by `img-src`, so the tag
exists and nothing is fetched. That is the desired outcome for a beacon.

Two smaller findings from the same pass. `tauri.conf.json` rejects unknown keys,
so the rationale above cannot live in the config as a comment — it sits in
`lib/document.ts` next to the policy it constrains. And the jobs
`ProgressEvent` type is now `JobProgressEvent`: the bare name is a DOM global,
so a file using it without an import resolved to `lib.dom` instead of failing.

### Deliberately not changed

- `useMutation` is still unused; writes hand-roll busy/error state across seven
  components. The concrete defect inside that finding (a busy flag never cleared
  on success) is fixed. A blanket migration is ~17 call sites of behavioural
  change in UI that cannot be verified without running every flow, and the
  existing pattern is consistent.
- The CSP values are reasoned but unverified — nothing here launched the app.
  First run after this pass should watch the devtools console for a CSP
  violation, especially around KaTeX styles and the iframe shells.

## Post-M14 — One database for every build (2026-09-02)

### What happened

- `/Applications/ClassHub.app` is a release build from 2026-08-25 (before M13, M14 and the
  data-directory refactor `f6a035a`), and it is the app the semester runs on. `f6a035a` renamed
  the data folder from Tauri's default `com.danny.classhub` to `ClassHub`; the installed build,
  still resolving the default name, found an empty folder there and created a second database
  in it (schema 6). Between 2026-08-27 and 2026-09-02 the two diverged: 11 deadlines, three
  sort moves and an extract in the installed build's database; 49 units, 29 deadlines, the
  Canvas mapping and the grade categories in the dev database.
- One consequence was visible as an app bug: the three files the installed build sorted into
  the tree on 2026-09-01 were the same three the dev build's Canvas sync had staged into
  `_Inbox/`, so the dev database held three pending Canvas proposals for files no longer in the
  inbox. Another was a false demotion: those three moves were approved while extract job 4 was
  running, and the write-scope fingerprint counted the app's own renames as the job's writes
  (`job.out_of_contract`, six paths). M17 fixes both mechanisms.

### What was done

- The dev database (schema 8) is canonical. Imported from the other: the three `files` rows it
  alone indexed (with their extract state, so nothing re-extracts), one fresher `files` row for
  an edited `.R` file, its three approved `move_proposals`, and all 15 `audit_log` rows tagged
  `fromDatabase: aug25-build`. The three stale Canvas proposals (ids 9–11) were dismissed. One
  `library.merge_databases` audit row records the counts. `jobs.log_path` values were rewritten
  to the new directory.
- The canonical directory was renamed to Tauri's default, `com.danny.classhub`, and
  `lib.rs::data_dir` now resolves `app_data_dir()` for every build. Both originals are under
  `~/Library/Application Support/ClassHub-merge-backup-2026-09-02/` (`aug25-build-data/` is the
  installed build's whole directory; `dev-before-merge.db` is the canonical database before the
  merge).
- `db::open` sets `journal_mode = WAL` so the installed app and a dev build can hold the file
  open together. The mode persists in the file; the Aug 25 build honours it without knowing.
- The installed build was relaunched on the merged database and verified: it opened
  `com.danny.classhub/classhub.db`, left `user_version` at 8 (its migration runner skips past
  entries it does not have and never lowers the version), ran its self-check as job 285 in the
  shared sequence, and its launch scan found every extract current — no job, no tokens.

### Gotchas

- **The installed build stays two milestones behind until it is rebuilt.** It opens the shared
  database fine, but it has no Canvas, no Structure section and no `Weeks/` filing, and its
  startup recovery still fails every active job row on launch (M15 Phase 1). Reinstall from the
  current commit before relying on it.
- Two builds on one database is now the normal state, not an accident. Any code that infers
  "no other process" — recovery, queue guards, cleanup — has to ask the table who owns a row.
- A bare copy of `classhub.db` under WAL can miss the last transactions; back up with
  `sqlite3 classhub.db ".backup out.db"` or copy the `-wal` and `-shm` files with it.

## M15 — One database for every build (2026-09-02)

### What exists now

- **Job rows own their process.** Migration `0009_job_owner.sql` adds `jobs.owner_pid`, and
  `enqueue` stamps `std::process::id()`. `startup_recovery` is `recover_orphans(conn, self_pid,
  alive)` over the predicate `is_orphan`: an active row is failed only when its owner is NULL (a
  row from the Aug 25 build, whose inserts name their columns and never set it), equal to this
  process's own pid (at startup nothing has been enqueued, so that is a reused pid), or not
  alive by `kill(pid, 0)` — EPERM counts as alive, and pid 0 is refused outright because it
  names the process group and always answers yes. The error text is `interrupted — the process
  running it exited`. The duplicate-active guards (`guides::has_active_job`,
  `sorter::has_active_sort`, `deadlines::run_scan`, `extract::has_active_extract_job`) were not
  touched: they read status from the table and are correct once the rows are.
- **Cancel is per-process, and says so.** `cancel_job` falls through the in-memory `running`
  map and queue to the table: an active row with a different owner returns "job N is running
  in another ClassHub process (pid X) — cancel it from there", which the Job Center's error line
  shows. The manager guard is dropped before the Db lock is taken, keeping the existing order.
- **The self-check runs once a day.** `jobs::startup_self_check` looks for a `self_check` that
  succeeded within `SELF_CHECK_INTERVAL` (24 h). Found, it restores the verdict through
  `set_auth("ok", …)` with the stored summary and its age, so `get_auth_check` reports the truth
  rather than the default "pending"; not found, it enqueues. `run_auth_check`, the manual re-run
  behind the auth warning, still calls `enqueue_self_check` unconditionally. Before this, 242 of
  the 285 job rows were self-checks.
- **`scripts/install-app.sh`, as `npm run install-app`.** Finds the installed instance by
  executable path (`pgrep -f "^/Applications/ClassHub.app/Contents/MacOS/classhub"`, which does
  not match a dev build), quits it by bundle path through `osascript` so the job runner's
  shutdown still reaps its children, waits up to 30 s, runs `tauri build --bundles app`, removes
  the old bundle, `ditto`s the new one in and relaunches with `open`. It warns when the working
  tree is dirty and prints the commit it built. The build precedes the removal, so a failed
  build leaves the installed app in place. SPEC §13 names it as the one production build, and
  §14's session protocol gained it as step 5.
- `libc` is a direct dependency now (already in the lock through tauri).

### Verified

- `cargo test`: 130 pass, three new in `jobs::tests` — the predicate as a table, recovery over
  `memory_db` (live-owned kept, dead / NULL / self-pid failed, a settled row untouched), and
  `process_alive` against this process, a reaped child and pid 0. `npx tsc --noEmit` is clean.
- Two debug processes on the shared database beside the Aug 25 app (pid 8598): `npm run tauri
  dev` (pid 11074) and `target/debug/classhub` launched directly (pid 11179), both on the one
  Vite server. With rows seeded as `running`/11074, `running`/99999 (a free pid) and
  `queued`/NULL, the second launch left the first `running` and failed the other two; its stderr
  named `[287, 288]`, and its launch scan skipped class 1's pipeline (the live-owned extract row)
  while running classes 2 and 4 — the cross-process form of "one changed file, one extract job".
  SIGKILL of 11074 holding a row of its own beside a row owned by 11179, then a relaunch: only
  11074's row failed. Three launches in the day added no `self_check` row; job 285, from the
  installed build's own launch that morning, stood. The seeded rows were deleted afterwards and
  the max id is 285 again.
- Migration 0009 ran under the installed app's open connection without incident
  (`user_version` 8 → 9). The Aug 25 build's rows carry `owner_pid NULL` and any newer build's
  recovery fails them — the intended reading until it is reinstalled.
- The install script's process match returns only 8598, `zsh -n` passes, and AppleScript's
  path-addressed `quit` was checked against TextEdit. The script itself was not run.

### Gotchas

- **Reinstall first.** Until `npm run install-app` runs, the Aug 25 build still fails every
  active row when *it* launches, and none of its own rows say who owns them.
- Concurrency is per process: two builds can each run `max_concurrent` jobs, and master-guide
  exclusivity is per process too. Nothing here makes either global.
- The cross-process cancel message was exercised in review only; nothing scripted can press the
  Job Center's cancel.
- `kill(pid, 0)` cannot tell a reused pid from the original process. The self-pid case is
  handled; a pid reused by some unrelated process would keep a dead job's row `running` until
  that process exits. Acceptable at n-of-1, and the row is visible in the Job Center.

## Post-M15 — Review fixes (2026-09-02)

A two-agent review of the M15 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 5 warnings and
8 suggestions once the orchestrator had downgraded a second critical; all were
addressed as individual commits. What future sessions should know:

- **The duplicate-active guard was check-then-act across processes.**
  `extract::run_pipeline` read the guard, released the lock, converted decks for
  up to minutes, then enqueued — so two launches in the same minute both passed
  it and both enqueued the same batch. `jobs::enqueue_unique` (over
  `insert_unique_job`) now does the check and the insert in one
  `BEGIN IMMEDIATE` transaction, making SQLite's write lock the cross-process
  mutex; `enqueue_extract` returns `None` when it yielded and the pipeline logs
  that. The pre-flight guard stays as an early-out. The user-triggered guards
  (sort, scan, guide) keep their read-then-enqueue shape: the window is
  milliseconds and needs a human in two windows, and SPEC §6 now says that
  rather than "hold across processes". Tested on `memory_db`.
- **Cancel settles ghost rows.** `shutdown` writes no final status and the
  process exits before a worker's terminal write lands, so a peer that recovered
  while the quitting process was still alive kept the rows, and nothing re-ran
  recovery until a relaunch — meanwhile the guards refused that class's work
  and cancel referred the user to a dead pid. The fall-through asks `is_orphan`,
  the same predicate recovery uses: a live owner elsewhere still gets the
  referral; an exited owner, a NULL owner, or this process's own handle-less row
  is settled as `cancelled`. It is the one place besides recovery that writes a
  status without holding a child handle, and it asks who owns the row first.
- **The self-check gate reads the latest verdict.** It had selected the latest
  *success* inside the window, so a failed manual re-run after a morning success
  was hidden at the next launch. `standing_self_check(conn, now)` returns the
  latest settled self-check only when it succeeded within the interval; tested
  with a lapsed success, a fresh one, an in-flight row and a later failure.
  `startup_self_check` returns `Result<()>`, and `SELF_CHECK_INTERVAL` sits
  with the other constants.
- **The install script confirms the bundle exists before `rm -rf`**, quits the
  installed instance by pid through `NSRunningApplication` (`terminate`, the
  Dock's graceful quit, so the job runner still reaps its children), and matches
  the executable path whole with its dots escaped. In JXA a zero-argument
  Objective-C method is a property access: `app.terminate` sends the quit, and
  `app.terminate()` throws *after* the quit has already gone out. Verified
  against throwaway dev binaries; the script itself has still not been run.
- **rusqlite installs a 5 s busy timeout on every connection it opens**
  (`inner_connection.rs`: `sqlite3_busy_timeout(db, 5000)`), which is why two
  processes writing already waited rather than failing; the review's "no busy
  timeout" critical assumed SQLite's zero default and was downgraded. `db::open`
  now sets 10 s explicitly, says why, and reads back `PRAGMA journal_mode`,
  failing startup if the answer is not `wal`.
- WAL sidecar files: no change. The backup guidance already sits in the M15
  brief and the Post-M14 notes.
- The auditor's report was cut off after its third suggestion when the review
  agents were stopped; anything that followed is not reflected here.
- The repo still has no git remote; review fixes are local commits only.

## M16 — The first real lecture (2026-09-02)

### What happened

- The recording was the Fundamentals of AI in Medicine I session of 2026-09-01, as a
  Zoom share link from Canvas, run through the Add lecture form on the dev build with the
  digest left on. The form resolved the date to `Week 02 — Responsible AI, Ethics, and
  Governance`, which is the week the syllabus dates 2026-09-01. Five seconds after ADD
  LECTURE the transcript was filed at `Weeks/Week 02 — …/2026-09-01 — Lecture.md`, its
  `lecture_contributions` row was `applied` on unit 24 spanning 0–9,300,000 ms over 196
  lines, its `files` row carried a zero-token extract, and digest job 286 was running.
- **Zoom.** No sign-in and no passcode were asked for: the share link serves the
  recording-player page to an anonymous GET and the player's store fills in on its own.
  The probe went `waiting` (1.5 s, no store) → `fetching` (4.5 s, store with `ccUrl`) →
  `ready` (6.1 s). The caption is a 102 KB sentence-level WebVTT of 755 cues with no
  speaker prefix on any of them — one Zoom account in a lecture hall records everyone as
  nobody. `zoom.rs` now writes each state change, the route that paid off, the timing, the
  byte and cue counts and the first cue to stderr, and the probe reports `named:N` for
  the `transcriptList` route, because the filed transcript cannot show whether names were
  absent from the page or dropped by the merger. Measured the second time round (see
  Verified) since the first capture pre-dates the logging.
- **Normalization.** 755 cues became 67 paragraphs under 30 `## HH:MM` anchors; 2 h 36 m
  reported in the header. Anchors label paragraph start times, so they run `00:08, 00:17,
  00:20, 00:25 …` — the mic was off for the first eight minutes (the transcript says so)
  and one paragraph spanned 00:15. Zoom punctuates sparsely, so seven paragraphs ran past
  the 1,500-character soft cap (longest 2,449); the digest's `Read` returned them whole.
- **Digest** (job 286): 12.4 min, 5 turns, $3.06 list-equivalent, 69k output tokens.
  It named the session *Clinical Model Evaluation, Fairness, and HIPAA*, wrote the 57 KB
  markdown, the 75 KB HTML in a single `Write` (no output cap hit, so the chunked-writing
  clause was not needed) and the 18 KB corpus note with 46 anchors, whose header states
  that the transcript carries no speaker labels. The HTML has no external reference.
- **Guide** (job 287, `unit:Week 2 — …`): 18.2 min, 34 turns, $5.59 list-equivalent,
  103k output tokens (29k thinking), 160 KB through one `Write` and 13 `Edit`s, six
  sections, self-contained (the two "CONTINUE" hits are SVG label text). It read the
  corpus note once and then the transcript in full across four paged `Read`s, despite the
  prompt's "open it only where the distillation is not enough": with the note as its only
  source it chose the record. It cites `2026-09-01 — Lecture.md · HH:MM` 117 times and
  the note once by path. Left as is — the transcript is ~20k input tokens against 103k
  of output, and the citations do scrub to the recording.
- **Chat**: asked where the instructor said prevalence changes PPV, it searched, read the
  transcript's extract copy (`.classhub/extracts/Weeks/…/Lecture.md.md`, `## 01:05`) and
  cross-cited the session markdown at line 73. Citing the extract rather than the source
  transcript is how every text-route file is cited; cosmetic.
- **Parakeet** was not on the path (Zoom published a caption), so its throughput was
  measured with the app's exact invocation on 73 minutes of 16 kHz speech synthesized by
  `say` from the real transcript: 49 s wall time including the model load, ~0.7 minutes of
  compute per hour of audio, 548 cues.

### What broke, and what was done

- **A refile orphaned the session document.** An approved move rewrote the `files` row,
  moved the extract and re-resolved the contribution, but the `guides` row for the digest
  is keyed `session:<transcript path>` with a manifest naming that path, so after the move
  the Lectures listing showed the transcript as undigested (DISTILL) and the digest as a
  row for a path nothing was at. `lectures::refile_session` rewrites the scope and the
  manifest entry; a stale row already at the new scope is cleared first so the rewrite
  cannot hit `UNIQUE(class_id, scope)`.
- **A refile deleted the corpus note**, by M14 design, pricing every correction at a
  fresh $3 digest of a three-hour lecture for content the move did not change.
  `refile_contribution` now relocates the note into the new unit's corpus folder (and
  removes the emptied old folder); it is removed only when the transcript leaves `Weeks/`.
  SPEC §8.5 states the new design.
- **The Structure row hid a guide whose sources had left.** `UnitRow` rendered the guide
  cluster only when the unit had a folder or a distilled lecture, so after the refile the
  Week 2 row showed neither VIEW GUIDE nor STALE — exactly the state the row exists to
  show. A guide that exists is now always shown.
- **Staleness never refreshed after a move.** The `files` hub change invalidated the tree,
  units and contributions but not `["guides"]`, and the comment there claimed the guides
  query was tree-keyed; it is keyed on the class alone. The badge stayed wherever it was
  until something else refetched. `query.ts` now invalidates guides on `files`.

### Verified

- `cargo test`: 133 pass, one new (`a_session_document_follows_its_refiled_transcript`);
  the refile test now asserts the note moved and is readable at its new path, and is gone
  once the transcript leaves `Weeks/`. `npx tsc --noEmit` clean. The probe harness under
  node still passes with the `named:` entry.
- Every UI state was seen live in the dev build: the form's resolved week and "feeds"
  line; `TRANSCRIPT FILED` with the digest running; `DISTILLING · 2026-09-01 — Lecture.md`
  and the `LECTURE DIGEST 04:26` pill; the session row with its `WEEK 2 — …` badge; the
  Week 2 row gaining `1 LECTURE` and SYNTHESIZE GUIDE; `SYNTHESIZING…`; VIEW GUIDE fresh.
- Refile through a chat proposal (proposal 17, APPROVE): one contribution row, now on
  unit 25 with the note under `corpus/Week 3 — Biomedical Data Foundations/`, the session
  row's scope and manifest on the new path, the extract moved, `sort.move` audited. The
  move back (proposal 18) restored all of it, and the Week 2 guide's stored manifest equals
  the current one byte for byte, so it reads fresh without a resynthesis.
- The same link captured a second time with the digest off filed as `2026-09-01 — Lecture
  (2).md` (the never-overwrite rule) and put a second source under Week 2, which flipped
  its guide to `STALE — RESYNTHESIZE` live; a proposal moving the duplicate to the class
  root cleared its contribution row, and the guide read fresh again live after the
  `query.ts` fix (proposals 19–21; the duplicate and its extract were then deleted and the
  class rescanned). The round trip Week 2 → Week 3 → Week 2 was repeated after a full
  page reload to see stale and then fresh with no remount.
- Final state: one transcript under `Weeks/Week 02 — …`, its note under
  `.classhub/corpus/Week 2 — …/`, the session pair under `Study Guides/Sessions/`, the
  Week 2 guide, one `applied` contribution row, guides rows 3 (session) and 4 (unit). No
  file in the tree was edited by hand.

### Gotchas

- **Two processes named `classhub`.** AppleScript resolves `process "classhub"` by name,
  so with the installed app running every reference — even one built from `unix id` —
  re-resolves to the first one, and the first half-hour of AX driving landed on the Aug 25
  build. The Swift accessibility driver used instead (`AXUIElementCreateApplication(pid)`,
  scratch only, not committed) needs `AXEnhancedUserInterface` set on the app element
  before the web content is exposed. The dialog's `aria-modal` hides the rest of the page
  from AX, which is why "ADD LECTURE" resolved to the form's button while it was open.
- **`tauri dev` rebuilds and relaunches the app on any `src-tauri` change**, which would
  orphan a running job. Rust edits waited for jobs 286 and 287 to settle.
- **Vite HMR of `query.ts` creates a second `QueryClient`** that the provider never sees,
  so a cache-invalidation change looks broken until a full page reload (`touch
  index.html`). Verify frontend cache changes after a reload, not after HMR.
- The contribution row's `summary` reverts to the filing-time placeholder on a refile
  (`contribution_for` re-records the row); nothing displays it, so it was left.
- The Materials tree offers SYNTHESIZE GUIDE on the `Weeks` folder as if it were a module
  folder. Untouched here.
- The installed app is still the Aug 25 build; it was not reinstalled this session and
  was left running throughout (no job of its own was active).

## Post-M16 — Review fixes (2026-09-02)

A two-agent review of the M16 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced no criticals, 6 warnings and
10 suggestions, 4 of them corroborated by both reviewers; all but one pre-existing
suggestion were addressed as individual commits. What future sessions should know:

- **Nothing on disk changes inside the move's transaction any more.** The M16
  refile renamed or deleted the corpus note inside `record_move`'s transaction,
  and the sorter's undo path (`undo_artifact_moves`) knew only the transcript and
  its extract artifacts — a failed audit insert, proposal update or commit rolled
  the rows back and left the note wherever the filesystem had put it.
  `lectures::refile_lecture` (renamed from `refile_contribution`, since it moves
  the session row as well as the contribution) now returns `RefileEffects` — the
  note's `NoteMove` and the documents of any row cleared at the destination scope
  — and `record_move` applies it after `tx.commit()`. A failure there is logged,
  not propagated: the database has already recorded the move. The rollback path
  therefore stays exactly what it was.
- **An occupied corpus destination is refused, inside the transaction.** Notes
  are keyed by unit and transcript name, and a Part spanning several weeks can
  hold two transcripts with one name; `fs::rename` would have replaced one
  distillation with another silently. Tested with two lectures under one Part.
- **A destination week the course has not declared keeps the note.** The arm
  commented "out of `Weeks/`" also fired for an undeclared week or a folder that
  does not parse as one, deleting the distillation only a fresh digest could
  bring back. The note is removed only when the transcript actually leaves
  `Weeks/`; otherwise it stays for the sync that declares the week or the refile
  that comes back, which finds it again.
- **A stale session row at the destination is cleared with its documents,
  whether or not the incoming transcript brings a row.** The guard had run only
  when the source had a row, and even then left the pair under
  `Study Guides/Sessions/` for chat's search to keep finding.
- The digest's title now travels across a refile (`PLACEHOLDER_SUMMARY` marks the
  filing-time value that belongs to the old unit); a session manifest that does
  not parse is left as it is, stale, rather than failing the whole move, matching
  `manifest_is_stale`.
- `describe_caption` counts cues as `transcripts::parse` reads them and shows the
  first cue with its speaker when there was one; page-derived strings in the
  capture log are quoted; the probe harness checks the `named:` counter.
- A Structure row whose guide exists but whose sources are gone shows a plain
  `STALE` label (reason in its title) and VIEW GUIDE, never a resynthesize the
  backend would refuse; the gating flag is `hasGuideCluster`, with `canBuild`
  beside it. The `files` hub change also refetches the class cards, since a newly
  filed lecture emits `files` alone.
- Lecture test fixtures use a per-process scratch folder.
- **Left as is:** the one-row-per-transcript read in the refile (SPEC §8.5 keeps
  the span columns for a course that divides itself finer than it meets, and none
  does). The guide job reading the transcript in full remains the model's call.
- Verified: `cargo test` 137 pass (four new: the collision, the undeclared week,
  the stale destination row, the caption description), `npx tsc --noEmit` clean,
  and the Week 2 → Week 3 → Week 2 refile repeated live on the reordered code —
  note relocated after the commit, emptied folder removed, session row following,
  the Week 2 row reading `STALE` with VIEW GUIDE while its lecture was away and
  fresh once it returned. Nothing was pushed: the repo has no remote.

## M17 — Proposals that tell the truth (2026-09-02)

### What was built

- **Deadline proposals on the card.** `db::ClassCard` carries
  `pending_deadline_proposals` (`deadlines::pending_count`, one COUNT per class on
  every dashboard refetch), the card shows `N PROPOSED` in the badge cluster beside
  `N TO SORT`, and `tools::overview_text` folds the two confirm queues into one line
  ("2 file move proposal(s) and 9 deadline proposal(s) awaiting approval"). The
  `deadlineProposals` hub change now also invalidates `["classes"]`, since the card
  reads the same queue.
- **Past-dated proposals.** Client-side, as the brief asked: a card whose `due_at` is
  before today shows its date in the muted register with a `PAST` tag (not OVERDUE —
  nothing is overdue until it is a deadline), `ADD ALL` sends only the ids of the
  rest, the queue's line says "ADD ALL SKIPS 1 PAST DATE", and the button is not
  rendered once nothing addable is left. The card's own ADD DEADLINE still works.
  `approve_deadline_proposals` is unchanged: one backend call, filtered ids.
- **Vanished proposals.** `sorter::dismiss_vanished` runs at the top of both
  `sort_state` and `pending_count`: every pending proposal of the class — sort job,
  Canvas or chat — whose `source_rel_path` is not a file on disk is set `dismissed`
  with a `sort.proposal_vanished` audit row carrying the whole row. A class folder
  that is not a directory is skipped, because an unmounted volume or a root setting
  mid-change is not a folder every proposed file has left. Chat's `overview_text`
  counts the table directly and is the one reader this does not sit in front of.
- **The write guard ignores the app's own moves.** `jobs::app_written_paths` reads the
  audit log for the job's window and `excluding_app_writes` subtracts those paths from
  the fingerprint diff. The window opens at the first fingerprint, not at
  `started_at`: `execute_job` stamps `started_at` after the "before" picture is taken,
  so a move between the two would otherwise be in the picture and outside the window
  at once. `APP_WRITES` is the table of (action, payload keys) the guard understands —
  `sort.move` (`from`, `to`), `lecture.added` (`relPath`), `canvas.staged_file`
  (`source`), `chat.write_note` / `ui.write_note` (`relPath`) — and the one unaudited
  app write into the guard's walk, a drop staged into `_Inbox/`, now writes
  `sort.staged` with the list. `_Inbox/` and `Notes/` are inside the walk
  (`fingerprint_walk` excludes only `Study Guides/` and dot-folders), which is why the
  note actions are on the list. An unreadable audit log leaves the guard strict, and
  a demotion that did not happen is logged, since it is otherwise invisible.
- **SORT BY CONTENT.** A Canvas card gains the button; `sorter::sort_by_content`
  checks the row is a pending Canvas placement of a file still in the inbox, refuses
  while a sort for the class is active, renders the prompt over that one file
  (`build_prompt` split into its filter and `render_prompt`) and enqueues a sort job
  whose `scope` is the file. `QueuedJob` now carries the row's `scope`, and
  `sorter::finalize_job` receives it: the entry whose source equals the scope goes
  through `override_canvas_placement`, which rewrites the pending row as a sort
  proposal with "Canvas files it under \"<folder>\" — " prefixed to the reasoning; a
  Canvas row resolved by hand while the job ran falls back to the ordinary upsert,
  which still refuses to replace Canvas. The card reads the running job's scope to
  show `SORTING BY CONTENT…` and hold its actions, through a flag that is not the
  card's `busy`: the row survives the sort rewritten in place, so a held flag would
  outlive the run.

### Verified

- `cargo test`: 140 pass, three new — the guard's exclusion (a rename with an audit
  row clears both halves and nothing else; without the row it stays the job's; another
  class's row and a row before the window clear nothing; the list and single-path
  payload shapes both read), the Canvas override (dest, source, confidence and the
  prefixed reasoning), and the vanished pass (present file untouched, absent file
  dismissed with its row on record, missing class folder skipped). `npx tsc --noEmit`
  clean.
- Live, on the dev build beside the installed app, through the accessibility driver:
  the dashboard read `9 PROPOSED` on Fundamentals (the table held nine, the brief
  said eight: the ninth was Canvas's "Live coding session 08/25", due Aug 25) and
  `2 PROPOSED` on Design Studio; the Fundamentals queue showed the Aug 25 card with
  `PAST`, the line "ADD ALL SKIPS 1 PAST DATE" and `ADD ALL 8`; ADD ALL added the
  seven homeworks and the capstone (deadlines 31–38) and left the past card, whose own
  ADD DEADLINE then inserted deadline 39 (open, `canvas`); the card badge was gone
  after a DASHBOARD round trip.
- Applied Generative AI held two pending proposals whose inbox files were still on
  disk (12, the Week 01 deck; 13, the parking PDF), so the vanish check was run by
  moving the parking PDF out of `_Inbox/` from the shell: the badge read `1 TO SORT`,
  the reopened queue had one card, proposal 13 was `dismissed` and audit row 105
  carried its row. The file was moved back, where it listed as LEFT IN INBOX.
- **Fixture, stated plainly.** No pending Canvas placement existed (9–11 were
  dismissed in the merge for files already sorted by content), so proposal 12 was
  restored to what Canvas recorded for it on Aug 26 — `Slides/…`, source `canvas`,
  no confidence (audit row 67 holds the staging; the Aug 26 build's sort job had
  overwritten it, the case the M13 rule now forbids). The restore is audit row
  `library.restore_canvas_placement` with the sort job's version kept in its payload.
  For the Phase 4 check a second approvable move was needed in the same class after
  the vanish check had dismissed the parking PDF's row, and a fresh drop would have
  spent a second sort job, so the row was re-proposed by hand to `Syllabus/` (audit
  row `library.restore_proposal`, proposal 24).
- **SORT BY CONTENT** (job 288, the one sort run this session): scope
  `_Inbox/CAI6734_Week01_Course_Overview_Liu.pdf`, 2 turns, 15.6 s, **$0.37**
  list-equivalent (33k cache-write input, 24k cache-read, 991 output). The card
  showed `SORTING BY CONTENT…` under `PROPOSING DESTINATIONS…`, then re-rendered as
  `HIGH` → `Weeks/Week 01/` (NEW FOLDER) with the reasoning opening "Canvas files it
  under "Slides" — Skimmed the PDF: …" and no SORT BY CONTENT button. APPROVE moved
  the file to the sort's destination (audit row 107).
- **The guard.** That move indexed a new PDF, so extract job 289 started two seconds
  later; the parking move (proposal 24 → `Syllabus/`, audit row 108) was approved
  eight seconds into its window. Job 289 finished `succeeded` (4 m 15 s, the deck's
  extract written) with no `job.out_of_contract` row — the same shape that demoted
  job 4 on 2026-09-01.

### Left as it is

- Deadline 39, "Live coding session 08/25", is open and overdue on the Fundamentals
  card: adding it was the acceptance check for a past-dated card, and whether it is
  done or deleted is a one-click decision that belongs to the reader.
- The parking PDF was indexed while job 289 held the class's extract slot, and the
  pipeline only re-checks on a scan: the dev build's next launch enqueued it as
  extract job 290, which succeeded. Nothing to change, but worth knowing that a file
  moved during an extract waits for the next scan.
- `fingerprint_walk` skips dot-entries at depth 0 only, so a `.DS_Store` Finder writes
  inside a subfolder during a run is a change the guard reports. Not touched here:
  exclusion is by audit row, and that file is nobody's audited write.
- Exclusion is by path, as the brief specifies. A job that edits a file the app moved
  in the same window is not caught, because the `to` path is cleared whatever its new
  signature; comparing the moved file's (size, mtime) at `to` against `from` would
  close that and was left for the day it matters.
- The chat overview's move-proposal count reads the table without the vanish pass.

### Gotchas

- The AX driver from M16 was recompiled from that session's scratchpad with one
  addition, an `AX_NTH` environment variable to press the nth element of a name: a
  queue's cards all carry APPROVE and ADD DEADLINE, and the section header carries
  ADD DEADLINE too.
- A Vite server from the M16 session was still holding port 1420 after its app had
  gone; `tauri dev` fails on the port rather than reusing it.
- `perl -pi` with `|` as the substitution delimiter and `|row|` in the pattern
  matched an empty string and inserted the replacement at a random offset in two
  files. Both were caught by the compiler; edits went through the editor after that.
- The page reloaded to the dashboard once mid-session with no rebuild in the log;
  the workspace was simply reopened.

## Post-M17 — Review fixes (2026-09-02)

A two-agent review of the M17 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 7 warnings and
10 suggestions, 5 of them corroborated by both reviewers; all were addressed as
individual commits. What future sessions should know:

- **The vanish pass writes conditionally.** It read a row as pending, checked
  the disk, then wrote `dismissed` by id. With two processes on the table, an
  approve in the other one is exactly what makes a file leave the inbox, so the
  write could land over an approval and add a `sort.proposal_vanished` row
  contradicting the real `sort.move`. `dismiss_rows` updates `WHERE status =
  'pending'` and writes the audit row only when the update changed a row;
  tested with a row approved between selection and write.
- **"Cannot tell" is not "gone".** `is_file()` answers false for a permission
  or I/O error as readily as for a missing path, and a dismissal is terminal per
  path. `file_is_gone` dismisses on `NotFound` or a non-file at the path only;
  tested with a proposal inside a `chmod 000` folder, which stays pending. The
  stored path is read through `clean_rel` first, and the stats stay per
  proposal because a directory listing could not make that distinction.
- **The pass is housekeeping, not a precondition.** `pending_count` and
  `sort_state` propagated its error, so one class's write that could not commit
  failed `list_classes` for every class. `reconcile_vanished` logs and the read
  answers from the table as it stands. Chat's overview now runs the same pass
  per class before counting (`sorter::pending_move_count`), so the three
  readers agree.
- **A moved file is cleared from the write guard only under its own
  signature.** Path-only exclusion left a path unguarded for the whole window
  once the app had moved a file there — a job rewriting it at minute 20 of a
  master guide went unreported. `app_written_paths` now returns
  `AppWrite::MovedFrom(origin)` for a move's destination, and
  `excluding_app_writes` clears it only while the after-run fingerprint still
  equals the file's before-run signature at the origin (a rename changes
  neither size nor mtime); a file that arrived during the window under its own
  row stays path-based, and a path a file left is the app's outright, which is
  what makes a chain of moves read correctly whatever order the rows come in.
  The same read filters the class in SQL — `json_extract` raises on a payload
  that is not JSON, and only `CASE WHEN json_valid(...)` is guaranteed not to
  reach it — binds through `params_from_iter`, and states that the
  whole-second window is inclusive on purpose. SPEC §6 states the signature
  rule.
- **A scoped sort records only its file.** `finalize_job` routed the scoped
  entry to the override but let any other entry through to the ordinary upsert,
  and counted a refused Canvas replacement as recorded. The loop is now
  `record_entries`, which a test drives with a scope: other files are ignored
  and said so in the summary, `upsert_proposal` returns whether it wrote so a
  refusal is a skip, and a scoped file resolved while the job ran — approved,
  which moved it, or left in the inbox — is a reported outcome ("resolved while
  the sort ran — nothing recorded") rather than a failed job or a card brought
  back. A class-root placement reads "in the class root" instead of a quoted
  folder called the class folder.
- **One sort per class is settled by the insert.** `enqueue_sort` goes through
  `enqueue_unique`, so two clicks or two processes cannot both get a job past a
  check that ran a moment earlier. In the queue, SORT BY CONTENT is disabled on
  every Canvas card while any sort for the class is active, a card that started
  one holds itself until the job id it was handed reaches the jobs snapshot,
  and the header stays quiet for a scoped run, whose card carries the state.
- **A drop's bookkeeping never fails a finished copy.** The dismissed-row
  deletes and the `sort.staged` row commit together and a failure is logged,
  the way a filed lecture records its own row.
- Tests added: the conditional dismissal, the unreadable folder, the scoped
  run, the override's no-row and class-root branches, a job rewrite after an
  app move, a non-JSON audit row, the overview's waiting line and
  `deadlines::pending_count`; the guard fixture asserts its rewrite changed the
  length, since mtime is whole seconds.
- Verified: `cargo test` 144 pass, `npx tsc --noEmit` clean, and the fixed
  build launched beside the installed app with the dashboard and the Applied
  Generative AI workspace rendering as they were left (no card, no badge).
  Nothing was pushed: the repo has no remote.

## M18 — This week (2026-09-02)

### What was built

- **The current division.** `units::current_week` picks, from the `WeekSlot`s
  `week_slots` already builds for the Add lecture form, the slot whose published
  date is the latest on or before today — a week runs from its meeting to the
  next one's — and `units::current_unit` loads that slot's unit, so a
  Part-numbered course would resolve to the Part whose range holds the week
  through the same join. No slot is dated for Applied Generative AI (its ranges
  name weeks, not days), so it answers `None`, as does any day before a course's
  first published date and any string that is not a date. Past the last dated
  week the last one stays current: `ends_on` is NULL on all 49 rows, so nothing
  published says a course has ended. `list_units` now reads rows through
  `read_unit`, which `unit_by_id` shares.
- **On the card.** `db::ClassCard` carries `current_unit: Option<CurrentUnit>`
  (`name`, `kind`), and `list_classes(conn, today)` takes the day from the client
  the way `run_scan` and `chat::send` do — `listClasses()` sends `todayIso()`,
  so the line is measured against the same clock as the card's `IN 6 DAYS` and
  `DUE` labels. The card renders `currentUnitLabel(name)` — the name's own
  separator (em or en dash, spaced hyphen, or a Part's colon) becomes a middle
  dot and the whole line is uppercased — as one mono accent line under the
  meeting row, `truncate` so it never takes a second line from the deadline.
- **In the workspace.** `ClassWorkspace` subscribes to `["classes"]` and reads
  its own card out of the live result rather than the snapshot the dashboard
  handed over, because a syllabus scan run from the workspace rewrites the
  divisions. The eyebrow gains the division's name after the meeting time,
  keeping its own dash inside the dot-separated line. `StructureSection` takes
  the current division's name (unique per class) and `UnitRow` marks the match
  with a still accent dot and `NOW` after the name, plus `aria-current`; the
  list does not scroll to it. The `units` hub change now also invalidates
  `["classes"]`, since the card and the header resolve from those rows.
- **In chat.** `overview_text(conn, detailed, today_iso)` prints `Now: <name>`
  after each dated class's `Meets` line; the system prompt and `get_overview`
  both pass the turn's date.

### Verified

- `cargo test`: 147 pass, three new. The date table in `units.rs` runs the
  seeded syllabi through `current_unit` on the seeded class ids: Aug 19 → none
  and Aug 20 → Week 1 for Biostatistics (the boundary); Sep 2 → Week 2 for
  Fundamentals and Biostatistics with Week 3 from Sep 3; Nov 24 → Week 13 of
  Fundamentals (arithmetic says 14) and Dec 1 → Week 14; Nov 26 → `Week 15 — No
  Class (Thanksgiving Week)` and Dec 4 → `Reading Days — No Class (Reading
  Days)`; Dec 20 → Week 14 still; Applied Generative AI → none on both days;
  `"today"` → none; the undated Design Studio fixture → none. `db.rs` checks the
  card's `{name, kind}` on Nov 26 and `None` for the Part course and a class
  with no rows; `tools.rs` checks the `Now:` line lands in the Biostatistics
  block only. `npx tsc --noEmit` clean.
- Live on the dev build (pid 23085) beside the installed app, through the
  accessibility driver: the dashboard read `WEEK 2 · RESPONSIBLE AI, ETHICS, AND
  GOVERNANCE`, `WEEK 2 · HIPERGATOR AND NAVIGATOR I` and `WEEK 2 · STUDY
  DESIGNS` under the three meeting rows, above each `DUE` line, and no line on
  Applied Generative AI. The Biostatistics workspace eyebrow read `THU 11:45
  AM–1:40 PM · WEEK 2 — STUDY DESIGNS · JAX1 231 · 2 CR` and the Structure row
  `Week 2 — Study Designs` carried `NOW` beside `AUG 27`; the Applied Generative
  AI eyebrow was unchanged and its three Parts carried no mark. Screenshots of
  all three states were taken and read.
- No job ran, no chat turn was sent (pay-per-token), and no scan or sync was
  run: the chat line is covered by its test, and the `units` → `["classes"]`
  invalidation by reading, since exercising it needs a scan.

### Left as it is

- The `today` the cards were fetched with lives in the cached `["classes"]`
  result, so across midnight the week line lags until the next refetch — window
  focus or any hub change. The meeting chip recomputes at render and would be
  ahead of it for that interval.
- Opening a workspace now refetches `list_classes` (the query with the per-class
  inbox reads) on mount. Cheap at four classes; noted because `query.ts` already
  calls it the expensive one.
- A card line longer than the card truncates with an ellipsis and, under the
  card's full-surface button, has no tooltip to show the rest — the same is
  true of every badge title on the card today.

### Gotchas

- The dev window is only 734 px tall on this machine, so the cards sit below
  the fold: after `ax <pid> raise`, `osascript -e 'tell application "System
  Events" to key code 119'` (End) scrolls the *frontmost* app without naming a
  process, so it lands on the raised dev build and not the installed one.
- `screencapture -x -l <CGWindowID>` shoots one window; the id comes from
  `CGWindowListCopyWindowInfo` filtered by pid (a ten-line Swift in the
  scratchpad), since the two builds share a window title.
- In zsh, `S="perl splice.pl"; $S …` runs a command named `perl splice.pl` —
  no word splitting on an unquoted scalar. A function does what the alias was
  meant to.

## Post-M18 — Review fixes (2026-09-02)

A two-agent review of the M18 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 0 critical, 5 warnings and
5 suggestions, 3 of them corroborated by both reviewers; all were addressed as
individual commits. What future sessions should know:

- **The current division is one read over every dated row.** `current_unit`
  went through `week_slots`, kept the winning slot's week number and re-found
  a slot by it — `units` is unique on (class, name), not on ordinal, and the
  scan numbers a row by its position when the model omits the ordinal, so a
  rescan that renames a week leaves two rows on one number and the lookup took
  the lower id. It also saw `kind = 'week'` rows only: the live `Reading Days
  — No Class` and `Final Exam week` rows are weeks because the model said so,
  and a rescan omitting the kind would store them as `module`
  (`kind_for_name`'s fallback) and leave the card on Week 15 through December.
  The resolution now selects from every dated row of the class in one pass —
  the latest start on or before today, a week winning over a coarser division
  on the same day, then the later ordinal, then the later row. `current_week`
  and `unit_by_id` are gone. Tested with a duplicate ordinal, the Reading Days
  row stored as a `module`, and two shared start dates. SPEC §8.5 states the
  rule as it now stands.
- **The card carries the division's id.** `kind` crossed the wire unread, and
  the Structure row matched its `NOW` on the name string. `CurrentUnit` is
  `{id, name}` and the row matches on id.
- **The workspace reads its whole card live.** The header had joined the
  snapshot's meetings and room with a current division from the live query;
  `info` is now the live card out of the classes query, with the snapshot
  standing in until the first fetch, and the eyebrow renders the division
  through `currentUnitLabel` the way the card does.
- **The day is in the query key.** `classesQuery()` in `lib/classes.ts` is the
  one definition the dashboard, the workspace and the chat sidebar use:
  `["classes", today]`, fetched with the same day, so a dashboard left open
  across midnight asks again on its next render rather than serving
  yesterday's division; every `["classes"]` invalidation still matches by
  prefix.
- **Unit names now sit in the system prompt.** The `Now:` line carries text a
  model read out of a syllabus PDF into the prompt's context block — the same
  trust tier as the guide-scope and folder lines already there, capped at 120
  characters by `MAX_UNIT_NAME`. Deliberate, and said so at the line: a chat
  that does not know which week it is would be the worse trade.
- The overview test's block helper splits on the heading; the index
  arithmetic it replaced dropped the last three bytes of the final block.
- Verified: `cargo test` 149 pass, `npx tsc --noEmit` clean, and the fixed
  build launched beside the installed app: the three dated cards read the same
  three `WEEK 2` lines, the Biostatistics eyebrow read `THU 11:45 AM–1:40 PM ·
  WEEK 2 · STUDY DESIGNS · JAX1 231 · 2 CR` with `NOW` on the Week 2 row, and
  the Applied Generative AI workspace carried neither. Nothing was pushed: the
  repo has no remote.

## M19 — Chat knows what the app knows (2026-09-02)

### What was built

- **The overview.** `tools::overview_text` builds each class's block through
  `class_block`: after the meeting line, `Divisions: 17 weeks from the
  syllabus` (the course's own word where every row agrees on one, the neutral
  noun otherwise, and the source the way `Structure.tsx`'s `provenance` names
  it), `Now:` as before, then `Lectures:` — filed transcripts counted from
  `files` rows under `Weeks/*.md`, distilled and session-document counts, the
  divisions they feed with counts, and any transcript mapped to no division —
  then `Material:` with its depth-0 folders named as folders, `Guides:` with a
  session document named for what the session was about (`session 2026-09-01
  — Clinical Model Evaluation, Fairness, and HIPAA`, from its document path
  rather than its transcript-path scope), and `Waiting:` with both confirm
  queues counted. The detailed form lists every division with its date, `NOW`,
  its folder, its lecture counts and its guide's freshness; every lecture with
  its corpus note and session document's markdown twin; and every proposal
  with its id and the reader that proposed it. Ids stay out of the compact
  form: chat has no tool that acts on a proposal, so an id is something to
  name, not something to press. The tool description and the system prompt's
  "What you can see" paragraph say what the overview now carries.
- **Division-scoped triggers.** `resolve_scope` returns a `Scope` — `Master`,
  `Folder(rel)` or `Unit {id, name}` — matched through the same `best_match`
  tiers as classes over the units and the depth-0 folders together. A unit
  answers to its squashed name and to `kind + ordinal` (`week3`), so `Week 1`
  is an exact hit rather than a substring of Weeks 10–15, and a topic
  (`Data Exploration`) finds its week; a leading `unit:` is stripped, since a
  scope copied out of a listing carries it. A folder named exactly like a
  division is dropped from the candidates — `attach_folder_paths` joins the
  two on that equality, so the division's sources include the folder's files —
  and the division wins. `trigger_synthesis` routes a unit to
  `guides::synthesize_unit`; `generate_practice` passes `unit:<name>` through
  to `guides::generate_practice`, which looks the unit up by name, draws on
  `synthesis_context` with the unit id, and fills a new `{corpus}` section of
  `practice.md` from its distilled notes. `unit_context` is the shared step
  behind both the unit guide and the unit exam: it refuses when the division
  has neither a listed file nor a note — a filed-but-undistilled lecture puts
  the transcript in the manifest, which is what keeps the guide stale-aware,
  and nothing in the prompt's file listing, so the earlier code would have
  spawned a job with nothing to read. `lectures::corpus_block` now formats the
  `corpus_notes` list rather than reading the table itself.
- **The open class rides with the question.** `send_chat` takes
  `class_id: Option<i64>`; `sorter.ts` exports `openClassId()` — the same
  module variable `setDropTarget` writes — and `sendChat` sends it. The system
  prompt gains one line after the date: the workspace being looked at and that
  a question naming no class means that one, or that the dashboard is open.
  Nothing is persisted. `pickStarters` takes the open class's name and falls
  back to a random one on the dashboard; the memo re-draws when the id
  changes. `stream_turn` logs each round's usage from the API's own accounting
  (`message_start` for the prompt, `message_delta` for the answer), which is
  where the cost of the overview is read from now.
- **Practice exams from the workspace.** `PracticeAction` is one component
  used in all three guide clusters: FileTree's folder rows, Structure's
  division rows and the master strip. It sits beside VIEW GUIDE in the same
  mono register as resynthesize — muted, revealed on hover or focus on a row,
  standing on the strip, which has no row to hover — and turns into a pulsing
  `GENERATING EXAM…` while a practice job for that scope is queued or
  running. A division's row offers it exactly when a guide could be built
  (`canBuild`). The `generate_practice` Tauri command reuses
  `guides::generate_practice` with no focus; `lib/guides.ts` sends the two
  client-formatted labels. The three error slots now carry their whole line
  (`NO PRACTICE EXAM — …`, `PRACTICE EXAM NOT STARTED — …`), since a practice
  refusal under a `NO GUIDE` prefix read wrong. The active row in PRACTICE
  EXAMS labels a unit scope through `scopeLabel` instead of printing
  `UNIT:…`.
- The chat prompt's synthesis paragraph names the three scopes, says a
  division with neither folder nor distilled lecture is refused, and — after
  the live run below — that a refusal ends the action: no substituting a
  folder the reader did not name.

### Verified

- `cargo test`: 153 pass, four new in `tools.rs`. The scope parser: `Week 3`,
  `week 1` (not 10 or 13), `Data Exploration`, `unit:Week 3 — …` → the unit;
  `Module 1`, `m2` → the folder; `master`, `whole semester` → master; `?` →
  "which scope?"; `week` → six candidates named; `Week 99` → the divisions and
  folders listed; a Canvas unit called `Module 1` beside the `Module 1` folder
  → the unit. The overview: `Divisions: 5 weeks from the syllabus` and `Now:`
  for Biostatistics, `1 part` and no `Now:` for the Part course, `none
  declared` for a class without rows; the compact `Lectures: 1 filed, 0
  distilled, 0 session documents — Week 2 — Study Designs (1)`, `folders:`,
  `Waiting: 1 deadline proposal` and no `#`; the detailed division row with
  `NOW · 1 lecture (0 distilled)`, the lecture line, and `deadline proposal #1
  · Form Teams (project) due 2026-09-02 · from the syllabus scan`.
  `npx tsc --noEmit` clean.
- **Token count.** Measured against the live database with a throwaway
  ignored test printing `overview_text` read-only: the compact overview went
  from 6,007 to 6,407 characters, about 100 tokens across the four classes;
  the open-deadline list, capped at 25 of 37, is most of the text either way.
  The API's own accounting on the first live turn: 3,636 tokens cached at the
  tool-schema breakpoint and 4,995 at the system breakpoint (template plus
  overview); a later turn from another workspace rewrote the system block at
  5,001 with the tools still read from cache.
- Live on the dev build (pids 25364, then 25728 after a prompt edit) beside
  the installed app, driven through the M18 accessibility driver, model
  `claude-sonnet-5`, machine date still 2026-09-02 at 22:30 so `Now:` read
  Week 2:
  - From the Biostatistics workspace, "what's this week about?" — no class
    named — answered `Week 2 — Study Designs` from the overview and went on to
    read the Module 2 slide extract; it never asked which class. Four rounds:
    28,101 uncached input, 8,631 cache write, 25,893 cache read, 1,614 output
    — about $0.10 at Sonnet 5 list price.
  - "Make a practice exam for Week 3" from the same workspace: the model read
    the detailed overview, saw Week 3 has no folder and no lecture, and
    declined without calling the tool, naming what would give it sources. Two
    rounds, about $0.08. No job.
  - The same conversation, after opening the Fundamentals workspace: "Make a
    practice exam for Week 2" — the model kept the conversation's Biostatistics
    context over the open-class line, called `generate_practice` for
    Biostatistics Week 2 (refused: no folder, no distilled lecture), then on
    its own called it again for the `Module 2` folder and queued job 291.
    Cancelled from the Job Center about a minute in; the prompt now says a
    refusal ends the action. Three rounds, about $0.17.
  - A new conversation from the Fundamentals workspace: "Make a practice exam
    for Week 2" queued job 293, `practice` with scope `unit:Week 2 — Responsible
    AI, Ethics, and Governance`, in one tool call; the job's first reads were
    the corpus note and then the transcript. Two rounds, about $0.02. The
    starters in that panel all named Fundamentals; in the Biostatistics panel
    they had all named Biostatistics.
  - "Which deadline proposals are waiting for me?" named both — `#40 Form
    Teams (project)` from the syllabus scan and `#31 Introduction to Python
    and Version Control` from Canvas — as AI in Health Design Studio I's,
    waiting in that workspace's queue. Two rounds, about $0.02.
  - The Module 1 row's PRACTICE EXAM (the second of that name in the tree;
    the master strip's is first) queued job 292, `practice` with scope
    `Module 1`; the row read `GENERATING EXAM…` and the PRACTICE EXAMS section
    `GENERATING`. Screenshots: the Biostatistics master strip reading
    `STALE — REGENERATE · PRACTICE EXAM · VIEW GUIDE`, and the Fundamentals
    Week 2 row reading `NOW · 1 LECTURE · ⟳ · GENERATING EXAM… · VIEW GUIDE`
    with `GENERATE SEMESTER MASTER · PRACTICE EXAM` on its strip.
  - Both jobs succeeded, Opus at `xhigh` on the subscription, list-price
    equivalents from the CLI's own accounting: job 292 (`Module 1`, folder)
    10.4 min, 23 turns, $3.74, 54.7k output tokens, an 82 KB exam of 19
    questions; job 293 (`unit:Week 2 — …`) 8.3 min, 18 turns, $2.37, 45.2k
    output tokens, a 79 KB exam of 18 questions citing the corpus note and
    the transcript 37 times. Neither file carries a `CONTINUE` marker or an
    external reference. The finished exam appeared in the Fundamentals
    PRACTICE EXAMS listing and the Week 2 row returned to `PRACTICE EXAM ·
    VIEW GUIDE`. The cancelled job 291 ran about a minute: 20 assistant
    messages, 191k cache-write and 1.03M cache-read tokens, about $1.70
    list-equivalent.
  - Chat in total: five turns, about $0.39 at Sonnet 5 list price
    ($2/M input, $2.50/M cache write, $0.20/M cache read, $10/M output),
    plus five unlogged follow-up calls of a few hundred tokens each.

### Left as it is

- A conversation's own history outweighs the open-class line: a chat that
  had been about Biostatistics kept resolving "Week 2" there after the
  Fundamentals workspace opened. The line is one sentence against pages of
  transcript, and a new conversation from the workspace behaves. Worth
  knowing before assuming the open class re-targets an old thread.
- The model may decline a scope from the detailed overview without calling
  the tool, which is the right answer arrived at one call early; the tool
  refuses the same request the same way (job 291's first call proved it).
- Each round of a turn re-sends the conversation uncached, so a turn's
  `input` count climbs with every tool result (28k over four rounds on the
  first turn). The system block and the tool schemas are cached; the history
  is not. Pre-existing, and the largest cost in a multi-round turn.
- The compact overview's open-deadline list (25 of 37, `LIMIT 25`) is most of
  the text either way; the per-class blocks are about a third of it.
- `Lectures:` counts transcripts as `Weeks/*.md`; Applied Generative AI's
  `Weeks/Week 01` holds a PDF deck and reads `none filed` beside
  `folders: … Weeks (1)`, which is accurate.

### Gotchas

- `screencapture -l` with the first id `winid` prints can shoot a tooltip
  window (a 264×20 strip); filter the listing by the window name.
- The page came back on the dashboard after the jobs finished, with the AX
  tree showing only the dashboard's buttons — a reload, not a driver fault;
  navigating again was enough. Cause not found.
- `Cancel job N` from the Job Center takes a few seconds to show in the
  table; a read straight after the press still says `running`.
- The chat textarea has no accessible name; `ax <pid> focus role:AXTextArea`
  and `setvalue role:AXTextArea <text>` reach it, and the controlled React
  input takes the value.

## Post-M19 — Review fixes (2026-09-02)

A two-agent review of the M19 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 0 critical, 5 warnings
and 8 suggestions on the change plus one pre-existing warning, 2 of them
corroborated by both reviewers; all were addressed as individual commits.
What future sessions should know:

- **The overview reads the move queue directly.** `class_block` went
  through `sort_state` for a class's pending move proposals, which re-ran
  the vanish reconciliation the overview had already run for every class
  and listed the inbox directory only to discard it, on every chat send.
  `waiting_block` selects the pending rows itself.
- **A folder leaves the scope candidates on exact name equality only.**
  `resolve_scope` dropped a folder whenever its squashed name equalled a
  division's, but `attach_rel_path` joins the two on the exact name; a
  folder `module 1` beside a division `Module 1` was dropped without ever
  having been attached, leaving it unreachable as a scope and the division
  without its files. The comparison is now the attach's own, and a query
  hitting a near-match folder and its division is refused as ambiguous.
- **Scoped jobs check and insert in one transaction.** Guide, master,
  practice and digest enqueues ran `has_active_job` under the lock,
  released it, and inserted under a fresh one; `jobs::enqueue_unique_scoped`
  is the per-(class, scope) form of the IMMEDIATE check-and-insert the
  extract and sort enqueues already used. The callers' earlier check stays
  as the specific message; the transaction is the guard.
- **A practice exam's name avoids the ones still being written.** The
  folder `Module 1` and a division called `Module 1` label their exams the
  same, and the active-job guard keys on scope, so both could run at once;
  the naming loop only looked at the disk, where neither file existed yet.
  `practice_output_rel` also skips the paths claimed by the class's queued
  and running practice jobs (`claimed_practice_paths`, from their payloads).
- **The master strip keeps its practice action during a master run**, and
  a division row keeps its `GENERATING EXAM…` pulse after losing its
  sources; the strip takes `practiceActive` from the workspace's
  `activePracticeScopes` rather than filtering the jobs store itself.
- **The compact `Lectures:` line counts.** It had named every division
  with a lecture, which would grow the line that rides every turn by about
  a name a week; it now carries the counts and the current division's share
  (`1 in the current division (1 distilled)`), and the detailed form names
  the rest.
- **`class_block` is split**: `division_rows`, `lectures_block`,
  `material_line`, `guides_line` and `waiting_block` stand beside
  `divisions_line`. `Scope` derives `Clone`, its `Unit` carries `kind` and
  `ordinal` so the matcher's `week3` key needs no second lookup, and
  `generate_practice` takes an optional unit id the chat trigger passes
  instead of the name lookup; the Tauri command passes none. The Master
  arms of the matcher's closures are `unreachable!`, since master is
  answered by name before the candidates are built.
- The round-usage log inserts `output_tokens` through the object map:
  indexing a non-object `Value` panics, and the stream thread has nothing to
  catch one. `markdown_twin` and `document_stem` use `strip_suffix`.
- **Tests**: `unit_context`'s four cases — nothing, filed but not distilled,
  a note on disk, a folder — against an in-memory database and a temp root;
  the practice naming loop against the disk and the queue; the overview's
  twin/stem/proposer helpers, `divisions_line` across sources and kinds,
  `Scope::key` for master and folders; `open_class_line` for a workspace,
  the dashboard and a stale id. `cargo test`: 157 pass. `npx tsc --noEmit`
  clean. The rebuilt dev build showed the Fundamentals strip and rows with
  the same clusters and the exam listing. Nothing was pushed: the repo has
  no remote.

## M20 — Every format in the tree (2026-09-02)

### What was built

- **Kinds.** `scanner::kind_for` names `docx`, `ipynb`, `py` and `csv`
  beside the kinds it had; SPEC §5's comment now lists every value the
  column holds, `FileTree`'s icon map gives each a glyph, and
  `VIEWABLE_KINDS` gains the four plus `pdf` and `pptx`.
- **Zero-token extracts.** `extract::route` has three new arms. `Docx` runs
  the same bounded LibreOffice subprocess `pptx` uses — `convert_pptx`
  became `convert(app, class_dir, rel, sha, &Conversion)`, with
  `PPTX_TO_PDF` and `DOCX_TO_HTML` naming the `--convert-to` argument and
  the extension the mirror keeps — then feeds the result through the
  `Route::Html` code path to `<name>.docx.md`. `Notebook` is
  `notebook::flatten`, a pure function over nbformat 4: markdown and raw
  cells verbatim, code cells fenced with `language_info.name` (falling back
  to the kernelspec's language, then a bare fence), `stream` and
  `text/plain` outputs in a fenced `output` block capped at 40 lines and
  4 KB per cell, an image output replaced by `[Figure: image output]` before
  its `text/plain` placeholder is considered, an `error` output reduced to
  `ename: evalue` without the ANSI traceback. `Csv` is `cap_csv`: the first
  300 lines and a note naming the remainder. `py` joins the text route;
  `.txt` keeps its caption route. The pipeline loop now funnels every local
  route through one closure that extracts and records, so a fourth local
  route is one match arm.
- **The docx export is one file.** Writer's HTML export drops every figure
  beside the HTML as a loose PNG — eleven of them, 2.9 MB, for the one docx
  in the tree, against a 7 KB HTML — so the filter is invoked as
  `html:HTML (StarWriter):EmbedImages`, which inlines them as data URIs the
  stripper discards like any other `<img>`. The mirror holds
  `<name>.docx.html` (3.9 MB for that file) and its `.sha256` sidecar, the
  same rule as a deck's PDF twin, and the extract is 2.1 KB.
- **The stripper folds soft line breaks.** Writer hard-wraps every
  paragraph at about seventy characters, and the first extract read
  `Introducing Navigator\nToolkit API Key…`, which a line-based search
  could never match across the wrap. `strip_html` now tracks `<pre>` depth
  and, outside one, turns a newline in prose into a single space owed
  before the next word — a pending break carried across inline tags, so
  `Navigator\n<b>Toolkit</b>` does not glue. Inside `<pre>` nothing
  changes; a block tag still starts a line.
- **PDFs in the app.** `tauri` gains the `protocol-asset` feature,
  `tauri.conf.json` enables the asset protocol with an empty scope, and
  `lib::allow_asset_root` widens it to the AIBHS root at launch and again
  from `set_aibhs_root`. `frame-src` in both CSPs admits
  `asset: http://asset.localhost`. `extract::pdf_view_path` answers the
  `pdf_view_path` command with the absolute path to frame: a PDF itself, or
  a deck's twin when `conversion_is_current` says the sidecar matches the
  indexed hash, and an error otherwise. `ClassWorkspace::openMaterial`
  resolves that before opening the viewer and falls back to the default
  app on refusal; a notebook opens with `source` set to its extract path;
  scripts and CSVs open as text. `FileViewer` frames a `pdfPath` on
  `convertFileSrc` with no sandbox attribute — a sandboxed frame has no
  plugins, and WebKit's PDF view is one — and labels the eyebrow `PDF`,
  `SLIDES AS PDF`, `NOTEBOOK EXTRACT`, `PYTHON`, `CSV`. The document shell
  draws a notebook's `output` fences without fill, dashed and in muted ink,
  so what a cell printed reads apart from what was written in it.
- The chat prompt's binary-sources line names `.docx` and `.ipynb` beside
  `.pptx` and `.pdf`; `read_material` already refused both, and the extract
  is the copy worth reading in either case.

### Verified

- `cargo test`: 170 pass, 13 new. The notebook flattener against
  `src-tauri/tests/notebook.ipynb` (markdown verbatim, python fences, the
  stream and the describe() result, the HTML twin dropped, the figure
  marker with no base64 and no placeholder, the error by name, an unrun
  cell with no output block, a kernelspec-only and a bare-fence notebook,
  JSON that is not a notebook refused, a 100-line output capped at 40 with
  `(60 more lines)`, a 5,000-character line cut at a char boundary); the
  CSV cap at 300 lines with its note and a short CSV kept whole; the soft
  break fold with `<pre>` untouched; `pdf_view_path` for a PDF, an
  unconverted deck, a current twin, a twin from an older hash, a markdown
  file and a traversal. `npx tsc --noEmit` clean.
- Live on the dev build beside the installed app (pids 29957, 30619, then
  30746 across two rebuilds — the stripper fold and the scope's leading-dot
  rule), driven through the M16 accessibility driver, no job running at
  either rebuild:
  - The launch scan extracted the Biostatistics `.docx` and, in a
    throwaway `AI in Health Design Studio I/M20 Format Check/` folder, the
    fixture notebook, a 14-line `warmup.py` and a 350-line `cohort.csv`
    (extract of 302 lines ending `… 50 more lines not extracted (350 in the
    file, header included)`). The jobs table gained no row: the highest id
    stayed 293 throughout. Biostatistics went from 14 of 15 to 15 of 15
    extracted, and the compact overview, printed read-only from the live
    database through a throwaway ignored test, reads `Material: 15 files
    indexed, 15 with a current extract`.
  - The reading PDF under `Module 1/Reading Material` opened inline under
    `MATERIAL · PDF`; `Biostatistics_Module1_Slides_class2.pptx` opened as
    its twin under `MATERIAL · SLIDES AS PDF` (blank on the first try — see
    Gotchas); Applied Generative AI's `Weeks/Week 01` deck PDF opened
    inline. The notebook opened as its extract, the script as `PYTHON`, the
    CSV as `CSV`.
  - A hand-written `network probe.pdf` in the fixture folder — a URI
    `OpenAction`, a URI link annotation and an image XObject with a remote
    `/F` file specification, all at `http://127.0.0.1:8765` — rendered its
    text with a `nc` loop listening; the listener's log stayed at 0 bytes
    (a `curl` self-test showed it logging). The probe was pre-recorded as
    extracted in `files` before the scan reached it, since a stale PDF is
    exactly what enqueues an extract job.
  - One chat turn from the Biostatistics workspace, model `claude-sonnet-5`:
    "Which base URL does the Posit Assistant setup document say to enter
    alongside the UF Navigator Toolkit API key?" — `search_material`
    (12 lines in 2 files), a second search, `read_material` on the docx
    extract, and the answer quoted `https://api.ai.it.ufl.edu/v1/` from
    Step 5 citing the extract path. Three rounds: 21,575 uncached input,
    8,697 cache write, 17,394 cache read, 398 output — about $0.07 at
    Sonnet 5 list price. The cache write is the system block plus tools,
    66 tokens over M19's measurement.
  - The fixture folder and its mirror were removed and RESCAN pressed
    before the commit: Design Studio back to 1 indexed file, no `M20` rows,
    no `M20 Format Check` in the tree.

### Left as it is

- A `.docx` opens in its default app, as the brief lists it nowhere among
  the in-app kinds; its extract carries no figures and Word shows them.
- The deck fallback to the default app is covered by `pdf_view_path`'s
  refusal in the tests and not exercised live, since that launches
  PowerPoint; the frontend's `.catch` is the same call the row's arrow
  makes.
- The installed app is still the Aug 25 build. Until it is reinstalled its
  scans on window focus rewrite the four new kinds to `other` (the upsert
  sets `kind`), which the dev build's next scan rewrites back; the extract
  columns are untouched either way, so nothing is re-extracted and the
  overview's count holds.
- An old root stays in the asset scope after the setting changes, until
  relaunch: `forbid_directory` on the old root would also forbid a new root
  nested inside it.
- Chat citations still open only the text kinds (`answer.ts`'s map); a
  cited `.py` or `.csv` path stays a code span.

### Gotchas

- **Tauri's scope globs refuse a leading dot by default on Unix.**
  `allow_directory(root, true)` pushes `root/**`, and with
  `require_literal_leading_dot` true that never matches `.classhub/…`, so
  the deck's twin framed blank while the reading PDF beside it rendered.
  The config's object form of `scope` carries `requireLiteralLeadingDot`;
  set to `false`, the scope is still the root and nothing else.
- **soffice names its HTML `<stem>.html`**, stem including the trailing
  space this docx has before its extension, so the rename to
  `<name>.docx.html` is the same move the PDF twin makes.
- The editor passed the fixture's ANSI escape bytes through literally, and
  `serde_json` refuses a control character in a string; the fixture
  carries them as `\u001b` escapes.
- A raw `<` in test HTML (`x <- 1` inside `<pre>`) is a tag to the
  stripper; real notebook HTML escapes it, so the test does too.
- The scan of a class runs at launch, so a file added a minute later waits
  for the next scan — the probe PDF was indexed by hand for that reason,
  and the extract columns set with it.
- `cd src-tauri` persists across Bash calls; two later commands with
  relative paths failed on it.

## Post-M20 — Review fixes (2026-09-03)

A two-agent review of the M20 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 0 critical, 7 warnings
and 8 suggestions, 2 of the warnings corroborated by both reviewers; all
were addressed as individual commits. What future sessions should know:

- **The docx twin was being searched.** `search_material` greps the whole
  extracts mirror, where `<name>.docx.html` (3.9 MB, every figure a base64
  line) sits beside `<name>.docx.md`; the live M20 chat turn's "12 matching
  lines in 2 files" was one document matched twice. `run_rg` now skips
  `*.docx.html` the way it skips a session document's HTML copy, and
  `read_material` turns a twin path into a pointer at the extract.
- **The sorter moves the whole mirror.** `update_index` and
  `undo_artifact_moves` named `.md`, `.pdf` and `.pdf.sha256`, so an
  approved move of a docx would have orphaned its twin and sidecar.
  `extract::MIRROR_SUFFIXES` holds the list beside the `Conversion`
  constants, and a test asserts every conversion's twin and sidecar are in
  it.
- **A notebook opens what the index holds.** The tree node carries
  `extractRelPath` — the row's `extract_rel_path` when `extracted_sha256`
  equals the file's current hash — and the workspace opens a notebook as
  that, or in its default app when there is none yet, the rule an
  unconverted deck already followed. The frontend's copy of the mirror
  rule (`extractPath`) is gone. `pdf_view_path` answers `None` for a deck
  with no current twin and keeps errors for failures; the workspace shows
  those under the Materials heading (`COULD NOT OPEN … —`) through the
  state now called `materialsError`, instead of opening the default app as
  if nothing had gone wrong.
- **Local reads are bounded.** `read_text` refuses a source above 32 MB
  with a logged reason (it stays stale), and a CSV never goes through it:
  `text_lines` streams the file so the 300-line cap costs one line at a
  time. `cap_csv` takes an iterator. The cap counts physical lines, which
  its doc comment now says.
- **`extract_local` refuses what it cannot do.** The `Pdf`, `Pptx`, `Docx`
  and `Skip` arms bail instead of returning the raw text as the extract.
- **The notebook flattener**: one output budget per cell rather than per
  output (three 30-line streams are one cell that printed ninety; each
  dropped output names its own remainder), fences one backtick longer than
  any run inside the block, and an error output with neither name nor
  message skipped. `ConversionPaths` names what was a positional triple.
- **DOCX conversion egress, measured.** A docx whose only image was an
  external `http` relationship converted headless with `EmbedImages` to an
  `<img>` with an empty payload; a listener that logged a request before
  and after the run saw nothing from LibreOffice. Recorded in SPEC §13
  beside the PDF measurement; no profile setting was needed.
- SPEC §13's asset-scope bullet now says why a left root stays allowed
  until relaunch (a forbid would also cover a new root chosen inside the
  old one, and cannot be lifted without a relaunch), and §12 says a
  notebook without an indexed extract opens in its default app. The
  `read_material` tool description names `.docx` and `.ipynb` beside
  `.pptx` and `.pdf`, as the system prompt already did.
- **Tests**: `route` and `kind_for` table tests (the two dispatch
  functions had none), the mirror-suffix invariant, the read cap and the
  streamed CSV, the per-cell budget, the fence growth, the empty error
  output, and `pdf_view_path`'s `None`/error split. `cargo test`: 177
  pass, no warnings. `npx tsc --noEmit` clean.
- **Live**, on a rebuilt dev build (pid 32574) with the fixture notebook
  placed back in Design Studio before the launch scan: the notebook row
  opened as `MATERIAL · NOTEBOOK EXTRACT` from the node's indexed extract
  path, the reading PDF as `MATERIAL · PDF`, the deck as `MATERIAL · SLIDES
  AS PDF`; the fixture was removed and the class rescanned before the
  commit, and the jobs table still ends at id 293.
- The first attempt at two of these commits landed with a compile error
  because a `grep` in the commit pipeline masked `cargo test`'s exit code;
  both were reset and remade once the missing `BufRead` import was in.
  Nothing was pushed: the repo has no remote.

## M21 — Grades from Canvas (2026-09-03)

### What was built

- **Schema.** Migration `0010_canvas_grades.sql` adds
  `grade_categories.canvas_group_id`, `grade_items.canvas_assignment_id`,
  `deadlines.canvas_assignment_id` and the same on `deadline_proposals`,
  each with a partial unique index where set — per class for a group and a
  deadline, global for an item, since a Canvas assignment id is global and
  `grade_items` carries no class column. The live database went from
  `user_version` 9 to 10 the moment the dev build opened it; the installed
  Aug 25 build skips migrations past its own list and its own inserts name
  their columns, so it kept working beside the new columns.
- **Categories and items.** `grades::upsert_canvas_category` finds a
  category by group id, else claims a category with no id and the same name
  (case-insensitive, the chat tool's rule), else inserts one; the weight is
  taken from `group_weight` only when the course applies group weights, and
  a new category otherwise starts at zero so the ≠100% warning says what is
  missing. `grades::upsert_canvas_item` finds an item by assignment id and
  updates category, name, score, max and `graded_at` in place, else inserts.
  Both write an audit row (`canvas.upsert_grade_category`,
  `canvas.upsert_grade_item`) only when something changed, so a no-op sync
  leaves none. `canvas_sync::graded_and_posted` is the predicate: a score,
  a `posted_at`, not excused, positive `points_possible`.
- **The sync.** `sync_assignments` reads `/assignments?include[]=submission`
  and returns the list; `sync_grades` reads `/assignment_groups` and writes
  categories, then items for every assignment the predicate accepts, keyed
  through the group id. Grades run only when the assignment read succeeded.
  `ClassOutcome` carries `grades_recorded` and `deadlines_completed`, and
  notes name categories created or claimed and whether the course weights
  them. `hub-changed` fires for `grades` and `deadlines` when either moved.
- **Deadlines Canvas tracks.** `deadlines::settle_canvas_deadline` finds the
  deadline for an assignment by id, or — once — by (title, calendar day)
  among rows with no id, and stamps the id; it then moves the row to
  Canvas's due date (`canvas.update_deadline`, before and after) and marks
  it done when the submission carries `submitted_at`
  (`canvas.complete_deadline`, naming the submission). Nothing reopens. The
  sync settles before proposing, so a tracked assignment never earns a
  second card. `record_proposal` takes the Canvas id: a row with that id is
  the same card whatever its title or day now (pending → refreshed,
  dismissed → left, approved with its deadline deleted → reused as a fresh
  card, which is what one row per (class, assignment) requires), and the
  (title, day) rules that follow ignore rows carrying a *different* id. A
  syllabus card Canvas recognizes takes the id on refresh. Approval carries
  the id onto the deadline and refuses a card whose assignment is already on
  the list under another title.
- **UI.** `GradeItem` and `GradeCategory` carry their Canvas ids; the
  Grades section shows `VIA CANVAS` in the deadline row's tag register on
  categories and items the sync owns, the item's tooltip saying the next
  sync writes Canvas's score back over an edit. The empty state names the
  sync. `outcomeSummary` adds `N deadlines done` and `N grades recorded`,
  and the sync's `done` handler invalidates `grades` and `deadlines` beside
  what it already refetched.

### Verified

- `cargo test`: 184 pass, 7 new — the predicate (muted, ungraded, excused,
  zero and null points, no submission, a posted zero), group weights only
  when applied, the category claim (claimed by name across casing, typed
  weight kept, found by id after with no audit row, reweighted when the
  course weights, created at zero, another class's row untouched), the item
  upsert (created, unchanged with no audit row, a hand edit overwritten, the
  weighted grade seeing it, zero points refused), the syllabus deadline
  linked, moved, closed on the submission with an audit row naming it and
  left alone after, one proposal row per assignment through refresh,
  approval, deletion and dismissal, and the id joining the two readers.
  `npx tsc --noEmit` clean.
- Live on the dev build (pid 33483) beside the installed app, no job running:
  - **Phase 0**, from a temporary dump of what Canvas answered, removed
    before the commit: no course applies group weights; Biostatistics has
    four groups and no assignments; no course had a graded or posted
    submission. Recorded in SPEC §1.
  - **Sync 1** (all classes, from Settings; the stored session had expired,
    so the sign-in window opened and the owner completed SSO): the three
    typed Biostatistics categories were claimed (`Assignments` 50,
    `Quizzes` 20, `Project` 30, ids stamped, weights untouched) and six
    created at zero across the four classes; the four Canvas assignments
    became cards with their ids, the one already waiting (`Introduction to
    Python and Version Control`) refreshed with its id rather than
    duplicated; 12 new course files staged and one loose notebook sent to
    the sorter (job 294, 11 s). No item, since nothing is graded.
  - **Sync 2** (Fundamentals, from the workspace, after approving the
    `Live coding session 09/01` card, which Canvas holds a submission
    for): the deadline went `done` and the only new audit row was
    `canvas.complete_deadline` with `submittedAt 2026-09-01T21:57:07Z`.
    Settings read `1 deadline done`.
  - **Sync 3** (all classes): `nothing new` for every class, audit log
    unchanged at id 134, counts unchanged (9 categories, 0 items, 43
    proposals, 2 done deadlines, jobs still at 294).
  - The Grades section reads `VIA CANVAS` beside each claimed category.
- Not exercised live, because Canvas held no posted grade in any course
  and Quiz 1 is not a Canvas assignment: an item landing under its
  category and the card's grade chip. That path is `sync_grades` calling
  `upsert_canvas_item`, covered by the tests above; the first posted grade
  will be the first live run.

### Left as it is

- A Canvas-owned item or category deleted by hand comes back on the next
  sync, and a deadline reopened by hand is closed again while Canvas holds
  the submission. Both follow from Canvas being the source; neither has a
  tombstone.
- `omit_from_final_grade` is not read; no assignment sets it today.
- A linked syllabus deadline keeps `source='syllabus'` and its `VIA
  SYLLABUS` tag; only the id says Canvas tracks it.
- `Homework #1` (Canvas) and `Homework 1` (syllabus, same day) are two rows
  in Fundamentals, because the link is by exact title. The card is still
  there to skip.
- Two Canvas groups sharing one name would become two categories with one
  name, which the category editor's uniqueness check would then refuse to
  edit.

### Gotchas

- `perl -0pi` with `|` as the delimiter and `|r|` in the replacement: the
  M17 gotcha again, this time inserting the block at line 1 of
  `deadlines.rs`. Multi-line Rust edits went through the editor after that.
- The sign-in window lands on the UF e-Learning page and moves to
  `login.ufl.edu` on its own; `ax press GatorLink` reported NOT FOUND
  because the page had already moved.
- `screencapture` after `End` shoots the bottom of the workspace; the
  Grades section sits above Materials there, so the tag check came from
  `ax text`.

## Post-M21 — Review fixes (2026-09-03)

A two-agent review of the M21 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 6 warnings
and 9 suggestions, 3 of them corroborated by both reviewers; all were
addressed as individual commits. What future sessions should know:

- **A hand-entered score was counted twice.** Categories were claimed by
  name but items were not: a score typed from the returned paper before
  the professor posted it stayed beside the row the sync then inserted,
  and the weighted grade summed both. `upsert_canvas_item` now claims a
  row with no id and the same name anywhere in the class (the category
  the assignment sits in first), moving it to Canvas's category and
  writing Canvas's number, with `"claimed": true` on the audit row. The
  lookup by id joins `grade_categories` and refuses a row another class
  holds: the index on the assignment id is global, `classes.code` carries
  no UNIQUE constraint, and two classes matched to one Canvas course would
  otherwise move one row back and forth with an audit row each way.
- **Category names stay unique.** `save_category` and the chat tool
  refuse a second row under one name, so a Canvas write that produced one
  left both weights uneditable. A group renamed onto a taken name keeps
  its current name; a second group under a taken name lands as
  `Name (2)`; both come back as a `note` on `CategoryWrite`, which
  `upsert_canvas_category` now returns beside the id and the write, and
  the sync pushes into the report. The note repeats on every sync while
  the collision stands, deliberately.
- **The legacy link is audited and atomic.** Stamping a syllabus deadline
  with its assignment id was a bare autocommitted UPDATE before the
  transaction that moved its date; it now lands inside that transaction
  with a `canvas.link_deadline` row, and the (title, day) match orders
  open rows before done ones. `CanvasAssignment.due_at` is optional, and
  the settle runs before the sync's undated guard, so a tracked deadline
  whose assignment Canvas no longer dates is still closed by its
  submission; `submitted_at` goes through `local_iso` like every other
  Canvas timestamp.
- **The deadline row says who owns it.** `DeadlineInfo` and the `Deadline`
  type carry `canvas_assignment_id`, and `deadlineSourceBadge` prefers it
  over `source` with the grade item's words: an edit lasts until the next
  sync. Proposal cards keep the source-only badge.
- **The by-id refresh keeps the rank rule**, the (title, day) predicate and
  the not-another-assignment guard are two `const`s interpolated into the
  five queries that ask them, `fetch_assignments` does the one read both
  passes consume and `sync_assignments` returns nothing, a failed grades
  read is a report line rather than the class's failure (the file sync
  behind it runs), and `grade_state` names Posted / Unposted / Nothing so
  the report can say how many grades Canvas is holding. The unchanged
  check in `upsert_canvas_item` precedes its transaction; both upserts
  compare stored numbers with `==`, since a REAL round-trips bit for bit;
  the delete audit rows carry the Canvas ids.
- **Tests**: the item claim and the other-class refusal, the moved
  assignment, the name collisions, the undated settle, the link audit row
  and the open-row preference, approval refusing a card whose assignment
  is already on the list, the rank rule on the id path, and the three
  grade states. `cargo test`: 190 pass, no warnings. `npx tsc --noEmit`
  clean.
- **Live**, on the rebuilt dev build (pid 37717): a full sync of all four
  classes reported `nothing new` for each, left the audit log at id 134
  and every count as it was (9 categories, 0 items, 43 proposals, 2 done
  deadlines, jobs at 294).
- SPEC §7.2 and §11 record the item claim, the name rule, the link audit
  row, the undated completion and the badge; nothing was pushed, since
  the repo has no remote.

### Gotchas

- A perl substitution with `|` as its delimiter and `|conn|` in the
  replacement landed the replacement at the import line of
  `canvas_sync.rs`, the M17 gotcha a third time; the editor was used for
  every multi-line edit after that, and a sed replacement of `x($` put a
  literal `$` into three lines because `$` is not an anchor on the
  replacement side.
- A test helper named `landed` shadowed the `landed` bindings the older
  tests destructure into; renamed `place`.
- Each `src-tauri` edit rebuilt and relaunched the dev build (pids 33873
  then 37717) while no job was running; `cargo test` waits on the target
  directory lock while `tauri dev` compiles, so a test run after an edit
  can take a minute.

## M22 — What the professor said (2026-09-03)

### What was built

- **Phase 0.** A temporary env-guarded dump in `sync_class` (removed before
  the commit) wrote what Canvas answered for six reads per course during one
  sync from the dev build; the findings are in SPEC §1. Every course has
  announcements (1 / 4 / 2 / 2) and Pages (6 / 5 / 5 / 1, all published);
  the bare `/announcements?context_codes[]=` call and one widened with
  `start_date` returned the same sets today, but the endpoint's window is
  fourteen days by default, so the sync reads the course's
  `discussion_topics?only_announcements=true`, which returned the identical
  objects. `include[]=body` on the Pages listing carried every body. Every
  `syllabus_body` is a one-line link to the syllabus PDF already in the
  tree, so the sync takes it off the course listing with
  `include[]=syllabus_body` rather than spending a request per course.
- **Schema.** Migration `0011_announcements.sql` adds
  `announcements(id, class_id, canvas_id UNIQUE, title, body, posted_at)`;
  the live database went to `user_version` 11 when the dev build opened it,
  and the installed Aug 25 build kept running beside the new table.
- **Announcements.** `canvas_sync::sync_announcements` strips each message
  through `extract::strip_html` (now `pub(crate)`), converts `posted_at`
  through `local_iso`, skips delayed announcements that carry none, and
  `record_announcement` inserts or updates on the Canvas id — refusing a row
  another class holds, the M21 rule for grade items. `list_announcements`
  answers the `list_announcements` command newest first. The sync emits
  `hub-changed` for `announcements` when a row was written.
- **Pages and the syllabus page.** `sync_pages` reads
  `/pages?include[]=body`, skips unpublished or hidden pages and empty
  bodies, names each file by `page_file_stem` (the title as a path segment,
  Canvas's URL slug appended on a collision, `Syllabus` reserved) and writes
  `page_markdown` — a title line, one line saying what it is and where it
  lives, the stripped text — through `write_if_changed`, so an unchanged page
  is not rewritten. The syllabus body lands as
  `.classhub/extracts/Canvas/Syllabus.md`; `canvas_syllabus_path` answers the
  `canvas_syllabus` command with its rel path when the file exists, and the
  Deadlines picker offers it as `CANVAS SYLLABUS PAGE`. The scan reaches it
  through `resolve_rel`, which accepts any regular file under the class.
- **Quiet sessions.** `Session` carries `quiet`; `Session::open_quiet` opens
  a window that never reveals itself — `ask` returns `SignInNeeded` on a
  conclusive refusal (still discarding the stored copy) and a plain error on a
  stall, and `poll` skips the `HIDDEN_GRACE` reveal. `has_remembered_session`
  exposes the Keychain check.
- **Sync on launch.** `canvas_sync::sync_on_launch` runs after the launch
  scan on the same thread: `launch_sync_due` (no sync ever, or a day old),
  then the Keychain check, then `spawn_with(…, launch: true)`. `Progress`
  carries `launch`; a `SignInNeeded` failure is emitted as `Not synced` and
  the Settings report renders it as `NOT SYNCED ON LAUNCH — …` in muted
  text, a success as `SYNCED ON LAUNCH` above the per-class lines.
- **Sync age.** `App.tsx`'s `CanvasLine` sits under the date in the
  dashboard header: `CANVAS · SYNCED 6 DAYS AGO` from `syncAgeLabel` and
  `daysSinceSync` (calendar days, the deadline rule), destructive from
  `SYNC_STALE_DAYS` = 7, `SYNCING…` while a sync runs, opening Settings.
- **Workspace and chat.** `Notices.tsx` renders the `NOTICES` section after
  the inbox queue, newest first, each row a Megaphone, the title, the
  posting time through `formatDueDate`, and the body clamped to three lines
  until the row is opened; absent while there are none. `tools::notices_block`
  puts the latest three in the overview — titles and dates in the compact
  form, bodies capped at 600 characters in the detailed form — and
  `chat_system.md` names `Notices:` and the `Canvas/` extracts. `ClassOutcome`
  gains `announcements_recorded` and `pages_written`; `outcomeSummary` says
  `N notices` and `N Canvas pages mirrored`.

### Verified

- `cargo test`: 195 pass, 5 new — the announcement upsert (inserted once,
  unchanged on a repeat, updated in place, refused for another class, listed
  newest first), page file stems (a repeated title, a page titled Syllabus,
  a title with a slash, a title that sanitizes away), the mirrored markdown
  and the rewrite-only-when-changed rule on disk, the launch rule at the
  day boundary, and the overview's compact and detailed notices.
  `npx tsc --noEmit` clean.
- Live on the dev build beside the installed app (pid 8598, no job of its
  own), driven through the accessibility driver:
  - **Sync 1** (pid 41145, from Settings, 15 s, the stored session live):
    `1 notice · 7 Canvas pages mirrored`, `4 notices · 6 …`, `2 notices ·
    6 …`, `2 notices · 2 …`, each with "the Canvas syllabus page was
    mirrored — SCAN SYLLABUS can read it". Nine rows in `announcements`, 21
    files under the four `Canvas/` folders; Fundamentals' `Lecture Slides.md`
    is the three deck names, its `Module 2.md` the module's objectives.
  - **Sync 2**: `nothing new` for every class, no `Canvas/` file's mtime
    changed, announcements still 9, audit log at 134, jobs at 294.
  - Design Studio's workspace shows `NOTICES · 4 FROM CANVAS` with "Today's
    Office Hours Postponed · SEP 2 3:03 PM" opening to its full text.
  - **One chat turn** from the Fundamentals workspace ("What does the Module
    2 Canvas page say students should be able to do by the end of the
    module?"): 15 s, `search_material` then `read_material` on
    `Biostatistics for AI/.classhub/extracts/Canvas/Module 2: Study
    Designs.md`, cited as a link. The model chose Biostatistics's Module 2
    over the open class's; its call, and the citation is what the criterion
    asks for.
  - **Syllabus scan** of `.classhub/extracts/Canvas/Syllabus.md` for Applied
    Generative AI (job 295, 21 s, Opus, $0.44 list-equivalent on the
    subscription): `no date-bearing items found · 3 of 3 division(s)
    recorded`. The page is a link to the PDF, so no dates was the right
    answer. See the first item under "Left as it is" for what the
    divisions did.
  - **Launch sync**: with every `canvas_synced_at` set two days back and
    the dev build relaunched (pid 41858), the sync ran on its own inside the
    first eighty seconds, the stamps read the launch time, the dashboard
    read `CANVAS · SYNCED TODAY` and Settings `SYNCED ON LAUNCH` over four
    `nothing new` lines. The launch a minute earlier, inside the day, ran
    none.
  - **Stale line**: stamps set eight days back and the page reloaded gave
    `CANVAS · SYNCED 8 DAYS AGO` in the destructive colour; the stamps were
    restored to the second.
- Not exercised live: `SignInNeeded` — the stored session was live all
  session, and lapsing it deliberately would cost a Duo round on the next
  sync. A test announcement: a student account cannot post one, so the nine
  real announcements were the data.

### Left as it is

- **A rescan can fork the divisions.** The scan of the Canvas syllabus page
  followed the link to the syllabus PDF and reported the three Parts without
  their `(Weeks 1-8)` suffixes; `units::upsert` matches a syllabus row by
  name, so three new rows landed beside the three that carry the week
  ranges M14's week-to-Part resolution reads. The three were deleted by hand
  (ids 55–57, no contribution or guide referenced them). The rule is the
  scan's, not this milestone's, and a matcher on (source, kind, ordinal)
  is the candidate.
- A launch sync that stages loose Canvas files enqueues the follow-up sort
  job the sync always has, so a launch can now start a subscription job
  with nobody pressing anything. Cheap, and the same job a manual sync
  would run.
- Two builds launched on the same stale day could each sync on launch;
  both are no-ops against the tables, and a new course file could land in
  the inbox twice under a `(2)` name.
- A Page Canvas has removed keeps its file; the sync deletes nothing.
- Link targets are dropped with the tags, so a page that is a list of file
  links mirrors as the file names, which is what a search wants.
- The announcement's author is not stored; the brief's columns were kept.

### Gotchas

- In zsh, `echo ====X====` fails with "=X=== not found": a leading `=` is
  command-path expansion. Quote such markers.
- The `/announcements` endpoint's silent fourteen-day window would have
  looked complete today and dropped the welcome posts by mid-September.
- `cd src-tauri` persists across Bash calls; a later `sed` on
  `src/components/…` failed on the relative path.
- The dev window's `DASHBOARD` button was reported NOT FOUND straight after
  a `touch index.html` reload; the reload had already put the dashboard up.

## Post-M22 — Review fixes (2026-09-03)

A two-agent review of the M22 changeset (one bug-hunting pass, one
architecture/security/data-integrity pass) produced 1 critical, 9 warnings
and 9 suggestions, 3 of them corroborated by both reviewers; all were
addressed, as individual commits where the findings were separate code
changes. What future sessions should know:

- **The Canvas texts folder is reconciled.** A page retitled or unpublished
  on Canvas kept its old markdown beside the new, and chat read both as the
  course's own words. `sync_pages` records every stem it wrote or confirmed
  and `prune_stale_texts` removes the other `.md` files in
  `.classhub/extracts/Canvas/` — the one folder the sync owns, with no
  `files` row pointing into it, which is what keeps this compatible with
  never deleting source material. SPEC §7.2 says so; the M22 "Left as it
  is" line about a removed page keeping its file is superseded.
- **Page file names are the page's own.** Canvas's URL slug is sanitized
  before it becomes part of a file name (it was raw, and `write_if_changed`
  creates parent folders), every page whose sanitized title is duplicated in
  the listing carries its slug — decided from the listing before the loop,
  so a swapped order does not rewrite two files with each other's content —
  and the title half of a stem is capped at 120 characters, since Canvas
  allows 255 and the slug and `.md` follow.
- **Neither new pass is worth the class.** `note_or_carry_on` gives grades,
  announcements and pages one shape: a refusal is a line, any other failure
  a line saying the read did not happen, and the file sync and the sync
  stamp still follow. Inside `sync_pages` a page that will not write is
  noted and kept rather than aborting the pass; both passes assign their
  counts to the outcome as each item lands.
- **A sign-in need ends a quiet sync wherever it appears.** The wrapper
  passes `SignInNeeded` up, and `run` re-raises it instead of recording it
  as one class's error, so a session lapsing mid-sync reaches the report's
  quiet note. That note keys on its own `signInNeeded` flag on the progress
  event rather than on `launch`, so a launch sync that failed for any other
  reason still reads as a stopped sync.
- **The refusal decision is pure and tested.** `read_signal` maps a 401 or
  an SSO bounce to the session being over — the only verdict that discards
  the stored cookie — any other status to a decline that keeps it (Canvas's
  rate limiter answers 403), and a stall to no evidence; `quiet_stop` gives
  each its words, `SignInNeeded` typed for the first.
- **Smaller things.** A refused announcement (a row another class holds for
  the same Canvas id) is a report line; titles are folded onto one line at
  both sinks the way bodies were, so a newline in a professor's title cannot
  open a line in the system prompt or a heading in a mirrored file; the
  overview reads `LIMIT 3` and a count through `latest_announcements`
  instead of every body; the date half of `posted_at` is taken by
  characters; `has_remembered_session` is `remembered_session_kept`, named
  for the copy it drops; the syllabus write emits a `canvasSyllabus` hub
  change so the picker offers the page as soon as it exists; a notice from
  another year carries its year in the NOTICES stamp; the Pages
  request-count comment says one request per hundred.
- **Tests**: the prune, order-independent stems with a sanitized slug and a
  capped title, the refusal verdicts and their quiet messages, the syllabus
  path resolving only when the file exists, the overview's body cap and
  ellipsis; the tests' scratch folders are removed by a drop guard.
  `cargo test`: 198 pass, no warnings. `npx tsc --noEmit` clean.
- **Live**, on the rebuilt dev build (pid 45083): a full sync of all four
  classes reported `nothing new` for each, left all 21 files under the four
  `Canvas/` folders with their mtimes, and left every count as it was
  (9 announcements, audit log at 134, jobs at 295). Nothing was pushed,
  since the repo has no remote.

### Gotchas

- `cd src-tauri` persisted into a later Bash call, so `git add
  src/components/Notices.tsx` failed on the pathspec and the following
  `git add -A` folded that fix into the neighbouring commit; the message
  was amended to name it.
- A gate script whose `cargo test` is piped through `tail | grep 'test
  result: ok'` passes on a failed lib test, because the doc-test binaries
  print their own `ok` lines last. The gate runs `cargo test` directly and
  reads its exit code.
- `sanitize_name("../week-3")` is `-week-3`: the separator becomes a dash
  and the leading dots go, which is the single segment wanted, not the
  `week-3` a first test expected.

## Weights from the syllabus (2026-09-03)

Built on the `weights-from-syllabus` branch in a worktree beside the M22
session, which held main and the dev port for the whole session.

### What was built

- **The prompt.** `syllabus.md` reports a third thing beside deadlines and
  divisions: `grading`, a list of `{name, weight}` with the weight as a
  number of percent. `build_prompt` fills a `{categories}` block with the
  class's category names — names only, so the weights come out of the
  document rather than echoing what was typed — and the grading section
  says what counts: the final-grade breakdown and nothing inside it (a
  component's own rubric and the letter scale are not shares of the final
  grade); a component that is the same work as an existing category goes
  under that name, spelled as listed; one the class lacks goes under the
  syllabus's own short name; an existing category the syllabus never
  weights is not reported, at zero or otherwise.
- **The parser.** `split_output` returns a `ScanOutput` holding the three
  lists. `grading` is read the way `units` is — absent or null is an empty
  list, present and not a list fails the scan — and an object carrying only
  `grading` is a valid answer. `RawWeight` keeps the weight as a JSON value
  and `percent_of` accepts a number or the `"50%"` the syllabus tables
  print, since a model copying the string should not cost a scan its
  breakdown.
- **The write.** `grades::set_syllabus_weight`, beside the Canvas upsert so
  the category rules stay in one module: the name is matched
  case-insensitively; a weight within `WEIGHT_EPSILON` of the syllabus is
  `Unchanged`; a zero takes the syllabus weight (`Set`); a category the
  class lacks is inserted with no Canvas id (`Created`); any other weight
  already set is `Kept(current)`, untouched. `syllabus.set_grade_weight`
  audit rows land for `Set` (before and after) and `Created` only. The
  name bound `canvas_name` became `read_name`, since both readers use it.
- **The step.** `deadlines::record_weights` mirrors `record_units`: it runs
  before the deadline half, reports and never propagates, and emits
  `hub-changed grades` only when something was written. The pass itself is
  `apply_weights` over a connection, so it is tested directly; after the
  entries it lists the categories still at zero that the scan did not
  name. Its summary reads `weights set: Assignments 50, Peer Design
  Sessions 20 (new) · N weight(s) already as the syllabus states ·
  Assignments kept at 50 — the syllabus says 40 · Survey left at 0 — the
  syllabus does not weight it · weights skipped: …`, and the job summary
  joins the three halves with ` · `.
- No schema change and no UI: the ≠100% warning and chat's weights line
  already say whether the numbers add up.

### Verified

- `cargo test`: 193 pass, 3 new — the split accepting `grading` alone and
  refusing it as an object; the Design Studio shape (three Canvas
  categories at zero filled, one created with no Canvas id, `"20%"`
  accepted, a second pass writing nothing and leaving no row); the
  Biostatistics shape (typed weights agreeing, then disagreeing and kept, a
  repeated name, a bad weight and a nameless entry all named in the
  summary); and the write itself (cross-case match, another class's row
  untouched, the audit payloads, the epsilon, the bounds). `npx tsc
  --noEmit` clean, run in the worktree against the main tree's
  `node_modules` through a symlink.
- **Not run live.** Main carried uncommitted M22 work all session, so the
  branch was not merged and no dev build was launched. The four scans and
  the rescan are still to do; what each should produce, read from the
  extracts:
  - Biostatistics (`Assignments (%10 x 5)` 50 · `Quiz (%5 x 4)` 20 ·
    `Project` 30): nothing written, `3 weight(s) already as the syllabus
    states · Survey left at 0`.
  - Design Studio (`AI Design Project` 60 · `Studio Participation` 20 ·
    `Peer Design Sessions` 20): the two matching names filled, `Peer
    Design Sessions` created with no Canvas id, `Quizzes left at 0`, sum
    100. The milestone table under the project is `% of Project Grade`
    and must not appear.
  - Fundamentals (`Weekly Live Coding Sessions` 20 · `Homework (7
    assignments)` 50 · `Final Project (Capstone Project)` 30): Homework
    onto `Assignments` at 50, the other two created, sum 100.
  - Applied Generative AI (six components, 10/10/10/50/10/10): the
    homework onto `Assignments` at 10, five created, sum 100. The syllabus
    carries no dates, so the deadline half reads `no date-bearing items
    found`.

### Left as it is

- A weight is filled once. A syllabus revised mid-semester changes nothing
  after a scan has set the number; the Grades section is where that edit
  happens, and the summary names the disagreement on every rescan.
- Which syllabus component is which category is the model's reading. A
  wrong mapping lands as a wrongly named category with a weight, which the
  summary names and the Grades section reverses; nothing is guessed on the
  Rust side beyond the case-insensitive name match.
- A category the scan creates carries no Canvas id, so a Canvas group that
  later arrives under its name claims it (the M21 rule) and keeps its
  weight.
- A component the syllabus weights at 0 and the class lacks is created at
  0. The prompt says not to report those; the write does not second-guess
  it.

### Gotchas

- A worktree builds its own `target/` from cold and has no `node_modules`;
  a symlink to the main tree's serves `tsc`, and git ignores it.
- `zsh` treats a leading `=` as equals-expansion, so `echo ====` is an
  error rather than a separator.

## Post-review — Weights from the syllabus (2026-09-03)

A two-agent review of the branch's changes since 4d36ed5 (one bug-hunting
pass, one architecture/security/data-integrity pass) produced no critical
issue, 6 warnings and 7 suggestions, 3 of the warnings corroborated by
both reviewers; all but one were addressed as individual commits on the
branch. What future sessions should know:

- **Model text reached the job summary uncapped.** The pass used the
  model's name as given in every summary line and printed a bad weight as
  its whole JSON value, while the stored name is capped by `read_name`, so
  the summary could name a category the class does not hold. The name is
  capped once at the top of the loop at `MAX_NAME_CHARS` (now
  `pub(crate)`), the bad-weight rendering the same way, and the summary
  names the stored name.
- **A scan demoted for its deadlines said nothing about its weights.**
  `record_units` and `record_weights` commit before the deadline part, and
  an all-invalid deadline batch bailed with the job's summary set to none,
  so a weight the scan had set — or a typed weight it had refused to
  change — went unreported while `hub-changed grades` had already fired.
  The divisions had the same gap before this branch. The bail's error now
  ends with the other parts' summaries, and the success path joins the
  same list.
- **Two folding rules sat on one path.** The repeat check and the
  still-at-zero filter folded Unicode; the lookup folds ASCII, SQLite's
  `LOWER()`. Both fold ASCII now, the deadline part's rule for titles, and
  a test pins that `ÜBUNGEN` is `Übungen` to both layers and `übungen` a
  second row to both.
- **The still-at-zero read is best-effort** (`still_at_zero`): every entry
  commits on its own, so a failure of that trailing read costs the summary
  a line rather than reporting the weights as not recorded.
- **Zero is read within the epsilon.** A stored 0.005 was a typed weight
  to the exact test; `current.abs() >= WEIGHT_EPSILON` is typed now, and
  `still_at_zero` asks `ABS(weight) < ?2` with the same constant. The
  read-only outcomes return before any transaction is opened.
- **A bare fraction is refused.** `percent_of` took `0.5` as half a
  percent; a bare value between 0 and 1 is skipped and named, and `"0.5%"`
  is what it says.
- `read_name`'s empty-name error no longer says Canvas; the scan's
  comments say parts rather than halves; SPEC §11 says a part tolerates a
  malformed entry and a part that is not a list fails the scan, that a
  demoted scan still reports what the other parts wrote, and what each
  audit row carries.
- **Tests**: `percent_of` in every shape; the category rules through
  `apply_weights` (an over-long name stored and reported as the same
  81-character string, a weight of exactly 100, a negative weight refused
  by name, the folding rule); `categories_block`, factored out of
  `build_prompt` because the prompt itself needs the real tree. Two of the
  three whole-string summary assertions became field assertions; the
  Design Studio one keeps the full comparison. `cargo test`: 196 pass, no
  warnings. `npx tsc --noEmit` clean.
- **Left as it is**: a unique index on `(class_id, LOWER(name))`, which
  the auditor suggested as a schema backstop for the name rule — no schema
  change in this branch, and the app check plus the connection mutex
  stand. A name capped at 81 characters (80 plus the ellipsis) cannot be
  saved again from the Grades section without shortening, since
  `valid_name` refuses more than 80; a long Canvas group name has the same
  property.
- Still not run live: the branch is unmerged and no dev build was
  launched. The merge, the four scans and the rescan are listed under
  "Verified" above.

### Gotchas

- Renaming half to part in `deadlines.rs` went through `sed` with a list
  of line-numbered `s` expressions and `#` as the delimiter, so `day half`
  and the pre-existing test name stayed as they were.
- The reviewers' reports arrive truncated at about 4,000 characters, and
  one reviewer regenerated its report with different numbering when asked
  to resend by number; asking for sections by name, and for the Verdict
  line explicitly, got the whole of both.

### Live

Run after the branch was rebased onto main at c16f66a and fast-forwarded
(bf526ab), on a dev build launched from main (pid 45655) beside the
installed app, with no job of its own and no Canvas sync due — every
`canvas_synced_at` was from today. One scan per class from the Deadlines
picker, choosing the syllabus PDF; the audit log stood at 134 and jobs at
295 before:

- **Biostatistics** (job 296, 35 s): `3 weight(s) already as the syllabus
  states · Survey left at 0 — the syllabus does not weight it`. Nothing
  written.
- **Design Studio** (297, 35 s): `weights set: AI Design Project 60,
  Studio Participation 20, Peer Design Sessions 20 (new) · Quizzes left at
  0 — the syllabus does not weight it`. Audit rows 135–137, the new
  category with no Canvas id, sum 100.
- **Fundamentals** (298, 75 s): `weights set: Assignments 50, Final
  Project 30 (new), Weekly Live Coding Sessions 20 (new)`. Audit rows
  144–146, sum 100. The deadline part proposed twelve `Live coding session
  MM/DD` cards, one per remaining dated week — new items rather than
  re-proposals, left in the queue for the reader — and the divisions part
  added two rows the first scan had not recorded, `No class (Nov. 24) —
  Thanksgiving Break` and `Finals week — Capstone Presentations`, under
  names nothing else uses, so no fork; Week 14's ordinal moved from 14 to
  15 to make room.
- **Applied Generative AI** (299, 20 s): `weights set: Quizzes 10 (new),
  Assignments 10, Project Progress Report 10 (new), Project Final Report
  50 (new), Project Presentation and Poster 10 (new), Paper Presentation
  10 (new)`. Audit rows 138–143, sum 100, `no date-bearing items found`,
  the three Parts untouched.
- **Design Studio again** (300, 50 s): `3 weight(s) already as the
  syllabus states · Quizzes left at 0 — the syllabus does not weight it`.
  Audit log still at 146, no weight changed.

Every class's weights sum to 100. The dev build was stopped afterwards;
the installed app is still the Aug 25 build until `npm run install-app`.

## M23 — Divisions that survive a rescan (2026-09-03)

### Phase 0 — measured

Against a copy of the live database (`user_version` 11, 51 units), through
`units::upsert` and `week_slots` as they stood, spending no scan:

- **Fundamentals.** The 2026-09-03 rescan (job 298) inserted `No class (Nov.
  24) — Thanksgiving Break` at ordinal 14 and moved `Week 14 — Introduction to
  Deep Learning and Course Synthesis` to ordinal 15. `week_slots` read the
  week off the ordinal, so week 14 was the Thanksgiving row and week 15 was
  the course's Week 14, with the folder `Week 15 — Introduction to Deep
  Learning and Course Synthesis`; `nearest_week` for 2026-12-01 answered 15.
- **Applied Generative AI**, fed the three suffix-less Part names the M22
  Canvas-page scan produced: 3 rows became 6 (57–59 beside 37–39), the new
  three parsed to no range, and all 16 week slots still came from the old
  three.
- **Fundamentals, `Week 2 — Responsible AI and Governance`** in place of the
  name row 24 holds: a second row (60) beside it; the contribution (unit 24),
  its corpus path and the guide row (`unit:Week 2 — …`, one manifest entry)
  all stayed with 24; week 2 got two slots, 24 first.

### What was built

- **Schema.** Migration `0012_unit_identity.sql` adds `units.number`,
  `first_week` and `last_week`, the partial unique index `idx_units_label`
  on `(class_id, source, kind, number)`, and rewrites `guides.scope` and
  `jobs.scope` from `unit:<name>` to `unit:<id>` by a join on the name.
  `units::backfill_labels` runs inside that migration's transaction
  (`db::UNIT_LABELS_MIGRATION`): each row's number from its label, a
  non-week row's range from its name, and a number another row of the same
  class, source and kind holds left empty rather than failing the launch.
- **Identity.** `units::label_number` reads `Week 7 —` as 7 and `Part II:`
  as 2 (`roman_number`), nothing for an unlabelled name. `find_held` matches
  the Canvas id, then the exact name, then `(source, kind, number)` — the
  name before the number so a fork from before labels is found as itself.
  `upsert` returns `Upserted { id, outcome, effects }` with `Outcome` as
  `Inserted | Updated | Unchanged`; `find` exposes the row a division would
  land on, so `deadlines::record_units` claims each row once and names the
  second entry under a label in `collapsed`, and a refused entry in
  `skipped`, both in the job summary (`3 of 3 division(s) updated in
  place`). A rename onto a name another row holds is refused;
  `free_number` leaves a duplicate label unnumbered.
- **The range as data.** `NewUnit.weeks` and `declared_weeks(kind, name,
  stated)`: the model's `first_week`/`last_week` when sane, else the name's
  range for a non-week row, nothing for a week. `syllabus.md` asks for the
  two fields on a division that groups weeks. A same-source refresh keeps
  the held range when none is stated — unlike a date — because a Part
  without its range files no lectures. `week_slots` reads a week row's
  `number` (ordinal fallback, numbered rows first) and then the
  `first_week..=last_week` ranges, never `parse_week_range` on a name.
- **Scope by id.** `db::unit_scope(id)` and `unit_scope_id(scope)`;
  `guides::scope_label(scope, unit_name)`; `GuideInfo.label` and
  `JobInfo.scope_label` from a `LEFT JOIN units u ON scope = 'unit:' ||
  u.id`; `extract::current_manifest` looks the unit up by id, and a scope
  from before ids has no sources; `generate_practice` takes the unit from
  the scope and lost its `unit_id` parameter; the chat's `Scope::Unit`
  carries `number` (ordinal fallback) for `week3` and resolves `unit:<id>`.
- **The rename cascade.** `rename_effects`, inside `upsert`'s IMMEDIATE
  transaction, rewrites every contribution's `corpus_rel_path`
  (`lectures::corpus_rel_path` and `corpus_folder`, now `pub(crate)`) and
  the guide row's `rel_path` (`guides::unit_guide_rel_path`), and returns
  `RenameEffects` — the corpus folder move and the guide file move — which
  `record_units` and the Canvas sync apply after the lock is released. A
  target already on disk refuses the rename inside the transaction.
  `record_units` emits `files` after a move so the guides and lecture
  listings refresh.
- **Frontend.** `unitScope(unitId)`, `GuideInfo.label`, `JobInfo.scopeLabel`;
  the client-side `scopeLabel()` is gone, and `Structure.tsx`,
  `GuideViewer.tsx`, `JobCenter.tsx` and `ClassWorkspace.tsx` read the
  label the backend sends.

### Verified

- `cargo test`: 213 pass, no warnings. New: the label parser; the M22 fork
  replayed as an update in place with the ranges kept and 16 slots, and a
  stated range replacing the held one; the inserted-row case (Week 14 at
  ordinal 15 stays week 14, Thanksgiving gets no slot, finals week takes
  16); a rename carrying the corpus folder, every stored path and the guide
  file, and refusing a target on disk; a rename onto a held name refused;
  `find` claiming one row for two entries; the backfill; the guide and job
  labels; the chat scope by id. `npx tsc --noEmit` clean.
- Live, on a dev build (pid 47103) beside the installed app: the shared
  database went to `user_version` 12 on open — every labelled row numbered,
  the Parts at 1–8, 9–12 and 13–16, guide 4 and jobs 287 and 293 rescoped
  `unit:24`. Fundamentals' Structure row for Week 2 still offers VIEW GUIDE
  and the Study Guides list labels it by name. The Add lecture form, given
  December 3 (typed through System Events — the date field takes no
  accessibility set-value, and the `12012026` keystrokes landed as 12/03),
  resolved `Week 14 — Introduction to Deep Learning and Course Synthesis`,
  filed under `Weeks/Week 14 — …`; before the change that read Week 15.
- Two scans of Applied Generative AI, both on the subscription: job 301
  (the Canvas syllabus page, 21 s, $0.35 list-equivalent) — the model again
  dropped the suffixes and reported `first_week`/`last_week` on each Part —
  `3 of 3 division(s) updated in place`, ids 37–39, ranges intact; job 302
  (the PDF, 15 s, $0.40) — `3 division(s) already recorded`. 51 units
  before and after, audit log at 146, no deadline proposals. No chat turn
  was run.
- Not read live: the Job Center's label for job 287, which sits below the
  panel's fold behind the recent scans; the join is covered by the test.

### Left as it is

- A job queued at the moment of upgrade keeps `unit:<name>` in its payload;
  only an older build could have written one, and the installed app
  predates unit guides.
- A unit scope naming a division no longer in the table keeps `unit:<name>`,
  labels as the name and has no sources.
- A table holding a fork from before labels matches the exact name first,
  and the second row stays unnumbered.
- The PDF scan now names the Parts without the suffix too (job 302), since
  the range has fields of its own; the name is the model's reading and the
  range is data either way.
- A digest running while a rescan renames its unit writes its note to the
  old path and fails its own check; a redistill repairs it.

### Gotchas

- `perl -0pi -e 's{…}{…}'` dies with "Substitution replacement not
  terminated" when the replacement holds an unbalanced `{` (`pub struct X
  {`); `/` delimiters or a file-fed replacement avoid it.
- The Add lecture form's date field ignores `AXValue` set on the
  `AXDateTimeArea` and on its incrementors; focusing it and sending
  keystrokes through System Events works, segment by segment.
- The Materials section's RESCAN is the file scan; the syllabus picker is
  behind the Deadlines section's SCAN SYLLABUS button every time.
- The tauri dev watcher rebuilds and relaunches the app on any change under
  `src-tauri/`, so the last tests were added after the dev build was
  stopped.

## Post-M23 — Review fixes (2026-09-03)

A two-agent review of the M23 changeset since eab7e17 (one bug-hunting
pass, one architecture/security/data-integrity pass) produced no critical
issue, 8 warnings and 9 suggestions, 2 of the warnings corroborated by
both reviewers. All were addressed as individual commits, except the two
observations under "Left as it is". What future sessions should know:

- **The label is read off the original text.** `label_number` sliced the
  name by the byte length of its lower-cased first word, and lower-casing
  does not preserve length for every character — a Kelvin sign lowers to a
  plain k — so such a name panicked on the scan path (both reviewers).
  `split_label` measures on the original text, and also splits a digit run
  glued to the word, so `Week7` and `Week 7` are one label.
- **Only a standard numeral is a Part's number.** `Part Civil` scored 153
  letter by letter; a run counts only when `roman(value)` writes back as
  the run itself.
- **One transaction per upsert, and a `Batch` per pass.** `upsert` opened
  its IMMEDIATE transaction after the match and the checks, so a second
  process could rename the row in between and the loser planned moves from
  a name already gone (both reviewers). The transaction opens first now.
  `find` is gone: `upsert` takes a `Batch` that holds the rows the pass has
  written — a second entry for one row comes back `Outcome::Claimed` — and
  the moves pending, so a target an earlier rename of the same pass vacates
  counts as free (the rename chain, reachable through Canvas modules
  renumbered in one sync). The row and its moves reach the batch only after
  the commit, and the batch is applied once the pass has released the lock.
- **A rename target on disk is refused whether or not the source is.** The
  check sat inside the source-exists guard while the row rewrite ran
  regardless, so a row whose file was gone could be repointed onto another
  division's (`folder_segment` is not injective). Found while fixing it:
  APFS volumes are case-insensitive by default, so a rename that only
  changed case saw its own folder as an occupied target; a target is free
  when it is the source's own inode (`same_entry`).
- **A Canvas module sharing a label is inserted unnumbered.** Two modules
  with one number and different names met on the label match, and the
  second was refused as "already called" a name nobody held; the label
  match now passes over a row Canvas gave a different id.
- **Pending moves are reported on drop.** `#[must_use]` did not cover a
  caller reading a field and letting the rest fall; `RenameEffects` says on
  stderr how many moves were never applied.
- **Migration 0012 is tested against old-style rows**
  (`db::migration_tests`): guide and job scopes by name rescoped to ids, a
  scope naming a missing division kept as it is, a folder scope untouched,
  labels and ranges backfilled, `user_version` at the end.
- **A rename refreshes the listings.** The `units` hub area now invalidates
  the guides and contributions queries, whose labels carry the division's
  name; the Canvas sync already emits `units` after every successful sync.
- **Smaller things.** The backfill is keyed on the migration's own text
  rather than its position; the range branch of `week_slots` reads only
  rows whose kind is not `week`; the frontend `Unit` type carries `number`,
  `firstWeek` and `lastWeek`; `rename_effects` says where the naming rules
  it reads live.
- **Tests**: 216 pass, `npx tsc --noEmit` clean. Every fix went through a
  gate script that runs `cargo test` to a log file and stops on its exit
  status — never piped through grep — then `tsc` when frontend files were
  staged, then commits. Nothing was pushed; the repo has no remote.
- **Left as it is**: a transcript filed under a week number the ordinal
  shift produced before M23 would resolve to no slot — none exists, the only
  filed lecture being Fundamentals' Week 02; `list_guides` running
  `current_manifest` once per guide, pre-existing and fine at a handful of
  guides per class; the module cycle between `units` and the owners of a
  division's artifacts, noted in the code.

### Gotchas

- With `perl -0pi -e 's|…|…|'`, a `\|` inside the replacement is not an
  escaped pipe: the replacement ends at the first `|` and the tail lands in
  the file. `#` as the delimiter, or the Edit tool, for text with pipes.
- `cd src-tauri` does not persist into a Bash call that opens with `sleep`;
  `cargo test` from the repo root fails on the missing Cargo.toml.
- A job's label in the Job Center cannot be read through the accessibility
  tree — the row's text sits inside a button titled "Show output of job N"
  — and the panel shows only the most recent rows; the label join has a
  unit test instead.

## M24 — The first lecture in a Part (2026-09-03)

### Phase 0 — measured

Nothing spent. The form on the dev build (pid 50460) for Applied Generative
AI with Sept 1 typed in: the WEEK popup read `Sort it into a week` with no
default, its options the bare folders `Week 01`…`Week 16`, the line under it
"Pick the week this session belongs to. It goes to the inbox until you do,
and the sorter proposes one from what the lecture covers.", the digest
checkbox disabled until a week is picked, and nothing on the form naming a
Part. Against a `.backup` copy of the live database (`user_version` 12, 51
units) through the code as it stood: `week_slots(4)` gave 16 slots, weeks
1–8 on unit 37, 9–12 on 38, 13–16 on 39, `meets_on` empty on every one, and
`nearest_week` for 2026-09-01 answered nothing; a Week 2 transcript filed to
`Weeks/Week 02/2026-09-01 — Lecture.md` with its note at `.classhub/corpus/
Part I- Deep Learning to Large Language Models/2026-09-01 — Lecture.md`, and
the same name filed into Week 03 derived the same note path; the guide path
was `Study Guides/Part I- Deep Learning to Large Language Models.html`;
`current_manifest("unit:37")` and `corpus_notes(37)` were empty. Read off
`record_contribution`: a second row naming the first row's note path, so the
second digest would have written over the first note and `corpus_notes`
listed it twice.

### What was built

- **The form asks.** `AddLecture.tsx` labels each week's option with the
  division it feeds when a unit spans more than one slot (`grouped`: fewer
  distinct `unitId`s than slots) — `Week 02 · Part I: Deep Learning to Large
  Language Models` — and, when slots exist and `defaultWeek` resolved to
  null, replaces the sorter line with "This course publishes no dates for
  its weeks, so pick the one the session fell in. Left unpicked, it goes to
  the inbox and the sorter proposes a week." A week-numbered course's form
  is unchanged.
- **One note per name in a division.** `lectures::refuse_held_note` refuses
  a transcript whose derived note path another transcript of the class
  already holds, naming the holder and the title field. `add` resolves the
  slot and the provisional unique name before `fetch`, so the refusal costs
  nothing rather than a three-hour capture; `record_contribution` checks
  again at the write, which covers the sorter's path through
  `contribution_for` and a refile inside its transaction. SPEC §7.1 and §8.5
  state the rule; §1 carries the two recordings' shapes and the costs.

### Verified

- `cargo test`: 218 pass, two new — a Part's two same-named transcripts
  refused at the row with the first row and its note untouched, a titled
  second one recorded at a path of its own, and a re-run of the first not a
  collision with itself; one week folder's second transcript taking ` (2)`
  and a note path of its own. The refile collision test asserts the new
  message. `npx tsc --noEmit` clean.
- Live on the rebuilt dev build (pid 50718) beside the installed app (8598),
  which the owner used through the run: the popup offered sixteen weeks each
  naming its Part (`phase1-popup.png`), no default, the asked-outright line.
- **Aug 25 → Week 1** (link from the owner): the capture went `waiting`
  1.6 s → `ready` 3.1 s through the store's `transcriptList` — 1,032 cues,
  every one named, 161 KB, no `fetching` state and no `ccUrl`. Filed at
  `Weeks/Week 01/2026-08-25 — Lecture.md` (117 KB, 2 h 56 m, 34 anchors from
  00:11, 161 paragraphs: 154 Xuefeng Liu, 7 Mombo Ngu), contribution row 2
  on unit 37, audit 147, the form naming both speakers. Digest job 303:
  15.5 min, 8 turns, $3.98 list-equivalent, 81k output tokens — "Generate,
  Align, Act in Medicine", 51 KB md, 68 KB html, a 31 KB note whose header
  calls the speaker labels unreliable (student turns folded into the
  instructor's blocks). It read the Week 01 deck's extract, which
  `lecture_context` lists from the bare week folder.
- **Sept 1 → Week 2**: `waiting` 1.5 s → `fetching` 3.0 s → `ready` 4.5 s
  through `ccUrl`, 629 unnamed cues, 70 KB, the first cue at 01:23 of a
  3 h 17 m recording. Filed at `Weeks/Week 02/2026-09-01 — Lecture.md`
  (49 KB, 22 anchors 01:23–03:15), row 3 on unit 37, its note beside the
  first. Digest job 304, alongside 303: 8.8 min, 5 turns, $2.11, 49k output
  tokens — "Backpropagation, Overfitting, and the MLP Lab", a 16 KB note
  opening with its coverage limits (the first 1 h 20 m absent, no speaker
  labels). Both session HTMLs have no external reference; guides rows 5 and
  6; both summaries replaced.
- Between the digests the owner approved proposals 37 and 38 (the Week 2
  deck to `Slides/`, the notebook to `Weeks/Week 02/`) in the same build;
  the write-scope guard logged the audited moves as the app's own for jobs
  303, 304 and 305 and demoted nothing.
- After both: the Part I row `2 LECTURES` with SYNTHESIZE GUIDE and PRACTICE
  EXAM, `2 SESSIONS`, both rows badged `PART I: DEEP LEARNING TO LARGE
  LANGUAGE MODELS`. **Guide job 306** (scope `unit:37`, manifest the two
  transcripts): 21.5 min, 36 turns, $7.86, 118k output tokens, 194 KB through
  one `Write` and ten `Edit`s, six sections, no `CONTINUE` marker, no external
  reference. It read each note once, the Week 01 deck PDF paged and the Week
  02 notebook, and never opened a transcript; 48 citations anchor the Aug 25
  lecture and 37 the Sept 1 one, the rest the deck's slides and the
  notebook's sections. Guides row 7; the row read VIEW GUIDE with the quiet
  resynthesize affordance.
- **Staleness**: a two-cue caption fixture filed into Week 04 with the digest
  off (row 4 on unit 37, `2 LECTURES` unchanged) flipped the row to
  `STALE — RESYNTHESIZE` with both notes at their 12:49 sizes and times. A
  move proposal out of `Weeks/` (a `chat`-sourced row inserted by hand, the
  shape `propose_file_moves` writes) was refused at `_Inbox/` — an
  app-managed destination — and approved at the class root: row 4 cleared,
  audit 162, the row fresh again. The file and its extract were deleted, the
  empty `Week 04` folders removed, and RESCAN left 7 file rows, 2
  contributions, 3 guides, 51 units, jobs at 307, audit at 162.
- Not run: any chat turn. Session cost on the subscription: $13.95
  list-equivalent (two digests, one guide). Job 307, a $3.26 Biostatistics
  extract the owner's own edit demoted, was not this session's.

### Left as it is

- **The week folders' material is not in a Part's manifest.** The guide
  cited the deck under `Weeks/Week 01/` and the notebook under `Weeks/Week
  02/` throughout, found through `--add-dir`; neither is the Part's folder
  (`units.rel_path` is NULL) nor a contribution, so a changed deck leaves the
  guide reading fresh and the prompt's file listing was empty. The same holds
  for a week-numbered course's own week folder. Widening `current_manifest`
  to the week folders a unit's slots name would flip the just-built guide
  stale, and the budget allowed no rebuild — the next Part I guide, stale
  anyway once Week 3 lands, is the moment to do it.
- A transcript deleted in Finder keeps its contribution row and its note
  (`scan_class` never touches `lecture_contributions`); the refile path is
  the one that clears them.
- An edit of the owner's own during an extract job demotes the job (307):
  the guard excludes audited app moves only, by SPEC §6, while the stream
  log records every write the job made and could tell the two apart.
- The Materials tree still offers SYNTHESIZE GUIDE on `Slides` and `Weeks`.

### Gotchas

- Setting the RECORDING field through accessibility re-derives the date, so
  the date is typed after the source, and the first pass at the segments
  landed wrong every time (`2026-12-03`, `0002-09-03`, today); a second pass
  with longer pauses took, except once, which is why the probe is named
  `2026-09-03`. The WEEK popup takes a typed `Week 02` and Return.
- The Part I row's SYNTHESIZE GUIDE is the first in tree order; the Materials
  folders' come after it.
- The owner used the dev build during the run, so the view moved between
  dumps; every dump was re-taken after opening the workspace.

## Post-M24 — Review fixes (2026-09-03)

A two-agent review of the M24 changeset since 93250cc (one bug-hunting
pass, one architecture/security/data-integrity pass) produced no critical
issue, 4 warnings and 4 suggestions, two of each corroborated by both
reviewers. All were addressed as individual commits, the two form
suggestions as one since they share their lines. What future sessions
should know:

- **The refusal names the file, not only the form.** "Give this lecture
  a title" reached the sorter's approve card and the DISTILL button, which
  have no title field; the message now names the form's title or a rename
  of the file (both reviewers).
- **A stale holder gives the name up.** A transcript deleted in Finder
  keeps its row (`scan_class` never touches `lecture_contributions`), and
  the new check refused a real lecture in the name of one not on disk.
  `refuse_held_note` clears a holder whose transcript is gone and lets the
  newcomer take the name; a note it left is the next digest's to replace.
- **The filing is settled in one testable step.** The pre-capture
  resolution lived inline in `add`, behind an `AppHandle`, so the claim
  that a refusal costs no capture had no test. `resolve_filing` holds the
  week's folder or the inbox, the free name and the refusals, and a test
  walks the inbox route, the bare folder of a Part-numbered course, the
  held name, the ` (2)` suffix and a title of its own.
- **A note on disk refuses too.** A refile to an undeclared week keeps the
  note for the refile back, so a same-named filing could have written over
  it. The form refuses it — a title is a field away — and the sorter's
  path does not, since there the note is the lecture's own coming home.
- **A refusal at the write removes the transcript.** The form's check and
  the write are separate lock acquisitions with the capture between them;
  a row taking the name meanwhile left a file for the next scan to index
  as a lecture no division reads. The file goes with the error now.
- **Both note guards are pinned.** The refile collision test asserts the
  row guard's message, then drops the other lecture's row with its note
  still on disk and asserts the on-disk guard refuses the move.
- **The option names the division by kind.** Counting distinct unit ids
  against slots missed a Part spanning one week; `WeekSlot.unit_kind`
  says whether the division is the week itself, and the form names any
  other kind beside the bare folder. **The ask keys off the dates**: a
  date the field cannot parse also resolves to no default, so the line
  reads the slots' `meets_on` rather than the default (both reviewers on
  the first; the second DB1's).
- **§7.1 says the inbox.** "Files nowhere until one is picked" contradicted
  the `_Inbox/` route the form's own line describes (both reviewers).
- **One note per name is a unique index.** `refuse_held_note` is one
  process's promise and two builds share the database; migration 0013
  adds `UNIQUE(class_id, corpus_rel_path)` on `lecture_contributions`,
  reducing rows that already collide to the earliest — the row whose note
  the rule protects — with a migration test. Live: a dev build (pid 53569)
  took the shared database to `user_version` 13 on open, the index
  present and the three contribution rows intact; its launch scan also
  picked up the owner's stale Biostatistics PDFs as extract job 308.
- **Tests**: 220 pass, `npx tsc --noEmit` clean. Every fix went through a
  gate script that runs `cargo test` to a log file and stops on its exit
  status — never piped through grep, which is how it caught a compile
  error in the first attempt at the `resolve_filing` test — then `tsc`
  when frontend files were staged, then commits. Nothing was pushed; the
  repo has no remote.
- **Left as it is**: the week folders' material outside a Part's manifest
  (the M24 note above); `scan_class` not clearing rows for deleted
  transcripts, softened now by the stale-holder clearing at the next
  same-named filing; an edit of the owner's own demoting a running extract.

### Gotchas

- `status` is a read-only variable in zsh; a script that assigns it dies
  at the assignment with "read-only variable".
- `expect_err` needs `Debug` on the `Ok` type, and a struct holding a
  `WeekSlot` has none; `.err().expect(..)` asserts the same thing without.
- The launch scan of any build enqueues whatever is stale in every class,
  so a dev build brought up for a migration check may start an extract job
  of the owner's; wait for it before stopping the build.

### After the review — the elapsed label across a sleep (2026-09-03)

- Extract job 308 — the owner's seven Biostatistics PDFs, picked up by the
  migration-check dev build's launch scan at 13:38 — was running when the
  lid closed at 14:28; the machine woke at 15:02 and the job finished at
  15:06 with every extract recorded. Fourteen seconds after the wake the
  pill read `EXTRACT 83:34` for a job that had run fifty minutes, and
  nothing on screen told a suspended job from a hung one.
- The job store's one-second ticker now records a tick that arrives more
  than thirty seconds after the previous one as a sleep — when it began and
  for how long — and `formatElapsed` subtracts the sleeps that began after
  a job started, so the label reads the time the job actually had. The
  list is cleared once nothing is active. Thirty seconds is above anything
  App Nap's timer coalescing produces and below any lid-close worth
  noticing; a relaunch forgets the sleeps, which is the rare case.
- The Job Center's live output remains the way to tell a hung job from a
  suspended one; nothing here changes what the runner does.

## M25 — What a week's folder holds (2026-09-03)

### Phase 0 — measured

Nothing spent. Against a `.backup` copy of the live database
(`user_version` 13, 35 file rows, 3 contributions, 7 guides, jobs at 308,
audit at 162, no active job) through an ignored probe test in `guides.rs`,
under the code at 162b114: `current_manifest("unit:37")` was the two
transcripts and its `files_block` empty, the slots weeks 1–8 as bare
folders; `current_manifest("unit:24")` was its one transcript and its
listing empty; all five guides fresh. On disk, `Weeks/Week 01/` held the
deck and `Weeks/Week 02/` the notebook, both indexed and extracted, and
Fundamentals' Week 02 folder held its transcript alone beside an empty
`Week 03 — Biomedical Data Foundations/`. Under the widened code, same
copy: unit:37's manifest four entries — the deck and the notebook beside
the transcripts — its listing naming the deck with its extract and its
original and the notebook with its extract, row 7 stale, row 4 fresh. The
installed app is the Aug 25 build; no dev build was running; the Sept 8
lecture is next Tuesday's, so the fallback branch applied.

### What was built

- **The week folders count.** `extract::current_manifest` for a unit
  scope unions in every `files` row under `Weeks/` whose folder's week
  (`units::week_from_rel_path`) is one of the weeks `week_slots` maps to
  the unit — by number, so a week the syllabus renamed still counts the
  folder it was filed under. `files_block` is manifest-driven, so the deck
  and the notebook are listed with their extracts while the contributing
  transcripts stay listed apart through their notes; `unit_context`'s
  refusal names the weeks.
- **The row offers what the backend accepts.** `UnitInfo.materials` — the
  manifest less the contributing transcripts, filled by `list_units` —
  gates SYNTHESIZE GUIDE and PRACTICE EXAM in `Structure.tsx` beside the
  distilled count. `relPath !== null` left the gate: an empty folder was
  refused anyway.
- **A scan settles what a vanished transcript leaves.**
  `scanner::scan_class` returns `Scan { tree, changed }`. For each path
  the index held and the walk did not, `lectures::lecture_left` refiles it
  through `refile_lecture` when a scanned file the index did not hold
  carries the same hash, and otherwise clears the session row with both
  documents (`drop_session`, now shared with `refile_session`), the
  contribution row and the note. Each lecture runs on its own savepoint,
  effects apply after the commit, and a refusal is logged with the row
  left as it was. `NoteMove::Remove` prunes the emptied corpus folder the
  way `Relocate` does.
- **The `index` hub change.** The scan command and the launch scan push
  it when the index changed; `query.ts` refetches guides, units,
  contributions and classes, never the tree, which is the scan's own
  result. Before this a RESCAN that added a file beside a transcript moved
  no badge until a job settled.
- SPEC §4, §7 step 5, §7.2, §8.1, §8.5 and §13 state the design.

### Verified

- `cargo test`: 224 pass, four new — a Part's manifest over its own
  weeks' folders and not the next Part's, a renamed week's folder counted
  by number, staleness flipping on the deck's hash; the listing naming the
  deck and not the transcript (the unit-context test, extended); the
  materials count; a scan forgetting a deleted transcript (row, note,
  emptied corpus folder, session row, both documents, and a second scan
  unchanged) and refiling a moved one (row on Part II, note relocated,
  session row rekeyed, document kept). `npx tsc --noEmit` clean.
- Live on the dev build (pid 56780) beside the installed app (8598); the
  launch scan found nothing stale and enqueued no job. The Part I row read
  `2 LECTURES · STALE — RESYNTHESIZE · PRACTICE EXAM · VIEW GUIDE` on the
  widening alone, and Parts II and III offered nothing. Fundamentals' Week
  2 row read `1 LECTURE · VIEW GUIDE` fresh, `STALE — RESYNTHESIZE` after
  `m25-fixture.csv` was dropped beside its transcript and RESCAN pressed
  (extracted locally, no job), and fresh again after the file was removed
  and RESCAN pressed; the empty Week 3 folder offered nothing.
- A two-cue caption filed through the form into Week 04 with the digest
  off (`Weeks/Week 04/2026-09-03 — Lecture.md`, row 4 on unit 37, audit
  163), then given a note at its corpus path and a session row with two
  documents by hand: reopening the workspace read `3 LECTURES` and `3
  SESSIONS`. Moved in Finder to `Week 09` and RESCAN: row 4 on unit 38
  with its note under `Part II- …`, session row 8 rekeyed, the Lectures
  row badged `PART II`, Part I `2 LECTURES`, Part II `1 LECTURE` with
  SYNTHESIZE GUIDE, the log naming the move. Deleted in Finder and RESCAN:
  two rows for the class, guides 5–7, files at 35, the note and both
  documents gone, the log naming the loss. The emptied `Part II- …` corpus
  folder stayed, which is the prune added afterwards and pinned in the
  scan test. Fixture folders and extracts removed and RESCAN: 35 files, 3
  contributions, 7 guides, 51 units, jobs at 308, audit at 163.
- Not run: any chat turn, digest or guide. Session cost: nothing on the
  subscription, nothing on credits.

### Left as it is

- The Part I guide (row 7) reads stale until the Sept 8 lecture is filed
  and the guide rebuilt from three notes: its manifest lacks the deck and
  the notebook it read, and a rebuild from the same two notes would be
  stale again in five days.
- A row written by hand — a note at the corpus path, a session row — is
  not a scan change, so it shows after a reopen rather than a RESCAN.
- The scanner leaves the extract of a deleted file under
  `.classhub/extracts/`.
- The Materials tree still offers SYNTHESIZE GUIDE on `Slides` and
  `Weeks`; an edit of the owner's own during an extract job demotes the
  job.

### Gotchas

- The WEEK popup took a typed `Week 04` as Week 01 this time; `ax press
  Week` opens the menu and `ax press '<item title>'` picks the item.
- The digest checkbox is checked by default and enabled once a week is
  picked; uncheck it before ADD LECTURE or a fixture costs a digest.
- A `.md` fixture in a week folder is a lecture to the listing; a `.csv`
  extracts locally and spawns no job.
- Killing the app pid ended `tauri dev` and freed :1420.

## Post-M25 — Review fixes (2026-09-03)

A two-agent review of the M25 changeset since 162b114 (one bug-hunting
pass, one architecture/security/data-integrity pass) produced one
critical issue, five warnings and eight suggestions, one of them
pre-existing; four of the warnings were corroborated by both reviewers.
All were addressed as individual commits, through a gate that runs
`cargo test` to a log and stops on its exit status, then `tsc` when
frontend files are staged. What future sessions should know:

- **A parked transcript is not gone (critical).** The walk skips
  `_Inbox/`, `Notes/` and `Study Guides/`, so a transcript dragged into
  one of them vanished from the index exactly like a deleted one and the
  scan removed its note and its session documents while the file sat in
  the inbox. A vanished path is settled only when something is keyed by
  it (`lectures::keyed_by`), and `parked_in_app_managed` reads the three
  folders flat — the way the sorter reads the inbox — for a file of the
  same size and hash before anything is forgotten. The rows and the note
  wait; the index still reflects the walk.
- **A refused settle is retried.** The `files` row of every vanished
  path was deleted before the per-lecture savepoints, so a refusal — a
  refile onto a note another lecture of the Part holds — rolled back the
  lecture and still lost the row, and no later scan could try again. The
  index row now goes on the lecture's own savepoint, and a move candidate
  may be a path the index already holds when nothing is keyed by it,
  which is what a refused refile's destination looks like next time. The
  test runs two passes and is refused on both with the rows untouched.
- **A shared hash is not a move.** A move is one vanished lecture's
  content found at exactly one unkeyed path; two lectures with one
  content, or one content found twice, are settled as gone, so one refile
  cannot re-key a session row the other then drops with its documents.
- **A note is removed only at a corpus note's path.** `corpus_note_path`
  admits plain components under `.classhub/corpus/`, a division folder
  and a file deep; the remove, the relocate and the prune all go through
  it, so a row naming anything else removes nothing (both reviewers).
- **The launch scan pushes `files`.** Its tree reaches no one, so an open
  workspace has to fetch it; the RESCAN command keeps `index`, whose tree
  is its own result (both reviewers).
- **`changed` counts a folder attached to a division**;
  `attach_rel_path` reports whether a row took the folder.
- **A week folder opens with its week.** `week_from_rel_path` no longer
  reads "week" anywhere in a folder's name, so `Midterm Review (Weeks
  1-6)` under `Weeks/` is no week's, for the manifest and the join alike.
- **A week claimed twice is logged** at both skips in `week_slots`.
- **The slots and the `Weeks/` index are read once per listing.**
  `extract::unit_manifest` takes them from its caller; `current_manifest`
  reads them for one scope and `list_units` once for the class.
- **`materials` is `None` until a listing counts it**, and the row's gate
  reads null as zero; the comment on `refile_session`'s destination clear
  states the invariant the scan's caller shares; the markdown twin strips
  one `.html` (pre-existing).
- **Tests**: seven new — parked, refused-and-retried, twins, a deleted
  deck and an edited transcript, a session row without a contribution
  row, files under `Weeks/` outside a week folder, and the note-path
  guard; 230 pass, `npx tsc --noEmit` clean. Nothing was pushed.
- **Not changed**: the units ↔ extract module cycle (`materials` reads the
  manifest, the manifest reads the slots) stays until a third crossing
  appears, which the reviewer's own suggestion made the condition. A
  crash between the scan's commit and its filesystem effects leaves a
  note or a document on disk that no row names; a transcript deleted from
  `_Inbox/` after being parked keeps its rows until the next same-named
  filing clears the stale holder. Both are rare and recorded here rather
  than built for.

### Gotchas

- A gate that fails leaves the staged files staged: the next
  `git add <one file>` then commits everything under one message. Reset
  with `git reset --soft HEAD~1 && git reset` and re-stage per fix.
- `memory_db` enforces foreign keys: a contribution row needs a real
  `units` row, or the insert fails with a constraint violation.
- Two same-named transcripts in two weeks need session documents named
  for the whole path in a fixture, or a `guides.rel_path` lookup answers
  for the wrong one.
- A `_Inbox/` fixture in a scanner test needs the folder created first;
  the scratch class folder starts empty.

## M26 — A Tuesday's two lectures (2026-09-03)

### Phase 0 — measured

Nothing spent. Against a `.backup` copy of the live database
(`user_version` 13, 35 file rows, 3 contributions, 7 guides, jobs at 308,
audit at 163, no active job, every `canvas_synced_at` 2026-09-03) through
an ignored probe test in `guides.rs`, under the code at 4aabfa6:
`week_slots(1)` gave 15 slots and `nearest_week` for 2026-09-08 answered 3
— `Week 03 — Biomedical Data Foundations`, unit 25, meeting 2026-09-08 —
so the Fundamentals form resolves the week; `week_slots(4)` gave 16 slots
with no `meets_on`, `nearest_week` answered nothing, and week 3 mapped to
unit 37 (Part I, kind `part`, folder `Week 03`), so the Applied form asks.
`unit_context(4, 37)`: a four-entry manifest (the two transcripts, the
Week 01 deck, the Week 02 notebook), the deck listed with its extract and
its original and the notebook with its extract, neither transcript in the
listing, and two notes in the corpus block; `unit_context(1, 25)` refused
— nothing filed, no folder, nothing distilled. Guides: row 7 stale, row 4
fresh, the three session rows fresh. The M25 dump of the Applied workspace
showed `Slides`, `Syllabus` and `Weeks` each carrying SYNTHESIZE GUIDE and
PRACTICE EXAM; the top-level folders across the tree are Design Studio
`Syllabus`; Applied `Slides`, `Syllabus`, `Weeks`; Biostatistics `Coding
Material`, `Module 1`, `Module 2`, `Reading Material`, `Slides`,
`Syllabus`; Fundamentals `Syllabus`, `Weeks`. The extract mirror held
exactly the 35 indexed sources — no orphaned entry — and one leftover: the
empty folder `.classhub/extracts/Weeks/Week 03 — Biomedical Data
Foundations/` in Fundamentals, where M25's fixture extract had sat. The
installed app is the Aug 25 build; no dev build was running. Today is
Sept 3: neither Sept 8 recording exists, confirmed when asked, so the
fallback branch applied.

### What was built

- **The tree offers a guide where a folder is a unit of material.**
  `TreeNode.labelled` — `units::label_number` on the folder's name, the
  read §7.2 matches a folder to a division by — is set by `walk_dir`, and
  `FileTree`'s `DirNode` renders the guide cluster on a top-level folder
  when it is labelled or when a guide is already built for that scope, so
  nothing built goes unreachable. `Module 1` and `Module 2` keep the
  cluster; `Weeks`, `Slides`, `Syllabus`, `Coding Material` and `Reading
  Material` keep their file count and lose it. `synthesize_module` and
  chat's folder scope are untouched.
- **A vanished file's mirror entries go with its row.**
  `extract::remove_mirror` removes the five `MIRROR_SUFFIXES` entries under
  `.classhub/extracts/` for a plain class-relative path and prunes the
  folders they emptied up to the mirror root, which stays. `scan_class`
  collects every vanished path whose savepoint committed — not keyed,
  parked, or settled — and calls it after the transaction's commit; a
  refused settle keeps its row and its extract.
- SPEC §7 step 1, §8.3, §8.5, §13 and §14 state the design.

### Verified

- `cargo test`: 232 pass, two new — a deleted deck's extract, conversion,
  sidecar and emptied mirror folders gone with the root kept; the walk
  marking `Module 1` and not `Slides`, `Weeks` or a file — and two
  extended: the moved transcript's old-path extract gone, the refused
  settle's extract kept over both passes. `npx tsc --noEmit` clean.
- Live on the dev build (pid 60107) beside the installed app; the launch
  scan found nothing stale and enqueued no job. The Applied Materials rows
  `Slides`, `Syllabus` and `Weeks` carried no cluster while Part I's
  Structure row still read `STALE — RESYNTHESIZE · PRACTICE EXAM · VIEW
  GUIDE`; Biostatistics' `Module 1` row read `STALE — RESYNTHESIZE ·
  PRACTICE EXAM · VIEW GUIDE` and `Module 2` `SYNTHESIZE GUIDE · PRACTICE
  EXAM`, its four storage folders nothing. The Applied form with Sept 8
  typed read `Sort it into a week` under "This course publishes no dates
  for its weeks, so pick the one the session fell in"; the Fundamentals
  form read `Week 03 — Biomedical Data Foundations`, "Filed under
  Weeks/Week 03 — Biomedical Data Foundations, feeding Week 3 — Biomedical
  Data Foundations". `m26-fixture.csv` dropped into that empty folder and
  RESCAN: extracted locally, no job, the Week 3 row `SYNTHESIZE GUIDE ·
  PRACTICE EXAM`; deleted in Finder and RESCAN: the extract and the mirror
  folder gone — M25's leftover folder with it, since the fixture's extract
  landed there — the `Weeks/` mirror still holding Week 02, the row
  offering nothing. Database back at 35 files, 3 contributions, 7 guides,
  jobs at 308, audit at 163.
- Not run: any chat turn, digest or guide. Session cost: nothing on the
  subscription, nothing on credits.

### Left as it is

- The Sept 8 lectures: both forms are ready, and the Part I guide (row 7)
  reads stale until Applied's lecture is filed and the guide rebuilt from
  three notes with the deck and the notebook listed; Fundamentals' Week 3
  guide waits for its note. §1's cost of a guide whose files are listed
  rather than found is still unmeasured.
- `units: … claims week 14 …` is logged on every `week_slots` call — seven
  lines per RESCAN on the dev build — since Post-M25 made the skip say so.
- A folder guide built through chat for a storage folder (`Slides`) shows
  on its row from then on, by the guide half of the gate.

### Gotchas

- `ax dump` lists buttons and fields only this session; `ax text` carries
  the static text (the form's help lines, the Structure rows' names).
- The WEEK popup's menu is not in the window's AX tree: a dump while it is
  open shows the form alone; `ax press '<item title>'` still picks.
- Typing a date segment: `ax focus <segment>` then a System Events
  keystroke; zsh does not word-split an unquoted variable, so a `for` over
  "month 09" pairs needs explicit arguments.
- Killing the app pid ended `tauri dev` and freed :1420 within seconds.

## Post-M26 — Review fixes (2026-09-03)

A two-agent review of the M26 changeset since 4aabfa6 (one bug-hunting
pass, one architecture/security/data-integrity pass) produced no critical
issue, four warnings, eight suggestions and one pre-existing finding both
reviewers raised. Each was addressed as its own commit through
`scripts/gate.sh`, which runs `cargo test` directly and `tsc` when
frontend files are staged. What future sessions should know:

- **An extract recorded for a row that is gone is removed.** The scan
  takes no pipeline lock, so a source deleted between `stale_files` and
  the write of its extract has no row when `record` runs; the update
  matched nothing and the extract stayed as an orphan chat's search still
  walked. `record` reads the row count and clears the mirror when no row
  took it.
- **The Canvas texts folder is never cleared as a source's mirror** (both
  reviewers, pre-existing): a top-level source folder named `Canvas`
  mirrors into the sync's own `.classhub/extracts/Canvas/`, and the
  clearing had turned a write collision into a delete collision.
  `remove_mirror` skips that folder; the write collision itself is
  recorded here rather than built for, since no class folder is named
  `Canvas`.
- **`remove_mirror` is tested directly** (both reviewers): the path guard
  (the empty path, `..`, `./`, an absolute path, a parent mid-path), a
  top-level source whose parent is the root, a folder a sibling's extract
  still uses, and the parked branch's clearing.
- **The connection guard is released before the post-commit effects**;
  the prune loop's unreachable clause went; the list is
  `mirrors_to_clear`; `labelled`'s doc says it is read on the top-level
  rows; `extract::mirror_entry` is the one writer of a mirror entry's
  path, used by the sorter's move, its undo and the remover.
- **A folder with a job in flight keeps its cluster**: the tree's gate
  counts a running guide or exam job beside a built guide, applied once
  where the controls are narrowed; SPEC §8.3 and the cluster's docstring
  state the design.
- **Not changed.** The gate's reach: a top-level folder named `Unit 1` or
  `Module 0` gets no cluster from the tree, and chat's `trigger_synthesis`
  by name is the route — the guide it builds puts the cluster on the row —
  while a wider gate would serve a folder scheme none of the four courses
  uses. A source replaced by a symlink reads as vanished and loses its
  mirror with its row: the walk never followed symlinks, so a symlinked
  source is not material, and a mirror kept for a row that is gone is the
  orphan the feature removes.
- **Tests**: 234 pass, three new; `npx tsc --noEmit` clean. Nothing was
  pushed.

### Gotchas

- `npm run install-app` ran from the session for the first time and produced
  a newer bundle: efc8505 at `/Applications/ClassHub.app`, the binary dated
  thirty seconds after the commit, relaunched on the shared database. The
  installed app is no longer the Aug 25 build.
- The reviewers' reports arrive truncated near 4,000 characters; asking each
  for the Verdict and a numbered index first, then the rest by section, took
  one follow-up message per agent.
