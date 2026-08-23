//! SPEC §11 — deadlines: the UI's CRUD (per-class list + dashboard strip) and
//! the syllabus_scan flow (read-only job → proposals → confirm cards → insert
//! with source='syllabus').
//!
//! The UI writes mirror the chat tools exactly: same due_at/kind validation
//! (tools.rs), destructive writes park the prior state in `audit_log`, and
//! every successful write emits `hub-changed {area:"deadlines"}` so the
//! frontend refetches without a manual refresh. Nothing a syllabus scan
//! proposes becomes a deadline without explicit approval.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use crate::tools::{audit, valid_due_at, DEADLINE_KINDS};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/syllabus.md");

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn with_conn<T>(app: &AppHandle, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db = app.state::<crate::Db>();
    let guard = db.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&guard)
}

fn emit_hub_change(app: &AppHandle, area: &str) {
    let _ = app.emit("hub-changed", json!({ "area": area }));
}

// ---------------------------------------------------------------------------
// Deadline CRUD (UI side; the chat tools in tools.rs share the same rules)

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineInfo {
    pub id: i64,
    pub class_id: i64,
    pub class_name: String,
    pub class_color: String,
    pub title: String,
    pub kind: String,
    pub due_at: String,
    pub notes: Option<String>,
    /// open | done
    pub status: String,
    /// manual | agent | syllabus
    pub source: String,
}

