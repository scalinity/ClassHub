use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

// ---------------------------------------------------------------------------
// Shared infrastructure — the single definitions every module imports.

/// SPEC §4 filesystem contract. One definition each, because these names are
/// the layout: a module that keeps its own copy is a module that can drift out
/// of agreement with the scanner about what a class folder contains.
pub const INBOX_DIR: &str = "_Inbox";
pub const NOTES_DIR: &str = "Notes";
pub const GUIDES_DIR: &str = "Study Guides";
pub const PRACTICE_DIR: &str = "Study Guides/Practice";
/// Where a lecture's distilled session document lands. Under `Study Guides/`
/// deliberately: that is already contracted-writable for jobs and already in
/// chat's search scope, so a session digest needs neither widened.
pub const SESSIONS_DIR: &str = "Study Guides/Sessions";
pub const EXTRACTS_DIR: &str = ".classhub/extracts";
/// SPEC §8.5 — the distilled lecture notes a unit guide is actually built from,
/// keyed by unit. App-managed and hidden, like the extract cache beside it, and
/// in `search_material`'s scope so chat retrieves it.
pub const CORPUS_DIR: &str = ".classhub/corpus";
/// SPEC §4 — where a lecture lives: `Weeks/Week NN — <topic>/`. Ordinary
/// *source* material, not app-managed: the scanner indexes it, extraction
/// routes it through the zero-token text path, and chat searches it. Storing it
/// by date is also what settles its unit (SPEC §8.5), since no course here
/// divides itself finer than its meetings.
pub const WEEKS_DIR: &str = "Weeks";
pub const MASTER_SCOPE: &str = "master";
/// `guides.scope` prefix for a session digest, followed by the transcript's
/// class-relative path. Naming the source file rather than the date makes the
/// scope unique per transcript, so `UNIQUE(class_id, scope)` turns a re-run
/// into an update, and staleness has something real to hash.
pub const SESSION_SCOPE_PREFIX: &str = "session:";
/// `guides.scope` and `jobs.scope` prefix for one of the course's own divisions
/// (SPEC §8.1), followed by the unit's row id: `unit:24`. A prefix rather than
/// a bare value because every other scope is a folder rel path, and the id
/// rather than the name because a rescan may rename a division (SPEC §7.2) and
/// a scope keyed on the name would strand its guide.
pub const UNIT_SCOPE_PREFIX: &str = "unit:";

/// SPEC §8.3: a practice exam's row is scoped by its file — `practice:Study
/// Guides/Practice/<name>.html` — so several exams of one scope each keep a
/// row, and its staleness reads off its own manifest alone.
pub const PRACTICE_SCOPE_PREFIX: &str = "practice:";

pub fn is_practice_scope(scope: &str) -> bool {
    scope.starts_with(PRACTICE_SCOPE_PREFIX)
}

/// The scope of one of the course's divisions.
pub fn unit_scope(unit_id: i64) -> String {
    format!("{UNIT_SCOPE_PREFIX}{unit_id}")
}

/// The unit id a scope names, if it is a unit scope — `unit:24` → 24. A unit
/// scope carrying anything but a number is one from before ids that no
/// migration could resolve, and names no row.
pub fn unit_scope_id(scope: &str) -> Option<i64> {
    scope.strip_prefix(UNIT_SCOPE_PREFIX)?.parse().ok()
}

/// Session digests live in the `guides` table so they inherit the viewer and
/// staleness, but they are not module guides — the Study Guides list tells them
/// apart by this. Here rather than in `lectures.rs` so `guides.rs` needs no
/// dependency on the lecture module to ask.
pub fn is_session_scope(scope: &str) -> bool {
    scope.starts_with(SESSION_SCOPE_PREFIX)
}

/// The only paths a synthesis job is contracted to write. Everything else in a
/// class folder is source material, and SPEC §4 says the app never destroys it.
pub const JOB_WRITABLE: &[&str] = &[GUIDES_DIR, EXTRACTS_DIR, CORPUS_DIR];

/// Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// A poisoned lock means another thread panicked mid-write, not that this
/// connection is unusable — recovering the guard keeps one panic from taking
/// the rest of the session down with it. Every lock site uses this, so the
/// policy is uniform rather than split between readers and writers.
pub fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Runs `f` with the shared connection guard held for exactly that window.
/// The mutex is NOT reentrant: never reach anything that takes it again
/// (e.g. `jobs::enqueue`) from inside `f`.
pub fn with_conn<T>(app: &AppHandle, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db = app.state::<crate::Db>();
    let guard = lock(&db.0);
    f(&guard)
}

