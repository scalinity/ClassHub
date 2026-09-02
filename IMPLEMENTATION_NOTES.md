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
