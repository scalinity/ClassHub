# M13 — Canvas as ground truth

**Read `SPEC.md` in full first**, then this file. §1 (Canvas facts and the course-structure
table), §5 (`units`), §7.2 (Canvas sync) and §10 (the confirm queue) are the parts this
milestone implements. Everything needed is in those two files — this milestone was designed in
a session whose context is not worth reconstructing.

## Why

ClassHub currently learns a course's structure from whatever folders exist in the AIBHS tree.
Those folders are hand-made, which makes the human the liaison between what a course actually
is and what the app believes it is — and every hop through a human loses something. Canvas
already holds the truth: the module list, the files, the assignments with real due dates.

Reading the syllabi settles that this cannot be guessed. The four courses genuinely disagree
about their own shape — 14 weekly topics, 15 weekly topics, 3 Parts over 16 weeks, and one with
no published structure at all (SPEC §1). Any structure the app imposes is wrong for at least
two of them. So the app must read the course's own.

## Phase 0 — Prove the one available path

Authentication is already settled, so this is a narrow spike rather than a decision (SPEC §1):

- **Personal access token — closed.** Canvas answers *"Your Canvas administrators have chosen
  to limit your ability to generate your own access token."* Verified 2026-08-26. Do not build
  a token field and do not ask for one.
- **OAuth2 — closed, and not a fallback.** Instructure's own docs: *"For Canvas Cloud (hosted
  by Instructure), developer keys are issued by the admin of the institution."* Without an
  enabled key Canvas returns `unauthorized_client`. The `client_credentials` variant needs an
  LTI developer key — also admin. Do not spend time on the OAuth2 flow.
- **`POST /api/v1/users/:id/tokens` — do not use.** It exists, and a working session might even
  reach it, but it is behind the same permission the administrators disabled. Reading data the
  account can already read is automating the user's own browsing; minting a durable credential
  through that session is circumventing a control someone deliberately set. That line is not
  negotiable and is not an implementation detail to revisit under time pressure.
- **The `Canvas for iOS` tokens in the account are not a loophole.** Instructure's mobile apps
  run on Instructure's own globally-enabled developer keys. Regenerating one would break the
  real iOS app and would file this app's traffic under Instructure's key in UF's audit logs.

That leaves **session reads from inside a signed-in Canvas page**. Prove it end to end before
building anything on it:

1. Open `https://ufl.instructure.com` in a Tauri `WebviewWindow`. `zoom.rs::open_window` is the
   working precedent for the whole shape.
2. Let the user complete UF SSO in that window (Duo included). Poll for arrival the way
   `zoom.rs` polls its probe.
3. Run, via `eval_with_callback`:
   `fetch('/api/v1/users/self', {credentials:'same-origin'}).then(r => r.json())`

   **In-page is the requirement, not a preference.** Canvas honours the session cookie for
   same-origin GETs only. Reading cookies out with `cookies_for_url` and replaying them from
   `reqwest` is the wrong shape and invites a referer or CSRF refusal — and it is not needed,
   since only reads are ever performed.
4. Confirm a real user object comes back, then confirm `/api/v1/courses?enrollment_state=active`
   returns the four enrolled classes.

Only then continue. If the session path fails, **stop and report** — do not scrape Canvas's
HTML as a substitute. Record the finding in SPEC §1 and fall back to the syllabus source in
Phase 1, which is enough for M14 to proceed.

GETs need no `X-CSRF-Token`; the app never writes to Canvas, so that dance never arises.

**Use REST, not GraphQL.** Canvas exposes GraphQL at `POST /api/graphql` with permissions
mirroring REST, and it would collapse a sync into one round trip — but a POST under session auth
needs the `X-CSRF-Token` header pulled from the URL-decoded `_csrf_token` cookie. The rate limit
is 700 requests / 10 minutes and a full sync is nowhere near it, so REST GETs buy simplicity for
free. Revisit only if request counts ever actually justify the extra moving part.

## Phase 1 — `units` from the best available source

Migration `0007_units.sql` creates `units` and `lecture_contributions` exactly as SPEC §5
states. (`lecture_contributions` is M14's, but both tables land in one migration so M14 needs
no schema work.)

