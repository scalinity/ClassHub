# M22 — What the professor said

**Read `SPEC.md` in full first**, then this file. §7.2 (Canvas sync), §9 (chat context) and
§11 (syllabus extraction) are what this milestone extends.

## Why

Canvas carries three things the app never reads: announcements ("quiz moved to Thursday",
"slides posted"), Pages, where a course with empty Modules often keeps its weekly content, and
the syllabus page itself — Applied Generative AI's PDF carries no dates, and its Canvas syllabus
page may. Today the only way to learn any of it is to open Canvas. And the app never says when
it last synced: on 2026-09-02 the answer was six days, and nothing on screen said so.

## Phase 0 — Probe

In a signed-in session, for each of the four courses, record what comes back from
`/announcements?context_codes[]=course_<id>`, `/courses/:id/pages` and
`/courses/:id?include[]=syllabus_body`. Record it in SPEC §1 the way the modules finding was
recorded. Build only what has content.

## Phase 1 — Announcements

Table `announcements(id, class_id, canvas_id UNIQUE, title, body, posted_at)`; the body is
Canvas's HTML stripped through the extractor's stripper. Read on every sync. A `NOTICES`
section in the workspace, newest first, hidden when empty; the three most recent per class in
the chat overview's detailed form, titles only in the compact form. No unread state — the
section is a record, not a queue.

## Phase 2 — Pages and the syllabus body

Written as markdown into `.classhub/extracts/Canvas/<Page title>.md` and
`.classhub/extracts/Canvas/Syllabus.md`, so `search_material` covers them without a new root.
They are not `files` rows: nothing on disk is their source, so they take no part in guide
manifests. The syllabus scan's picker offers the Canvas syllabus as a source when it exists.

## Phase 3 — Sync age

A mono line on the dashboard header, `CANVAS · SYNCED 6 DAYS AGO`, in the destructive colour
past a week. And a decision to make here: a sync on launch when a stored session exists and
the last sync is older than a day — a hidden window, no sign-in prompt, a quiet note in the
Settings report if Canvas refused. SPEC §7.2 says sync runs only when asked; if this lands,
that sentence changes to say what is then true. It is not a timer, and nothing here introduces
a scheduler.

## Acceptance

- A test announcement posted in one course appears in its workspace after a sync and in the
  chat overview.
- A course's Pages are searchable in chat and cited by path.
- The Applied Generative AI syllabus scan can read the Canvas syllabus body, and proposes dates
  from it if any exist.
- The dashboard names the sync age; a launch a day after the last sync syncs on its own.
- Re-syncing is a no-op for all of it.

## Watch for

- **Announcement bodies embed images and links.** Strip to text; never render Canvas HTML in
  the app.
- **Rate limit 700 / 10 minutes.** Three more endpoints per course is far inside it; a
  per-page loop over a large Pages list is not — page it.
