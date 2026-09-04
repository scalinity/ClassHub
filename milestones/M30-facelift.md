# M30 — The facelift

> **Read `SPEC.md` in full first**, then this file. §12 (the design language this milestone
> replaces and then states), §13 (no `useEffect`; model output never reaches the DOM
> unfiltered), §8.5 (the current division on the card and the band), §10 steps 7 and 8 (the
> two `File under Week NN` labels), and §14's session protocol.

## Why

Every label, date, count and button in the app was set in 10px monospace tracked capitals
(145 `tracking-[` sites, 229 `font-mono` sites, 26 `toUpperCase()` calls under `src/`), every
section title sat on a hairline rule, meta strings were chained with middle dots, and the
ground was a near-black. It was coherent, and it read as a generated dashboard rather than a
study desk. The type scale was fourteen arbitrary pixel sizes, several half a pixel apart.

The facelift replaces the register rather than retouching it: one clean sans with weight and
tracking carrying the hierarchy, paper and ink grounds, the four class colours as washes,
sentence-case labels that say what happens, and a workspace band with a sticky row of section
links. It touches `src/` and the documents only — no Rust, no migration, no dependency — and
lives on the `facelift` branch beside a `pre-facelift` tag on `main`, so the original is one
`git switch main && npm run install-app` away.

Not built: a bundled typeface, a theme toggle, active-section tracking in the nav, any change
to the generated guides' own design (the prompts under `src-tauri/prompts/`), any change to
what a section does, and any change to an `aria-label` beyond the seven built from visible
titles.

## Phase 0 — Measure

Without spending a token, against a `.backup` copy of the live database through the code as it
stands:

- The register's footprint under `src/`: `tracking-[`, `font-mono` and `toUpperCase()` counts
  (expected: the three numbers above; the after-numbers are 0, code-and-paths only, and two).
- The installed app on nine surfaces in light and dark mode through the ax driver and
  `screencapture -l`, as the "before" (expected: the dark set from the planning session and a
  light set taken the same way).
- Whether the shadcn stylesheet import and the six unused card exports are referenced
  (expected: nothing in `src` uses either).
- The compiled radius scale (expected: bare `rounded` is 4px, `rounded-md` 8, `rounded-xl` 14).

The findings go in the notes; nothing is fixed until they are written down.

## Phase 1 — Tokens and primitives

`src/index.css` states the palette (paper and ink, `--surface`, `--wash-strength`, the four
class colours) and eight type roles as Tailwind text utilities; a base rule derives
`--accent-ink` and `--wash` from any container that sets `--accent` inline. `src/lib/styles.ts`
grows into the primitive set — filled, text, chip and icon buttons in class-scoped and neutral
variants, chips, `meta`, `errorLine`, `pulseDot`, `statusLine`, rows, the decision card, the
wash card, inputs, option rows — and `src/components/SectionHeading.tsx` is the one section
heading. The formatters in `src/lib` produce sentence-case words; Rust identifiers get label
tables in TypeScript. The frontend-design skill is read before this phase.

## Phase 2 — Dashboard

The day as the headline, the schedule grid with washed slabs, the due strip as washed chips,
and the class card as its wash: name in the class ink, the meeting line, the current division
as `Week 3` over its topic, the nearest deadline as a sentence, chips along the bottom.

## Phase 3 — Workspace

A full-width band in the class wash — back link, class name, current division, one meta row —
then the sticky row of section links in the page's own order (Inbox · Notices · Structure ·
Deadlines · Grades · Materials · Lectures · Practice exams · Notes), each present only while
its section is; the inbox and the notices export their own predicate and query so the nav
reads the same truth. Every section takes the heading, the rows and the cards.

## Phase 4 — Overlays and rooms

The jobs pill and panel, the chat sidebar and its settings pane (the answer's prose register
retuned in place), the guide, material and note rooms, the Add lecture dialog, Settings — and
`src/lib/document.ts`'s `docShell`, which takes the app's paper and ink so a note previews on
the page it will be read on.

## Phase 5 — Copy and documents

Every label to sentence case with a plain verb; the driver names in `CLAUDE.md`, the label
names `SPEC.md` cites, and §12 state the design; the notes get the measurements and the
gotchas, and a "Design system" subsection that supersedes M1's where they differ.

## Acceptance

- `cargo test`: unchanged, since nothing under `src-tauri/` changes; `npx tsc --noEmit` clean;
  `grep -rn 'tracking-\[\|\buppercase\b' src` empty, `toUpperCase` only in `utils.ts`
  (`sentence`) and `guides.ts` (the guide footer's stamp, which is the guides' own design).
- Live, on the dev build beside the installed app: every surface in the new language in light
  and dark mode at the current window and at the 920×600 minimum, with no clipped,
  overlapping or uppercase-tracked chrome; the note preview and chat answers on the app's own
  paper and ink; the section links reaching every section present and absent with their
  sections; every control the docs name found by the driver under its new name.
- `/Applications/ClassHub.app` runs the facelift commit, and `git switch main && npm run
  install-app` restores the original — the decision to keep is a merge, made after living
  with it.

## Watch for

- **The budget.** Nothing on the subscription, nothing on credits: never Sync Canvas, Scan the
  syllabus, Write guide, Practice exam, Send in chat, or Add lecture with the session document
  on; a dev build's launch scan enqueues extracts only for stale files, and a day-old sync runs
  on launch with a stored session — free, but it stages files and can queue a sort.
- **The driver matches visible text.** Every renamed button is a renamed AX element; section
  order stays exactly as it is, because `ax dump` order decides `AX_NTH`.
- **Two prose registers live outside Tailwind**: the document shell and the chat's prose
  selector list. Both move with the tokens or the preview disagrees with the chrome.
- **The drag strip** is `fixed top-0 h-9 z-10`; the sticky nav sits under it with 36px of its
  own padding, and the rooms keep `pl-24` for the traffic lights.
- **The installed app (84454ad) runs beside the dev build** on one database; nothing here
  writes to it.
- **The serif was tried and declined**: a first pass set headlines in New York and read as
  Times; the brief's own words — clean and modern — decide, so the hierarchy is weight and
  tracking in one sans.