/// Tells the frontend that hub data changed (src/lib/query.ts maps areas to
/// query invalidations — the whole "no manual refresh" mechanism).
pub fn emit_hub_change(app: &AppHandle, area: &str) {
    let _ = app.emit("hub-changed", json!({ "area": area }));
}

/// Char-safe display truncation with an ellipsis — job summaries, tool-chip
/// labels, and the deadline field caps all share it.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

/// Writes through a sibling temp file and renames over the target. `fs::write`
/// truncates in place, so a crash mid-write leaves a half-written note or a
/// truncated extract; rename is atomic on the same volume, which this always
/// is since the temp file sits next to the target.
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}tmp",
        path.extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    std::fs::write(&tmp, contents)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| {
        let _ = std::fs::remove_file(&tmp);
        format!("replacing {}", path.display())
    })?;
    Ok(())
}

/// One append-only history row. Destructive writes park their prior state
/// here first — recoverability in place of confirmation prompts. Returns
/// the row's id, which a notice carries so `Undo` can name the row it
/// reverses (SPEC §6).
pub fn audit(conn: &Connection, action: &str, payload: serde_json::Value) -> Result<i64> {
    conn.execute(
        "INSERT INTO audit_log (action, payload, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![action, payload.to_string(), now()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// What a notice says and which audit rows its `Undo` reverses (SPEC §12).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Notice {
    pub text: String,
    /// The rows `undo_audit` reverses, in the order they were written; empty
    /// for a notice with nothing to undo.
    pub audit_ids: Vec<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_id: Option<i64>,
}

/// Tells the frontend what a reversible action did, with the audit rows
/// behind it, so the notice at the bottom of the window can offer `Undo`.
/// Emitted by the write itself — a command, a chat tool, the sync — so every
/// surface reaches the same notice.
pub fn notify(app: &AppHandle, text: impl Into<String>, audit_ids: Vec<i64>, class_id: Option<i64>) {
    let _ = app.emit(
        "notice",
        Notice {
            text: text.into(),
            audit_ids,
            class_id,
        },
    );
}

/// One value from the SPEC §5 settings table (chat.rs and settings.rs share
/// this — the API key itself never lives here, only in the Keychain).
pub fn setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_job_payload.sql"),
    include_str!("../migrations/0003_move_proposals.sql"),
    include_str!("../migrations/0004_deadline_proposals.sql"),
    include_str!("../migrations/0005_grade_checks.sql"),
    include_str!("../migrations/0006_indexes.sql"),
    include_str!("../migrations/0007_units.sql"),
    include_str!("../migrations/0008_folder_units.sql"),
    include_str!("../migrations/0009_job_owner.sql"),
    include_str!("../migrations/0010_canvas_grades.sql"),
    include_str!("../migrations/0011_announcements.sql"),
    UNIT_LABELS_MIGRATION,
    CONTRIBUTION_NOTES_MIGRATION,
    include_str!("../migrations/0014_lecture_hints.sql"),
    include_str!("../migrations/0015_hints_read_at.sql"),
    include_str!("../migrations/0016_duplicate_files.sql"),
    include_str!("../migrations/0017_shift_runs.sql"),
    include_str!("../migrations/0018_one_run_a_night.sql"),
];

/// The migration that makes one note per transcript name in a division
/// structural (SPEC §8.5): a unique index the Rust-side refusal can only
/// promise for its own process.
const CONTRIBUTION_NOTES_MIGRATION: &str =
    include_str!("../migrations/0013_contribution_notes.sql");

/// The migration that adds the label columns; `units::backfill_labels` runs
/// inside its transaction, so the rows that predate them get theirs from
/// their names. Keyed on the migration itself rather than on its position,
/// so a migration slotted in ahead of it cannot move the backfill onto the
/// wrong one.
const UNIT_LABELS_MIGRATION: &str = include_str!("../migrations/0012_unit_identity.sql");

