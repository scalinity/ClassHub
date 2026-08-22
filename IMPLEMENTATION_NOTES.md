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
