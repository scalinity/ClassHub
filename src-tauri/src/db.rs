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
/// `guides.scope` prefix for a guide over one of the course's own divisions
/// (SPEC §8.1), followed by the unit's name. A prefix rather than a bare name
/// because every other scope value is a folder rel path, and a unit need not be
/// a folder — `UNIQUE(class_id, name)` on `units` is what makes the name enough
/// to identify one.
pub const UNIT_SCOPE_PREFIX: &str = "unit:";

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
/// here first — recoverability in place of confirmation prompts.
pub fn audit(conn: &Connection, action: &str, payload: serde_json::Value) -> Result<()> {
    conn.execute(
        "INSERT INTO audit_log (action, payload, created_at) VALUES (?1, ?2, ?3)",
        rusqlite::params![action, payload.to_string(), now()],
    )?;
    Ok(())
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
];

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
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrentUnit {
    pub name: String,
    /// module | week | part — the course's word for it.
    pub kind: String,
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
        // ISO text sorts chronologically, so MIN(due_at) is the nearest.
        let next_deadline = conn
            .query_row(
                "SELECT title, due_at FROM deadlines
                 WHERE class_id = ?1 AND status = 'open'
                 ORDER BY due_at LIMIT 1",
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
            name: unit.name,
            kind: unit.kind,
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
        assert_eq!(now.kind, "week");
        assert!(by_id(4).current_unit.is_none(), "no dates, no answer");
        assert!(by_id(1).current_unit.is_none(), "no divisions, no answer");
    }
}