/// An in-memory database with every migration applied.
///
/// Tests that touch storage run against the real schema rather than a
/// hand-copied subset of it, so a migration and the code that reads it cannot
/// drift apart unnoticed.
///
/// **The four classes are already here** — `0001_init.sql` seeds them, ids 1–4.
/// A fixture that inserts its own collides on `classes.id`; use one of theirs.
#[cfg(test)]
pub(crate) fn memory_db() -> Connection {
    let conn = Connection::open_in_memory().expect("opening an in-memory database");
    for migration in MIGRATIONS {
        conn.execute_batch(migration).expect("applying a migration");
    }
    conn
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meeting {
    pub weekday: u8,
    pub start_time: String,
    pub end_time: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineChip {
    pub title: String,
    /// ISO as stored: YYYY-MM-DD, optionally with THH:MM[:SS].
    pub due_at: String,
}

/// The division a course is in today (SPEC §8.5), in the course's own words.
/// The id is what the Structure list matches its row on.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentUnit {
    pub id: i64,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClassCard {
    pub id: i64,
    pub display_name: String,
    pub color: String,
    pub room: String,
    pub instructors: String,
    pub credits: i64,
    pub folder_name: String,
    pub folder_present: bool,
    /// Card-level staleness badge (SPEC §12), computed on demand.
    pub stale_guides: i64,
    /// Drop-to-sort badge (SPEC §10 step 5): pending proposals plus inbox
    /// files nothing has proposed for yet.
    pub inbox_pending: i64,
    /// Proposed deadlines (SPEC §11) still waiting on a decision, from either
    /// reader. Counted on the card because the queue lives inside the
    /// workspace, and a proposal nothing on the dashboard mentions is one that
    /// waits until something else brings the reader there.
    pub pending_deadline_proposals: i64,
    /// Nearest open deadline (SPEC §12 card contents), overdue included —
    /// an open deadline in the past is the most urgent line on the card.
    pub next_deadline: Option<DeadlineChip>,
    /// SPEC §11: current weighted grade over graded items (grades.rs math).
    pub current_grade: Option<f64>,
    /// ISO start of the final exam, when scheduled — the dashboard's
    /// countdown chips (SPEC §11).
    pub final_exam_start: Option<String>,
    /// Where the course is today, resolved from `units` and never from
    /// arithmetic (SPEC §8.5) — so a course that published no dates has none,
    /// and the card says nothing rather than guessing.
    pub current_unit: Option<CurrentUnit>,
    pub meetings: Vec<Meeting>,
}

pub fn open(db_path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Two processes share this file (SPEC §13), and writers still take turns
    // under WAL: the second one waits for the first's commit only because a
    // busy timeout says so. rusqlite installs five seconds on every connection;
    // stating a longer one here makes the reliance explicit rather than a
    // library default, and covers two launches migrating and scanning at once.
    conn.busy_timeout(Duration::from_secs(10))?;
    // WAL, so the installed app and a dev build can hold this database open at
    // once: a reader no longer blocks the writer. The mode persists in the file,
    // so a build that never sets it still runs under it. The pragma answers
    // with the mode actually in force — the old one, when the switch could not
    // be made — and a database quietly left in rollback mode would look exactly
    // like a healthy one, so the answer is checked rather than discarded.
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        bail!("the database is in {mode} journal mode, not WAL — two processes cannot share it");
    }
    run_migrations(&mut conn)?;
    seed_default_settings(&conn)?;
    Ok(conn)
}

fn run_migrations(conn: &mut Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)
            .with_context(|| format!("running migration {:04}", index + 1))?;
        if *sql == UNIT_LABELS_MIGRATION {
            crate::units::backfill_labels(&tx).context("backfilling unit labels")?;
        }
        tx.pragma_update(None, "user_version", (index + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

fn seed_default_settings(conn: &Connection) -> Result<()> {
    let home = dirs::home_dir().context("resolving home directory")?;
    let default_root = home.join("Documents").join("AIBHS");
    conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('aibhs_root', ?1)",
        [default_root.to_string_lossy()],
    )?;
    Ok(())
}

pub fn aibhs_root(conn: &Connection) -> Result<PathBuf> {
    let root: String = conn.query_row(
        "SELECT value FROM settings WHERE key = 'aibhs_root'",
        [],
        |row| row.get(0),
    )?;
    Ok(PathBuf::from(root))
}

/// `today` is YYYY-MM-DD from the client's clock — the same clock the card's
/// meeting and deadline labels are measured against.
pub fn list_classes(conn: &Connection, today: &str) -> Result<Vec<ClassCard>> {
    let root = aibhs_root(conn)?;

    let mut class_stmt = conn.prepare(
        "SELECT id, display_name, color, room, instructors, credits, folder_name,
                final_exam_start
         FROM classes ORDER BY id",
    )?;
    let mut meeting_stmt = conn.prepare(
        "SELECT weekday, start_time, end_time FROM meetings
         WHERE class_id = ?1 ORDER BY weekday, start_time",
    )?;

    let classes = class_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut cards = Vec::with_capacity(classes.len());
    for (id, display_name, color, room, instructors, credits, folder_name, final_exam_start) in
        classes
    {
        let meetings = meeting_stmt
            .query_map([id], |row| {
                Ok(Meeting {
                    weekday: row.get(0)?,
                    start_time: row.get(1)?,
                    end_time: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let folder_present = root.join(&folder_name).is_dir();
        let stale_guides = crate::guides::stale_guide_count(conn, id)?;
        let inbox_pending = crate::sorter::pending_count(conn, id)?;
        let pending_deadline_proposals = crate::deadlines::pending_count(conn, id)?;
        // Ordered by the instant, not the text: a date-only row is the end of
        // its day (SPEC §11), so it follows a timed row on the same day.
        let next_deadline = conn
            .query_row(
                &format!(
                    "SELECT title, due_at FROM deadlines
                     WHERE class_id = ?1 AND status = 'open'
                     ORDER BY {} LIMIT 1",
                    crate::deadlines::DUE_INSTANT_SQL
                ),
                [id],
                |row| {
                    Ok(DeadlineChip {
                        title: row.get(0)?,
                        due_at: row.get(1)?,
                    })
                },
            )
            .optional()?;
        let current_grade = crate::grades::weighted_grade(conn, id)?;
        let current_unit = crate::units::current_unit(conn, id, today)?.map(|unit| CurrentUnit {
            id: unit.id,
            name: unit.name,
        });
        cards.push(ClassCard {
            id,
            display_name,
            color,
            room,
            instructors,
            credits,
            folder_name,
            folder_present,
            stale_guides,
            inbox_pending,
            pending_deadline_proposals,
            next_deadline,
            current_grade,
            final_exam_start,
            current_unit,
            meetings,
        });
    }
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The card carries the division the course is in today, in the course's
    /// own words, and nothing for a course that published no dates.
    #[test]
    fn a_card_names_the_current_division() {
        let conn = memory_db();
        let root = std::env::temp_dir().join(format!("classhub-cards-{}", std::process::id()));
        set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        for (class_id, ordinal, kind, name, starts_on) in [
            (3, 14, "week", "Week 14 \u{2014} Project preparation", Some("2026-11-19")),
            (3, 15, "week", "Week 15 \u{2014} No Class (Thanksgiving Week)", Some("2026-11-26")),
            (4, 1, "part", "Part I: Deep Learning (Weeks 1-8)", None),
        ] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, starts_on, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'syllabus')",
                rusqlite::params![class_id, ordinal, kind, name, starts_on],
            )
            .expect("unit");
        }

        let cards = list_classes(&conn, "2026-11-26").expect("cards");
        let by_id = |id: i64| cards.iter().find(|c| c.id == id).expect("seeded class");
        let now = by_id(3).current_unit.as_ref().expect("Biostatistics is dated");
        assert_eq!(now.name, "Week 15 \u{2014} No Class (Thanksgiving Week)");
        let row_id: i64 = conn
            .query_row("SELECT id FROM units WHERE name = ?1", [&now.name], |row| row.get(0))
            .expect("the row");
        assert_eq!(now.id, row_id);
        assert!(by_id(4).current_unit.is_none(), "no dates, no answer");
        assert!(by_id(1).current_unit.is_none(), "no divisions, no answer");
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    /// Migration 0013 against rows from before it: two contributions naming
    /// one note are reduced to the earliest, whose note the rule protects, a
    /// row with a note of its own is kept, and a second row on one note is
    /// refused from then on.
    #[test]
    fn migration_0013_keeps_one_row_per_note() {
        let mut conn = Connection::open_in_memory().expect("open");
        let at = MIGRATIONS
            .iter()
            .position(|m| *m == CONTRIBUTION_NOTES_MIGRATION)
            .expect("the notes migration is listed");
        for sql in &MIGRATIONS[..at] {
            conn.execute_batch(sql).expect("migration");
        }
        conn.pragma_update(None, "user_version", at as i64).expect("version");
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, first_week, last_week, source)
             VALUES (37, 4, 1, 'part', 'Part I: Foundations', 1, 8, 'syllabus')",
            [],
        )
        .expect("unit");
        let note = ".classhub/corpus/Part I- Foundations/2026-09-01 — Lecture.md";
        let insert = |conn: &Connection, rel_path: &str, corpus: &str| {
            conn.execute(
                "INSERT INTO lecture_contributions
                 (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
                  corpus_rel_path, summary, confidence, status, created_at)
                 VALUES (4, 37, ?1, 0, 1, 1, 1, ?2, '', 'high', 'applied', 1)",
                rusqlite::params![rel_path, corpus],
            )
        };
        insert(&conn, "Weeks/Week 02/2026-09-01 — Lecture.md", note).expect("first");
        insert(&conn, "Weeks/Week 03/2026-09-01 — Lecture.md", note).expect("the collision");
        insert(
            &conn,
            "Weeks/Week 03/2026-09-01 — Guest lecture.md",
            ".classhub/corpus/Part I- Foundations/2026-09-01 — Guest lecture.md",
        )
        .expect("its own note");

        run_migrations(&mut conn).expect("migrate");

        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).expect("version");
        assert_eq!(version as usize, MIGRATIONS.len());
        let mut stmt = conn
            .prepare("SELECT rel_path FROM lecture_contributions ORDER BY id")
            .expect("prepare");
        let kept = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        assert_eq!(
            kept,
            ["Weeks/Week 02/2026-09-01 — Lecture.md", "Weeks/Week 03/2026-09-01 — Guest lecture.md"]
        );
        assert!(
            insert(&conn, "Weeks/Week 04/2026-09-01 — Lecture.md", note).is_err(),
            "a second row on one note was accepted"
        );
    }

    /// Migration 0012 against rows from before it: a guide and a job scoped
    /// `unit:<name>` are rescoped to the row's id, one naming a division no
    /// longer in the table keeps what it has, a folder scope is untouched,
    /// and every row's label and range are filled in the same transaction.
    #[test]
    fn migration_0012_rescopes_old_style_rows() {
        let mut conn = Connection::open_in_memory().expect("open");
        let at = MIGRATIONS
            .iter()
            .position(|m| *m == UNIT_LABELS_MIGRATION)
            .expect("the label migration is listed");
        for sql in &MIGRATIONS[..at] {
            conn.execute_batch(sql).expect("migration");
        }
        conn.pragma_update(None, "user_version", at as i64).expect("version");
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, source) VALUES
             (24, 1, 2, 'week', 'Week 2 — Responsible AI', 'syllabus'),
             (37, 4, 1, 'part', 'Part I: Foundations (Weeks 1-8)', 'syllabus')",
            [],
        )
        .expect("units");
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest) VALUES
             (1, 'unit:Week 2 — Responsible AI', 'Study Guides/Week 2 — Responsible AI.html', 1, '[]'),
             (1, 'unit:Week 9', 'Study Guides/Week 9.html', 1, '[]'),
             (3, 'Module 1', 'Study Guides/Module 1.html', 1, '[]')",
            [],
        )
        .expect("guides");
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, created_at, owner_pid) VALUES
             ('module_guide', 1, 'unit:Week 2 — Responsible AI', 'succeeded', 1, 1),
             ('practice', 4, 'unit:Part I: Foundations (Weeks 1-8)', 'succeeded', 1, 1),
             ('module_guide', 1, 'unit:Week 9', 'failed', 1, 1)",
            [],
        )
        .expect("jobs");

        run_migrations(&mut conn).expect("migrate");

        let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).expect("version");
        assert_eq!(version as usize, MIGRATIONS.len());
        let scopes = |table: &str| -> Vec<String> {
            let mut stmt = conn.prepare(&format!("SELECT scope FROM {table} ORDER BY id")).expect("prepare");
            let rows = stmt.query_map([], |row| row.get(0)).expect("query");
            rows.collect::<rusqlite::Result<Vec<String>>>().expect("rows")
        };
        assert_eq!(scopes("guides"), ["unit:24", "unit:Week 9", "Module 1"]);
        assert_eq!(scopes("jobs"), ["unit:24", "unit:37", "unit:Week 9"]);
        let mut stmt = conn
            .prepare("SELECT id, number, first_week, last_week FROM units ORDER BY id")
            .expect("prepare");
        let labels = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows");
        assert_eq!(labels, [(24, Some(2), None, None), (37, Some(1), Some(1), Some(8))]);
    }
}