Populate `units` with precedence **canvas > syllabus > folder**:

- **Canvas** — `/courses/:id/modules?include[]=items`. `kind` from the module's own naming
  (`Week 7 …` → `week`, `Module 3` → `module`, `Part II` → `part`); `name` is the course's
  own string, never normalized. Keep `canvas_id`.
- **Syllabus** — extend the existing `syllabus_scan` job rather than adding a kind. It already
  reads the syllabus for deadlines; the weekly schedule is in the same document and the same
  read. Its output contract grows a `units` array beside `deadlines`. Fundamentals and
  Biostatistics both carry explicit dates per week, so `starts_on` is available for them;
  Applied Generative AI numbers its weeks without dates and declares 3 Parts over them.
- **Folder** — top-level folders, which is what the app does today. `source='folder'` marks
  them so a hand-made folder is never mistaken for a course declaration.

Matching a Canvas course to a `classes` row: match on `display_name`/`code` and confirm rather
than guess. A wrong mapping files another class's material into this one.

## Phase 2 — Course files

`/courses/:id/files` (and module items of type `File`). Download into the tree, routed through
the §10 confirm queue like every other move — approval is what places a file, here as
everywhere. Skip anything already present by size + name; a re-sync must not duplicate.

Attribute each file to its unit where Canvas says so (a module item knows its module), so §8.5
guides can use it without a mapping pass.

## Phase 3 — Assignments → deadlines

`/courses/:id/assignments` gives titles and true due dates, which is strictly better than
parsing them out of a syllabus PDF. Insert through the existing `deadline_proposals` confirm
queue with `source='canvas'`. Reconcile against existing rows so a re-sync updates rather than
duplicates — match on title + due date.

## Phase 4 — UI

A Canvas section in Settings: last sync time and a **Sync now** button that opens the sign-in
window when there is no live session. Per-class sync from the Class Workspace. Sync is manual,
never on a timer (SPEC §7.2).

There is no credential to manage, so the UI has no key field and no "connected" state to
persist — a Canvas session simply expires, and the honest presentation is "sign in to sync"
rather than a connection that appears healthy until it silently isn't. Expect to hit this: a
sync started hours after the last one will find the session gone, so re-opening the window has
to be an ordinary part of the flow rather than an error path.

Show `units` in the Class Workspace with the course's own names, and mark units whose `source`
is `folder` so it is obvious which structure is real and which is inferred.

Read the frontend-design skill first (SPEC §12).

## Acceptance

- Signing in through the Canvas window makes `/api/v1/users/self` return the real user, and
  `/api/v1/courses?enrollment_state=active` the four enrolled classes — with no credential
  stored anywhere.
- A sync attempted after the session has expired re-opens the sign-in window instead of failing.
- All four classes' real divisions land in `units` from Canvas with nothing typed by hand:
  14 weekly topics for Fundamentals, 15 for Biostatistics, 3 Parts for Applied Generative AI.
  Design Studio takes whatever Canvas has, which may be nothing.
- A Canvas assignment appears as a deadline with its true due date, through the confirm queue.
- A course file downloads into the tree via an approved move and is indexed by the scanner.
- Re-syncing changes nothing: no duplicate units, files, or deadlines.
- A unit that disappears from Canvas is retained, not deleted (SPEC §7.2).
- `cargo test` and `npx tsc --noEmit` pass.

## Watch for

- **Do not delete on sync.** A mid-semester Canvas reshuffle must not orphan a guide.
- **Reads only.** ClassHub never writes to Canvas — that is what keeps the session approach a
  narrower exposure than the token the administrators disabled, and it is not negotiable.
- **Store no credential.** Nothing goes in the Keychain and nothing in the settings table; the
  reach expires with the session, which is the point.
- **Rate limit** 700 requests / 10 minutes. A full sync is far inside it; a retry loop is not.
- **Pagination.** Canvas paginates via the `Link` header. A class with 50 files returns 10 by
  default, and missing the rest looks exactly like success.
- **Session auth is undocumented** (SPEC §1). If a Canvas change breaks it, the answer is the
  syllabus fallback, not a workaround that digs deeper into Canvas's internals.
