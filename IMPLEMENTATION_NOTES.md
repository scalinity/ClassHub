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
