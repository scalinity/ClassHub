use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_job_payload.sql"),
    include_str!("../migrations/0003_move_proposals.sql"),
];

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
    /// Nearest open deadline (SPEC §12 card contents), overdue included —
    /// an open deadline in the past is the most urgent line on the card.
    pub next_deadline: Option<DeadlineChip>,
    pub meetings: Vec<Meeting>,
}

pub fn open(db_path: &Path) -> Result<Connection> {
    let mut conn = Connection::open(db_path)
        .with_context(|| format!("opening database at {}", db_path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
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

pub fn list_classes(conn: &Connection) -> Result<Vec<ClassCard>> {
    let root = aibhs_root(conn)?;

    let mut class_stmt = conn.prepare(
        "SELECT id, display_name, color, room, instructors, credits, folder_name
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
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut cards = Vec::with_capacity(classes.len());
    for (id, display_name, color, room, instructors, credits, folder_name) in classes {
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
            next_deadline,
            meetings,
        });
    }
    Ok(cards)
}
