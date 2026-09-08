# ClassHub — project rules and session facts

Personal, local-only macOS app: Tauri v2 with a Rust core, React 19 + TypeScript strict
(no `useEffect`; TanStack Query for server state). `SPEC.md` is the design and its §14 the
session protocol — read it in full at the start of a milestone session. `IMPLEMENTATION_NOTES.md`
holds dated history, one section per milestone; each milestone has a brief in `milestones/`.
`AGENTS.md`'s rules apply: read the frontend-design skill before any UI work; commit only
once a milestone's acceptance criteria are verified.

## After every milestone or changeset lands

- Run `npm run install-app` once the commits are in (the review-fix commits included). This
  is the explicit instruction to build: the script quits the installed app, runs
  `tauri build`, replaces `/Applications/ClassHub.app` and relaunches it. Confirm with
  `stat -f '%Sm' /Applications/ClassHub.app/Contents/MacOS/classhub` against
  `git log -1 --format=%ci`; the relaunched app scans on launch and syncs Canvas if the last
  sync is a day old. A dev build may stay up; the script quits only the installed instance.

## One database, two processes

- Database: `~/Library/Application Support/com.danny.classhub/classhub.db`, WAL,
  `user_version` 13; job logs under `logs/` beside it. Read it with `sqlite3 -readonly`.
- The installed app and a dev build share it. Check `lsof -nP -iTCP:1420` before
  `npm run tauri dev`; find the dev build with `pgrep -f target/debug/classhub` and stop it by
  pid, which ends `tauri dev` and frees the port.
- Any build's launch scan enqueues one extract job per class with stale files; wait for it
  before stopping a build. Never edit under `src-tauri/` while a job runs in the dev build:
  `tauri dev` rebuilds and relaunches on any change there, which orphans the job.
- A launch with a stored Canvas session and a day-old sync syncs on its own about a minute
  in; age `canvas_synced_at` only to provoke it. A live Canvas sync or a Zoom link needs the
  owner at the sign-in window.

## What costs what

- Chat turns spend API credits. Digests, guides, practice exams, PDF extracts, sorts and
  syllabus scans spend the Claude subscription through `claude -p` (Opus at `xhigh` by
  default). Text formats extract locally for nothing. Run only what acceptance needs, and a
  second guide or digest only if the first is wrong. Measured costs live in SPEC §1.

## The four classes (seeded ids; the tree root is `~/Documents/AIBHS`)

| id | display_name | folder | meets |
| --- | --- | --- | --- |
| 1 | Fundamentals of AI in Medicine I | Fundamentals of Artificial Intelligence in Medicine I | Tue |
| 2 | AI in Health Design Studio I | AI in Health Design Studio I | Wed |
| 3 | Biostatistics for AI | Biostatistics for AI | Thu |
| 4 | Applied Generative AI in Medicine | Applied Generative AI in Medicine | Tue |

Unit ids, guide rows and job ids are data: read them from the database at the start of a
session rather than from a prompt.

## Driving the app

- `scripts/ax-driver/ax <pid> raise | dump | press <name> | focus <name> | setvalue <name>
  <value> | text` — a Swift accessibility driver, arm64; its source was never committed and
  these binaries are the only copy. `AX_NTH=n` picks the nth element of a name. `dump` lists
  controls; `text` includes static text. `scripts/ax-driver/winid` prints window ids for
  `screencapture -l`. Raise, then dump twice; reopen the workspace before each dump, since the
  owner may be using the window. A modal (the Add lecture form) hides the rest of the page
  from AX, and the Week popup's menu is not in the tree — `press '<full item title>'` still
  picks an item.
- Add lecture form: `setvalue 'Recording link or file' <path>` first, then each date segment
  with `focus month|day|year` and a System Events keystroke (expect a second pass); the digest
  checkbox is on by default once a week is picked — uncheck it before Add lecture on a fixture
  (the dialog's own button is the second of that name).
- Fixtures: a `.md` under `Weeks/` reads as a lecture; use a `.csv`, which extracts locally
  for nothing. Never edit source material to test staleness — add a file beside it.
- A workspace dump opens with the section links (Notices … Notes, those present) before any
  section's controls, so a folder row of the same name — `Notes` — is the second of that name
  (`AX_NTH=2`). A card is `Open <display_name>`.
- Vite HMR of `query.ts` creates a second `QueryClient`; verify cache changes after a full
  reload (`touch index.html`), not after HMR. A render error while an edit is half-applied
  blanks the window and HMR does not bring it back; the same `touch` does.

## The facelift (M30)

- Merged into `main` on 2026-09-04 (1af2747, the installed build). The original look is the
  `pre-facelift` tag (84454ad): `git switch --detach pre-facelift && npm run install-app`
  shows it again, and `git switch main && npm run install-app` returns. The `facelift`
  branch points at the same commit and can be deleted.

## Commit pipeline

- `scripts/gate.sh <message-file>` runs `cargo test` directly (its exit status decides),
  `tsc --noEmit` when frontend files are staged, then `git commit -F`. A failed gate leaves
  the staged files staged. Stage each fix's files on their own.
- Message in a technical voice, no attribution, no names. A remote `origin` exists; never push.
- After the milestone commit: `/review-2` against the changes since the pre-milestone commit,
  asking each agent to put its Verdict and a numbered finding index first (reports arrive
  truncated near 4,000 characters, so ask for the rest by section); `/address` every finding,
  skipping the push step; record a "Post-M<N> — Review fixes" section in the notes; commit;
  then install.

## Documents

- A milestone brief, `milestones/M<N>-<slug>.md`, follows the previous one: Why, Phase 0
  (measure without spending a token, against a `.backup` copy of the database), the phases,
  Acceptance, Watch for. Its §14 entry is ticked when accepted. SPEC sections state the
  present design only — no history trails. The notes section for a milestone carries Phase 0
  measured, What was built, Verified, Left as it is, Gotchas.
- Prompts are templates in `src-tauri/prompts/`. The app never shows the word "unit"; it uses
  the course's own words (Week, Part, Module).

## Tests

- `cargo test` in `src-tauri/`. `db::memory_db()` applies the migrations and seeds the four
  classes (ids 1–4) with foreign keys enforced, so a contribution row needs a real `units`
  row. `scanner::tests::part_numbered_class` is the scratch-class fixture in Applied's shape
  (Parts over week ranges). Two same-named transcripts in two weeks need session documents
  named for the whole path.
- No TypeScript unit tests by convention; the UI is verified on the dev build.

## Shell gotchas (zsh)

- `status` is a read-only variable; unquoted variables do not word-split; a word that opens
  with `=` is expanded as a command path, so `echo ====` fails; never use `|` as a perl
  delimiter when either side holds a pipe; a `cd` in one Bash call does not carry into the
  next — use absolute paths or `git -C`.