/// Every deadline across every class, due-soonest first — the dashboard strip,
/// the class tab and the card line all filter this one payload client-side.
pub fn list_deadlines(conn: &Connection) -> Result<Vec<DeadlineInfo>> {
    let mut stmt = conn.prepare(
        "SELECT d.id, d.class_id, c.display_name, c.color, d.title, d.kind,
                d.due_at, d.notes, d.status, d.source
         FROM deadlines d JOIN classes c ON c.id = d.class_id
         ORDER BY d.due_at, d.id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(DeadlineInfo {
                id: row.get(0)?,
                class_id: row.get(1)?,
                class_name: row.get(2)?,
                class_color: row.get(3)?,
                title: row.get(4)?,
                kind: row.get(5)?,
                due_at: row.get(6)?,
                notes: row.get(7)?,
                status: row.get(8)?,
                source: row.get(9)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn validate_fields(title: &str, kind: &str, due_at: &str) -> Result<()> {
    if title.trim().is_empty() {
        bail!("the deadline needs a title");
    }
    if !DEADLINE_KINDS.contains(&kind) {
        bail!("kind must be one of: {}", DEADLINE_KINDS.join(", "));
    }
    if !valid_due_at(due_at) {
        bail!("due_at must be ISO — YYYY-MM-DD or YYYY-MM-DDTHH:MM, got '{due_at}'");
    }
    Ok(())
}

/// Create (id None, source='manual') or amend an existing row. Same validation
/// as the chat tool; amends keep the row's original source.
pub fn save_deadline(
    app: &AppHandle,
    class_id: i64,
    id: Option<i64>,
    title: &str,
    kind: &str,
    due_at: &str,
    notes: Option<&str>,
) -> Result<()> {
    let title = title.trim();
    validate_fields(title, kind, due_at)?;
    let notes = notes.map(str::trim).filter(|n| !n.is_empty());
    with_conn(app, |conn| match id {
        None => {
            conn.execute(
                "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'open', 'manual')",
                params![class_id, title, kind, due_at, notes],
            )?;
            audit(
                conn,
                "ui.upsert_deadline",
                json!({ "id": conn.last_insert_rowid(), "classId": class_id, "title": title,
                        "kind": kind, "dueAt": due_at, "notes": notes, "created": true }),
            )
        }
        Some(id) => {
            let before = conn
                .query_row(
                    "SELECT class_id, title, kind, due_at, notes FROM deadlines WHERE id = ?1",
                    [id],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()?
                .with_context(|| format!("no deadline #{id}"))?;
            if before.0 != class_id {
                bail!("deadline #{id} belongs to a different class");
            }
            conn.execute(
                "UPDATE deadlines SET title = ?1, kind = ?2, due_at = ?3, notes = ?4
                 WHERE id = ?5",
                params![title, kind, due_at, notes, id],
            )?;
            audit(
                conn,
                "ui.upsert_deadline",
                json!({ "id": id, "classId": class_id,
                        "before": { "title": before.1, "kind": before.2,
                                    "dueAt": before.3, "notes": before.4 },
                        "after": { "title": title, "kind": kind,
                                   "dueAt": due_at, "notes": notes } }),
            )
        }
    })?;
    emit_hub_change(app, "deadlines");
    Ok(())
}

/// done ↔ open toggle — the row's checkbox. Reopening is as cheap as closing.
pub fn set_deadline_status(app: &AppHandle, id: i64, done: bool) -> Result<()> {
    let status = if done { "done" } else { "open" };
    with_conn(app, |conn| {
        let changed = conn.execute(
            "UPDATE deadlines SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        if changed == 0 {
            bail!("no deadline #{id}");
        }
        audit(conn, "ui.set_deadline_status", json!({ "id": id, "status": status }))
    })?;
    emit_hub_change(app, "deadlines");
    Ok(())
}

/// The full row rides the audit entry, so a deletion is recoverable — that
/// stands in for a confirmation prompt.
pub fn delete_deadline(app: &AppHandle, id: i64) -> Result<()> {
    with_conn(app, |conn| {
        let row = conn
            .query_row(
                "SELECT class_id, title, kind, due_at, notes, status, source
                 FROM deadlines WHERE id = ?1",
                [id],
                |r| {
                    Ok(json!({
                        "id": id,
                        "classId": r.get::<_, i64>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "kind": r.get::<_, String>(2)?,
                        "dueAt": r.get::<_, String>(3)?,
                        "notes": r.get::<_, Option<String>>(4)?,
                        "status": r.get::<_, String>(5)?,
                        "source": r.get::<_, String>(6)?,
                    }))
                },
            )
            .optional()?
            .with_context(|| format!("no deadline #{id}"))?;
        conn.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        audit(conn, "ui.delete_deadline", row)
    })?;
    emit_hub_change(app, "deadlines");
    Ok(())
}

// ---------------------------------------------------------------------------
// Syllabus scan (SPEC §11): read-only job → proposals → confirm → insert

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineProposal {
    pub id: i64,
    pub class_id: i64,
    pub title: String,
    pub kind: String,
    pub due_at: String,
    pub notes: Option<String>,
    pub created_at: i64,
}

pub fn syllabus_proposals(conn: &Connection, class_id: i64) -> Result<Vec<DeadlineProposal>> {
    let mut stmt = conn.prepare(
        "SELECT id, class_id, title, kind, due_at, notes, created_at
         FROM deadline_proposals WHERE class_id = ?1 AND status = 'pending'
         ORDER BY due_at, id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(DeadlineProposal {
                id: row.get(0)?,
                class_id: row.get(1)?,
                title: row.get(2)?,
                kind: row.get(3)?,
                due_at: row.get(4)?,
                notes: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Enqueues a syllabus_scan over a chosen file (rel_path) or the whole class
/// folder (None). `today` is client-formatted — the prompt anchors relative
/// syllabus dates ("Sept 3") to the real semester.
pub fn run_scan(
    app: &AppHandle,
    class_id: i64,
    rel_path: Option<&str>,
    today: &str,
) -> Result<i64> {
    let prompt = with_conn(app, |conn| {
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM jobs
             WHERE kind = 'syllabus_scan' AND class_id = ?1
               AND status IN ('queued', 'running')",
            [class_id],
            |row| row.get(0),
        )?;
        if active > 0 {
            bail!("a syllabus scan for this class is already queued or running");
        }
        build_prompt(conn, class_id, rel_path, today)
    })?;
    // Enqueued outside the DB lock — the job runner takes the lock itself.
    crate::jobs::enqueue_syllabus(app, class_id, rel_path, &prompt)
}

fn build_prompt(
    conn: &Connection,
    class_id: i64,
    rel_path: Option<&str>,
    today: &str,
) -> Result<String> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let class_name: String = conn.query_row(
        "SELECT display_name FROM classes WHERE id = ?1",
        [class_id],
        |row| row.get(0),
    )?;

    let target = match rel_path {
        Some(rel) => {
            let abs = crate::scanner::resolve_rel(conn, class_id, rel)?;
            if !abs.is_file() {
                bail!("'{rel}' is not a file on disk — rescan the class");
            }
            format!(
                "Material to scan — this file, chosen in the app:\n\n{rel}\n\n\
                 If it is a `.pptx` (unreadable binary), read its conversion at \
                 `.classhub/extracts/{rel}.pdf` or its extract at \
                 `.classhub/extracts/{rel}.md` instead."
            )
        }
        None => {
            let mut tree_lines = Vec::new();
            let mut file_count = 0usize;
            crate::sorter::walk_tree(&class_dir, &class_dir, 0, &mut tree_lines, &mut file_count);
            let tree_block = if tree_lines.is_empty() {
                "(no material yet)".to_string()
            } else {
                tree_lines.join("\n")
            };
            format!(
                "Material to scan — the whole class folder. Look for syllabus-like \
                 files first (names containing \"syllabus\", course outlines, schedules), \
                 then any other material that commits to dates. Folder tree \
                 (app-managed folders excluded):\n\n{tree_block}"
            )
        }
    };

    let mut stmt = conn.prepare(
        "SELECT due_at, title, kind FROM deadlines
         WHERE class_id = ?1 ORDER BY due_at",
    )?;
    let existing_rows = stmt
        .query_map([class_id], |row| {
            Ok(format!(
                "- {} · {} ({})",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let existing = if existing_rows.is_empty() {
        "No deadlines are recorded for this class yet.".to_string()
    } else {
        format!(
            "Already recorded for this class (do not re-propose):\n\n{}",
            existing_rows.join("\n")
        )
    };

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{today}", today)
        .replace("{target}", &target)
        .replace("{existing}", &existing))
}

// ---------------------------------------------------------------------------
// Job completion (jobs.rs calls this on success, before the row leaves 'running')

/// One entry of the job's contracted JSON array.
#[derive(Deserialize)]
struct RawDeadline {
    title: String,
    #[serde(default)]
    kind: Option<String>,
    due_at: String,
    #[serde(default)]
    notes: Option<String>,
}

/// Parses and records the scan's proposals. Unlike the sort job, an empty
/// array is a legitimate success (the material may hold no dated items) —
/// only unparseable output or an all-invalid batch is an error the caller
/// demotes to job failure.
pub fn finalize_job(app: &AppHandle, class_id: i64, result_text: &str) -> Result<String> {
    let entries = crate::sorter::parse_entries(result_text)?;
    let summary = with_conn(app, |conn| {
        let mut recorded = 0usize;
        let mut duplicates = 0usize;
        let mut skipped = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        // Per-entry tolerance: one malformed entry costs itself, not the batch.
        for (index, raw) in entries.iter().enumerate() {
            let entry: RawDeadline = match serde_json::from_value(raw.clone()) {
                Ok(entry) => entry,
                Err(e) => {
                    skipped.push(format!("entry {} (malformed: {e})", index + 1));
                    continue;
                }
            };
            let title = entry.title.trim();
            if title.is_empty() {
                skipped.push(format!("entry {} (empty title)", index + 1));
                continue;
            }
            let due_at = entry.due_at.trim();
            if !valid_due_at(due_at) {
                skipped.push(format!("{title} (bad date '{due_at}')"));
                continue;
            }
            let kind = entry
                .kind
                .as_deref()
                .map(str::to_lowercase)
                .filter(|k| DEADLINE_KINDS.contains(&k.as_str()))
                .unwrap_or_else(|| "other".to_string());
            let notes = entry.notes.as_deref().map(str::trim).filter(|n| !n.is_empty());

            // Dedupe on (title, due date): within the batch, against existing
            // deadlines of any status, and one pending proposal per key.
            let key = format!("{}\u{0}{}", title.to_lowercase(), &due_at[..10]);
            if !seen.insert(key) {
                continue;
            }
            let existing: i64 = conn.query_row(
                "SELECT COUNT(*) FROM deadlines
                 WHERE class_id = ?1 AND LOWER(title) = LOWER(?2)
                   AND substr(due_at, 1, 10) = substr(?3, 1, 10)",
                params![class_id, title, due_at],
                |row| row.get(0),
            )?;
            if existing > 0 {
                duplicates += 1;
                continue;
            }
            let updated = conn.execute(
                "UPDATE deadline_proposals
                 SET kind = ?1, due_at = ?2, notes = ?3, created_at = ?4
                 WHERE class_id = ?5 AND LOWER(title) = LOWER(?6)
                   AND substr(due_at, 1, 10) = substr(?7, 1, 10) AND status = 'pending'",
                params![kind, due_at, notes, now(), class_id, title, due_at],
            )?;
            if updated == 0 {
                conn.execute(
                    "INSERT INTO deadline_proposals
                     (class_id, title, kind, due_at, notes, status, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6)",
                    params![class_id, title, kind, due_at, notes, now()],
                )?;
            }
            recorded += 1;
        }

        if recorded == 0 && duplicates == 0 && !skipped.is_empty() {
            bail!("no valid proposals in the scan output — skipped: {}", skipped.join("; "));
        }
        let mut summary = match (recorded, duplicates) {
            (0, 0) => "no date-bearing items found".to_string(),
            (0, d) => format!("nothing new — {d} dated item(s) already recorded"),
            (n, 0) => format!("{n} deadline proposal(s) awaiting review"),
            (n, d) => format!("{n} deadline proposal(s) awaiting review · {d} already recorded"),
        };
        if !skipped.is_empty() {
            summary.push_str(&format!(" · skipped {}", skipped.join("; ")));
        }
        Ok(summary)
    })?;
    emit_hub_change(app, "syllabus");
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Resolution: the confirm cards

/// Approve inserts the deadline (source='syllabus') and audits it; dismiss
/// parks the row — either way the card leaves the queue.
pub fn resolve_proposal(app: &AppHandle, proposal_id: i64, approve: bool) -> Result<String> {
    let summary = with_conn(app, |conn| {
        let row = conn
            .query_row(
                "SELECT class_id, title, kind, due_at, notes, status
                 FROM deadline_proposals WHERE id = ?1",
                [proposal_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )
            .optional()?
            .context("proposal not found")?;
        let (class_id, title, kind, due_at, notes, status) = row;
        if status != "pending" {
            bail!("this proposal was already resolved");
        }

        if !approve {
            conn.execute(
                "UPDATE deadline_proposals SET status = 'dismissed', resolved_at = ?1
                 WHERE id = ?2",
                params![now(), proposal_id],
            )?;
            return Ok(format!("skipped — {title}"));
        }

        // A matching deadline may have appeared since the scan (added by hand
        // or through chat) — approving would silently duplicate it.
        let existing: i64 = conn.query_row(
            "SELECT COUNT(*) FROM deadlines
             WHERE class_id = ?1 AND LOWER(title) = LOWER(?2)
               AND substr(due_at, 1, 10) = substr(?3, 1, 10)",
            params![class_id, title, due_at],
            |row| row.get(0),
        )?;
        if existing > 0 {
            bail!("'{title}' is already recorded for that date — skip this card instead");
        }
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open', 'syllabus')",
            params![class_id, title, kind, due_at, notes],
        )?;
        audit(
            conn,
            "syllabus.insert_deadline",
            json!({ "proposalId": proposal_id, "deadlineId": conn.last_insert_rowid(),
                    "classId": class_id, "title": title, "kind": kind,
                    "dueAt": due_at, "notes": notes }),
        )?;
        conn.execute(
            "UPDATE deadline_proposals SET status = 'approved', resolved_at = ?1
             WHERE id = ?2",
            params![now(), proposal_id],
        )?;
        Ok(format!("added — {title} due {due_at}"))
    })?;
    emit_hub_change(app, "syllabus");
    if approve {
        emit_hub_change(app, "deadlines");
    }
    Ok(summary)
}
