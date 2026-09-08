//! SPEC §11 — deadlines: the UI's CRUD (per-class list + dashboard strip) and
//! the syllabus_scan flow (read-only job → proposals → confirm cards → insert
//! with source='syllabus').
//!
//! The UI writes mirror the chat tools exactly: same due_at/kind validation
//! (tools.rs), destructive writes park the prior state in `audit_log`, and
//! every successful write emits `hub-changed {area:"deadlines"}` so the
//! frontend refetches without a manual refresh. Nothing a syllabus scan
//! proposes becomes a deadline without explicit approval.

use std::collections::HashMap;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::AppHandle;

use crate::db::{audit, emit_hub_change, notify, now, truncate, with_conn};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/syllabus.md");

// ---------------------------------------------------------------------------
// Domain rules — shared by every surface that writes a deadline (this
// module's UI commands and syllabus scan, and the chat tool in tools.rs).

pub(crate) const DEADLINE_KINDS: &[&str] = &["assignment", "exam", "quiz", "project", "other"];

/// Caps applied wherever a deadline title or notes crosses a boundary (chat
/// tool, UI form, syllabus scan). The scan is the one place model-supplied
/// document text enters persistent storage — and stored titles feed back
/// into the next scan's prompt, so unbounded growth would compound.
pub(crate) const MAX_TITLE_CHARS: usize = 200;
pub(crate) const MAX_NOTES_CHARS: usize = 1000;

/// ISO date, optionally with a time: YYYY-MM-DD[THH:MM[:SS]]. Stored as given;
/// lexicographic order is chronological order for this shape. Checks the
/// calendar, not just the shape: a syllabus scan can propose model-invented
/// dates like 2026-09-31, which would render as a rolled-over day while
/// sorting and deduping as the stored text.
pub(crate) fn valid_due_at(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    if b.len() < 10 || !digits(0..4) || b[4] != b'-' || !digits(5..7) || b[7] != b'-' || !digits(8..10)
    {
        return false;
    }
    let shape_ok = match b.len() {
        10 => true,
        16 => b[10] == b'T' && digits(11..13) && b[13] == b':' && digits(14..16),
        19 => {
            b[10] == b'T'
                && digits(11..13)
                && b[13] == b':'
                && digits(14..16)
                && b[16] == b':'
                && digits(17..19)
        }
        _ => false,
    };
    if !shape_ok {
        return false;
    }
    // The shape check proved every sliced range is ASCII digits.
    let num = |r: std::ops::Range<usize>| s[r].parse::<u32>().unwrap_or(0);
    let (year, month, day) = (num(0..4), num(5..7), num(8..10));
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > month_days {
        return false;
    }
    if b.len() >= 16 && (num(11..13) > 23 || num(14..16) > 59) {
        return false;
    }
    if b.len() == 19 && num(17..19) > 59 {
        return false;
    }
    true
}

/// The instant a due date names (SPEC §11): a date-only value is the end of
/// its day, a timed one its own time. One rule for every comparison, so a
/// date-only row sorts after a timed row on the same day rather than as its
/// midnight, and `schedule.ts` reads `overdue` by the same instant. Its live
/// form is `DUE_INSTANT_SQL`, which the test pins to this function.
#[cfg(test)]
pub fn due_instant(due_at: &str) -> String {
    if due_at.len() == 10 {
        format!("{due_at}T23:59:59")
    } else {
        due_at.to_string()
    }
}

/// `due_instant` as the expression an `ORDER BY due_at` takes.
pub const DUE_INSTANT_SQL: &str =
    "CASE WHEN length(due_at) = 10 THEN due_at || 'T23:59:59' ELSE due_at END";

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
    /// manual | agent | syllabus | canvas — stamped from the proposal that was
    /// approved, so a due date that turns out wrong can be traced to the reader
    /// that produced it.
    pub source: String,
    /// Set when the Canvas sync tracks this row (SPEC §7.2): its due date
    /// follows Canvas and a submission closes it, whatever `source` says
    /// about who first put it on the list.
    pub canvas_assignment_id: Option<String>,
}

/// Every deadline across every class, due-soonest first — the dashboard strip,
/// the class tab and the card line all filter this one payload client-side.
pub fn list_deadlines(conn: &Connection) -> Result<Vec<DeadlineInfo>> {
    // The instant, not the text (`due_instant`): a date-only row is the end
    // of its day, so it follows a timed row on the same day.
    let mut stmt = conn.prepare(&format!(
        "SELECT d.id, d.class_id, c.display_name, c.color, d.title, d.kind,
                d.due_at, d.notes, d.status, d.source, d.canvas_assignment_id
         FROM deadlines d JOIN classes c ON c.id = d.class_id
         ORDER BY {DUE_INSTANT_SQL}, d.id"
    ))?;
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
                canvas_assignment_id: row.get(10)?,
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
    let title = truncate(title.trim(), MAX_TITLE_CHARS);
    validate_fields(&title, kind, due_at)?;
    let notes = notes
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| truncate(n, MAX_NOTES_CHARS));
    // Write + audit land as one unit — a history entry must not be lost to a
    // failure between the two statements.
    let audit_id = with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let audit_id = match id {
            None => {
                tx.execute(
                    "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'open', 'manual')",
                    params![class_id, title, kind, due_at, notes],
                )?;
                audit(
                    &tx,
                    "ui.upsert_deadline",
                    json!({ "id": tx.last_insert_rowid(), "classId": class_id, "title": title,
                            "kind": kind, "dueAt": due_at, "notes": notes, "created": true }),
                )?
            }
            Some(id) => {
                let before = tx
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
                tx.execute(
                    "UPDATE deadlines SET title = ?1, kind = ?2, due_at = ?3, notes = ?4
                     WHERE id = ?5",
                    params![title, kind, due_at, notes, id],
                )?;
                audit(
                    &tx,
                    "ui.upsert_deadline",
                    json!({ "id": id, "classId": class_id,
                            "before": { "title": before.1, "kind": before.2,
                                        "dueAt": before.3, "notes": before.4 },
                            "after": { "title": title, "kind": kind,
                                       "dueAt": due_at, "notes": notes } }),
                )?
            }
        };
        tx.commit()?;
        Ok(audit_id)
    })?;
    emit_hub_change(app, "deadlines");
    let verb = if id.is_none() { "Added" } else { "Changed" };
    notify(app, format!("{verb} {title}"), vec![audit_id], Some(class_id));
    Ok(())
}

/// done ↔ open toggle — the row's checkbox. Reopening is as cheap as closing.
pub fn set_deadline_status(app: &AppHandle, id: i64, done: bool) -> Result<()> {
    let status = if done { "done" } else { "open" };
    let (audit_id, title, class_id) = with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let (before, title, class_id): (String, String, i64) = tx
            .query_row(
                "SELECT status, title, class_id FROM deadlines WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?
            .with_context(|| format!("no deadline #{id}"))?;
        tx.execute(
            "UPDATE deadlines SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        let audit_id = audit(
            &tx,
            "ui.set_deadline_status",
            json!({ "id": id, "classId": class_id, "status": status, "before": before }),
        )?;
        tx.commit()?;
        Ok((audit_id, title, class_id))
    })?;
    emit_hub_change(app, "deadlines");
    let verb = if done { "Marked" } else { "Reopened" };
    let tail = if done { " done" } else { "" };
    notify(app, format!("{verb} {title}{tail}"), vec![audit_id], Some(class_id));
    Ok(())
}

/// The full row rides the audit entry, so a deletion is recoverable — that
/// stands in for a confirmation prompt. Delete and audit commit together:
/// the audit row IS the undo, so the delete must never outlive it.
pub fn delete_deadline(app: &AppHandle, id: i64) -> Result<()> {
    let (audit_id, title, class_id) = with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let row = deadline_row(&tx, id)?.with_context(|| format!("no deadline #{id}"))?;
        let title = row["title"].as_str().unwrap_or_default().to_string();
        let class_id = row["classId"].as_i64().unwrap_or_default();
        tx.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        decline_assignment(&tx, &row)?;
        let audit_id = audit(&tx, "ui.delete_deadline", row)?;
        tx.commit()?;
        Ok((audit_id, title, class_id))
    })?;
    emit_hub_change(app, "deadlines");
    notify(app, format!("Deleted {title}"), vec![audit_id], Some(class_id));
    Ok(())
}

/// A deleted deadline that Canvas tracks is a decision the next sync has to
/// see, or the direct write (§7.2) puts the assignment back: its card is
/// recorded as declined — the row `insert_canvas_deadline` honours — in the
/// delete's own transaction. `row` is the deadline as `deadline_row` reads
/// it; a row Canvas does not track needs nothing.
pub(crate) fn decline_assignment(conn: &Connection, row: &serde_json::Value) -> Result<()> {
    let Some(canvas_id) = row["canvasAssignmentId"].as_str() else {
        return Ok(());
    };
    let class_id = row["classId"].as_i64().context("the row names no class")?;
    let declined = conn.execute(
        "UPDATE deadline_proposals SET status = 'dismissed', resolved_at = ?1
         WHERE class_id = ?2 AND canvas_assignment_id = ?3",
        params![now(), class_id, canvas_id],
    )?;
    if declined == 0 {
        conn.execute(
            "INSERT INTO deadline_proposals
             (class_id, title, kind, due_at, notes, status, source, canvas_assignment_id,
              created_at, resolved_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'dismissed', 'canvas', ?6, ?7, ?7)",
            params![
                class_id,
                row["title"].as_str().unwrap_or(""),
                row["kind"].as_str().unwrap_or("other"),
                row["dueAt"].as_str().unwrap_or(""),
                row["notes"].as_str(),
                canvas_id,
                now(),
            ],
        )?;
    }
    Ok(())
}

/// A deadline as its delete audits it — the whole row, Canvas id included,
/// so `undo_delete` can put it back as it was. Shared with the chat tool.
pub(crate) fn deadline_row(conn: &Connection, id: i64) -> Result<Option<serde_json::Value>> {
    Ok(conn
        .query_row(
            "SELECT class_id, title, kind, due_at, notes, status, source, canvas_assignment_id
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
                    "canvasAssignmentId": r.get::<_, Option<String>>(7)?,
                }))
            },
        )
        .optional()?)
}

// ---------------------------------------------------------------------------
// Undo (SPEC §6): the inverse of each deadline write, from its audit row

/// What an undo of one row did, for the notice and the hub pushes.
#[derive(Debug)]
pub(crate) struct Undone {
    pub what: String,
    pub class_id: Option<i64>,
}

/// The row an audit entry names, checked to still be that row: `deadlines.id`
/// is a plain rowid, which SQLite reissues once the highest is deleted, so a
/// bare id could name a deadline created since. The title and the class on
/// the payload settle it; a row the payload cannot vouch for is refused.
/// Returns the row's title for the notice.
fn same_deadline(
    conn: &Connection,
    id: i64,
    expected_title: Option<&str>,
    expected_class: Option<i64>,
) -> Result<String> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT title, class_id FROM deadlines WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((title, class_id)) = row else {
        bail!("{} is no longer on the list", expected_title.unwrap_or("the deadline"));
    };
    let same_title = expected_title.is_none_or(|t| t == title);
    let same_class = expected_class.is_none_or(|c| c == class_id);
    if !same_title || !same_class {
        bail!("deadline #{id} is a different row now ({title}) — this cannot be undone");
    }
    Ok(title)
}

/// `ui.upsert_deadline` / `chat.upsert_deadline`: a created row is removed,
/// an edited row takes its `before` back. Refused when the row is gone.
pub(crate) fn undo_upsert(conn: &Connection, payload: &serde_json::Value) -> Result<Undone> {
    let id = payload["id"].as_i64().context("the row names no deadline")?;
    let class_id = payload["classId"].as_i64();
    let title = same_deadline(conn, id, payload["title"].as_str(), class_id)?;
    if payload["created"].as_bool() == Some(true) {
        conn.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        return Ok(Undone { what: format!("Removed {title}"), class_id });
    }
    let before = &payload["before"];
    let (Some(old_title), Some(kind), Some(due_at)) = (
        before["title"].as_str(),
        before["kind"].as_str(),
        before["dueAt"].as_str(),
    ) else {
        bail!("the row carries no earlier state for deadline #{id}");
    };
    // The row must still hold what this change wrote: a later edit is not
    // this row's to undo, the rule the note undo keeps with its hash.
    let after = &payload["after"];
    let current: (String, String, String, Option<String>) = conn.query_row(
        "SELECT title, kind, due_at, notes FROM deadlines WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let still_this_change = after["title"].as_str().is_none_or(|t| t == current.0)
        && after["kind"].as_str().is_none_or(|k| k == current.1)
        && after["dueAt"].as_str().is_none_or(|d| d == current.2)
        && (after.get("notes").is_none() || after["notes"].as_str() == current.3.as_deref());
    if !still_this_change {
        bail!("{title} has been edited since — its current state is not this change's");
    }
    conn.execute(
        "UPDATE deadlines SET title = ?1, kind = ?2, due_at = ?3, notes = ?4 WHERE id = ?5",
        params![old_title, kind, due_at, before["notes"].as_str(), id],
    )?;
    Ok(Undone { what: format!("Restored {old_title}"), class_id })
}

/// `ui.delete_deadline` / `chat.delete_deadline`: the row comes back under
/// its own id. Refused when that id has since been taken.
pub(crate) fn undo_delete(conn: &Connection, payload: &serde_json::Value) -> Result<Undone> {
    let id = payload["id"].as_i64().context("the row names no deadline")?;
    let class_id = payload["classId"].as_i64().context("the row names no class")?;
    let title = payload["title"].as_str().context("the row carries no title")?;
    let taken: i64 =
        conn.query_row("SELECT COUNT(*) FROM deadlines WHERE id = ?1", [id], |r| r.get(0))?;
    if taken > 0 {
        bail!("deadline #{id} is on the list again already");
    }
    conn.execute(
        "INSERT INTO deadlines
         (id, class_id, title, kind, due_at, notes, status, source, canvas_assignment_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            id,
            class_id,
            title,
            payload["kind"].as_str().unwrap_or("other"),
            payload["dueAt"].as_str().context("the row carries no due date")?,
            payload["notes"].as_str(),
            payload["status"].as_str().unwrap_or("open"),
            payload["source"].as_str().unwrap_or("manual"),
            payload["canvasAssignmentId"].as_str(),
        ],
    )?;
    Ok(Undone { what: format!("Restored {title}"), class_id: Some(class_id) })
}

/// `ui.set_deadline_status` / `chat.complete_deadline`: the status before —
/// recorded on the row from M33 on, and the other one of the two before it.
pub(crate) fn undo_status(conn: &Connection, payload: &serde_json::Value) -> Result<Undone> {
    let id = payload["id"].as_i64().context("the row names no deadline")?;
    let before = match (payload["before"].as_str(), payload["status"].as_str()) {
        (Some(before), _) => before,
        (None, Some("open")) => "done",
        (None, _) => "open",
    };
    let (title, class_id, status): (String, i64, String) = conn
        .query_row(
            "SELECT title, class_id, status FROM deadlines WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .with_context(|| format!("deadline #{id} is no longer on the list"))?;
    // A status the row no longer holds was changed since; that change is
    // its own row's to undo.
    if payload["status"].as_str().is_some_and(|written| written != status) {
        bail!("{title} has been changed since — its status is not this change's");
    }
    conn.execute("UPDATE deadlines SET status = ?1 WHERE id = ?2", params![before, id])?;
    let what = if before == "done" {
        format!("Marked {title} done again")
    } else {
        format!("Reopened {title}")
    };
    Ok(Undone { what, class_id: Some(class_id) })
}

/// `syllabus.insert_deadline`: the deadline goes and its card returns to the
/// queue. Refused when the deadline is already gone.
pub(crate) fn undo_insert(conn: &Connection, payload: &serde_json::Value) -> Result<Undone> {
    let id = payload["deadlineId"].as_i64().context("the row names no deadline")?;
    let title = same_deadline(conn, id, payload["title"].as_str(), payload["classId"].as_i64())?;
    conn.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
    if let Some(proposal_id) = payload["proposalId"].as_i64() {
        conn.execute(
            "UPDATE deadline_proposals SET status = 'pending', resolved_at = NULL WHERE id = ?1",
            [proposal_id],
        )?;
    }
    Ok(Undone { what: format!("Removed {title}"), class_id: payload["classId"].as_i64() })
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
    /// syllabus | canvas — the card says where the date came from, because
    /// "Canvas says this is due then" and "a PDF seemed to say so" warrant
    /// different amounts of scrutiny before approving.
    pub source: String,
}

/// How many of the class's proposals still wait — the card's `N PROPOSED`
/// badge, read on every dashboard refetch, so it counts rather than loads.
pub fn pending_count(conn: &Connection, class_id: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM deadline_proposals WHERE class_id = ?1 AND status = 'pending'",
        [class_id],
        |row| row.get(0),
    )?)
}

/// The class's confirm queue: every proposed deadline still awaiting a
/// decision, whichever reader proposed it.
pub fn pending_proposals(conn: &Connection, class_id: i64) -> Result<Vec<DeadlineProposal>> {
    let mut stmt = conn.prepare(
        "SELECT id, class_id, title, kind, due_at, notes, created_at, source
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
                source: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The queue as the workspace lists it: every pending card, and the series
/// among them (SPEC §11) — read off the cards each time, never stored, so a
/// rescan that adds a thirteenth date joins the card.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineQueue {
    pub proposals: Vec<DeadlineProposal>,
    pub series: Vec<DeadlineSeries>,
}

pub fn queue(conn: &Connection, class_id: i64) -> Result<DeadlineQueue> {
    let proposals = pending_proposals(conn, class_id)?;
    let series = series_of(&proposals);
    Ok(DeadlineQueue { proposals, series })
}

/// A recurring proposal read as one card: pending cards of one class sharing
/// a title stem, a kind, a source and a weekday across three or more dates —
/// Fundamentals' twelve `Live coding session MM/DD` Tuesdays.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeadlineSeries {
    /// The title with its date token dropped: `Live coding session`.
    pub stem: String,
    pub kind: String,
    pub source: String,
    /// `Tuesday`, as chrono names it.
    pub weekday: String,
    /// The member cards, by date.
    pub ids: Vec<i64>,
    pub first_due: String,
    pub last_due: String,
}

/// The series among a queue's cards. Pure over the rows, so the grouping is
/// tested without a database.
pub fn series_of(proposals: &[DeadlineProposal]) -> Vec<DeadlineSeries> {
    // Keyed in a map, so the grouping is one pass and the series come out in
    // one order — by stem, then kind, source and weekday.
    let mut groups: std::collections::BTreeMap<(String, String, String, String), Vec<&DeadlineProposal>> =
        std::collections::BTreeMap::new();
    for proposal in proposals {
        let Some(weekday) = weekday_of(&proposal.due_at) else {
            continue;
        };
        let key = (
            title_stem(&proposal.title),
            proposal.kind.clone(),
            proposal.source.clone(),
            weekday,
        );
        groups.entry(key).or_default().push(proposal);
    }
    groups
        .into_iter()
        .filter_map(|((stem, kind, source, weekday), mut members)| {
            members.sort_by(|a, b| a.due_at.cmp(&b.due_at).then(a.id.cmp(&b.id)));
            let dates: std::collections::BTreeSet<&str> =
                members.iter().map(|m| &m.due_at[..10]).collect();
            if dates.len() < 3 {
                return None;
            }
            Some(DeadlineSeries {
                stem,
                kind,
                source,
                weekday,
                ids: members.iter().map(|m| m.id).collect(),
                first_due: members[0].due_at.clone(),
                last_due: members[members.len() - 1].due_at.clone(),
            })
        })
        .collect()
}

/// A title with its trailing date-like tokens dropped — `09/08`, `9-8`,
/// `2026-09-08`, `#3`, `(2)`, a bare number — and a dangling separator after
/// them: `Live coding session 09/08` and `Homework #1` read as `Live coding
/// session` and `Homework`. A title that is nothing but a date keeps itself.
pub fn title_stem(title: &str) -> String {
    let mut words: Vec<&str> = title.split_whitespace().collect();
    let date_like = |word: &str| {
        let core = word.trim_matches(|c: char| c == '(' || c == ')' || c == '#');
        !core.is_empty()
            && core.chars().any(|c| c.is_ascii_digit())
            && core.chars().all(|c| c.is_ascii_digit() || matches!(c, '/' | '-' | '.' | ':'))
    };
    loop {
        let before = words.len();
        while words.len() > 1 && date_like(words[words.len() - 1]) {
            words.pop();
        }
        while words.len() > 1 && matches!(words[words.len() - 1], "-" | "–" | "—" | ":" | "·") {
            words.pop();
        }
        if words.len() == before {
            break;
        }
    }
    words.join(" ")
}

/// The weekday a due date falls on, by its calendar day.
fn weekday_of(due_at: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(due_at.get(..10)?, "%Y-%m-%d")
        .ok()
        .map(|d| d.format("%A").to_string())
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
            crate::scanner::walk_tree(&class_dir, &class_dir, 0, &mut tree_lines, &mut file_count);
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
    let mut existing = if existing_rows.is_empty() {
        "No deadlines are recorded for this class yet.".to_string()
    } else {
        format!(
            "Already recorded for this class (do not re-propose):\n\n{}",
            existing_rows.join("\n")
        )
    };
    // A dismissal is a decision already made (the sorter's convention): tell
    // the model, and finalize enforces it either way.
    let mut stmt = conn.prepare(
        "SELECT due_at, title FROM deadline_proposals
         WHERE class_id = ?1 AND status = 'dismissed' ORDER BY due_at",
    )?;
    let dismissed_rows = stmt
        .query_map([class_id], |row| {
            Ok(format!(
                "- {} · {}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !dismissed_rows.is_empty() {
        existing.push_str(&format!(
            "\n\nSkipped earlier in the app (do not re-propose these either):\n\n{}",
            dismissed_rows.join("\n")
        ));
    }
    let categories = categories_block(conn, class_id)?;

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{today}", today)
        .replace("{semester}", &semester_label(today))
        .replace("{target}", &target)
        .replace("{existing}", &existing)
        .replace("{categories}", &categories))
}

/// The prompt's block of category names the syllabus breakdown is mapped
/// onto — names only, so the model reads the weights out of the document
/// rather than echoing what was typed. Canvas supplied most of them (§7.2),
/// so its wording is what the syllabus's has to be matched to.
fn categories_block(conn: &Connection, class_id: i64) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT name FROM grade_categories WHERE class_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| Ok(format!("- {}", row.get::<_, String>(0)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if rows.is_empty() {
        "The class has no grade categories yet.".to_string()
    } else {
        format!(
            "Grade categories the class already tracks (map the syllabus breakdown onto \
             these names where they mean the same thing):\n\n{}",
            rows.join("\n")
        )
    })
}

/// "Fall 2026" from a YYYY-MM-DD. Month-level precision is all the prompt
/// needs — it anchors how the model resolves partial dates like "Sept 3",
/// which is stored data, not a display label.
fn semester_label(today_iso: &str) -> String {
    let year = today_iso.get(0..4).unwrap_or("");
    let month: u32 = today_iso
        .get(5..7)
        .and_then(|m| m.parse().ok())
        .unwrap_or(1);
    let term = match month {
        1..=4 => "Spring",
        5..=7 => "Summer",
        _ => "Fall",
    };
    format!("{term} {year}")
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

/// One entry of the scan's `units` array — the course's own divisions, read
/// out of the same document in the same pass (SPEC §7.2).
#[derive(Deserialize)]
struct RawUnit {
    name: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    ordinal: Option<i64>,
    #[serde(default)]
    starts_on: Option<String>,
    #[serde(default)]
    ends_on: Option<String>,
    /// The weeks a division spans, for one that groups them (SPEC §8.5).
    #[serde(default)]
    first_week: Option<i64>,
    #[serde(default)]
    last_week: Option<i64>,
    /// What the division set out to teach, where the syllabus states it
    /// (SPEC §11): the bullet list under its topic, each line a string.
    #[serde(default)]
    objectives: Option<Vec<serde_json::Value>>,
}

/// One entry of the scan's `grading` array — a component of the final grade
/// and its share, read out of the same document (SPEC §11). The weight is
/// kept as a value because the tables it is read from print `50%`, and a
/// model copying that string should not cost the scan its breakdown.
#[derive(Deserialize)]
struct RawWeight {
    name: String,
    weight: serde_json::Value,
}

/// The three parts of a scan's output, split by `split_output`.
struct ScanOutput {
    deadlines: Vec<serde_json::Value>,
    units: Vec<serde_json::Value>,
    grading: Vec<serde_json::Value>,
}

/// Parses and records the scan's proposals. Unlike the sort job, an empty
/// array is a legitimate success (the material may hold no dated items) —
/// only unparseable output or an all-invalid batch is an error the caller
/// demotes to job failure.
pub fn finalize_job(app: &AppHandle, class_id: i64, result_text: &str) -> Result<String> {
    let ScanOutput { deadlines: entries, units: raw_units, grading: raw_weights } =
        split_output(result_text)?;
    let unit_summary = record_units(app, class_id, &raw_units);
    let weight_summary = record_weights(app, class_id, &raw_weights);
    // What the other two parts recorded, carried whichever way the deadline
    // part ends: their writes stand, so a job demoted for its deadlines must
    // still say what it wrote — a weight set in silence is the one thing the
    // weights part exists not to do.
    let recorded_parts: Vec<String> =
        [unit_summary, weight_summary].into_iter().flatten().collect();
    let summary = with_conn(app, |conn| {
        let mut recorded = 0usize;
        let mut duplicates = 0usize;
        let mut dismissed_skips = 0usize;
        let mut skipped = Vec::new();
        // key -> whether the winning entry carried a time of day.
        let mut seen: HashMap<String, bool> = HashMap::new();
        let mut superseded: Vec<String> = Vec::new();
        // Per-entry tolerance: one malformed entry costs itself, not the batch.
        for (index, raw) in entries.iter().enumerate() {
            let entry: RawDeadline = match serde_json::from_value(raw.clone()) {
                Ok(entry) => entry,
                Err(e) => {
                    skipped.push(format!("entry {} (malformed: {e})", index + 1));
                    continue;
                }
            };
            // The untrusted boundary: model-supplied text gets capped here,
            // before it can enter storage (and the next scan's prompt).
            let title = truncate(entry.title.trim(), MAX_TITLE_CHARS);
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
            let notes = entry
                .notes
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| truncate(n, MAX_NOTES_CHARS));

            // Dedupe on (title, due date): within the batch, against existing
            // deadlines of any status, and one pending proposal per key.
            // ASCII folding to match SQLite's LOWER() in the queries below —
            // Unicode folding here would let the two layers disagree on
            // non-ASCII titles and slip a duplicate pending row through.
            let key = format!("{}\u{0}{}", title.to_ascii_lowercase(), &due_at[..10]);
            // The key is date-granular, so "Quiz 1 on 2026-09-03" and "Quiz 1
            // at 2026-09-03T23:59" collide. Keeping whichever the model
            // emitted first would silently discard a stated time, so the entry
            // carrying one wins and the loser is reported rather than dropped
            // in silence.
            let has_time = due_at.len() > 10;
            match seen.entry(key) {
                std::collections::hash_map::Entry::Occupied(mut slot) => {
                    if has_time && !*slot.get() {
                        slot.insert(true);
                        superseded.push(title.clone());
                    } else {
                        skipped.push(format!("{title} (duplicate of another entry)"));
                        continue;
                    }
                }
                std::collections::hash_map::Entry::Vacant(slot) => {
                    slot.insert(has_time);
                }
            }
            match record_proposal(
                conn,
                class_id,
                &title,
                &kind,
                due_at,
                notes.as_deref(),
                "syllabus",
                None,
            )? {
                // A re-proposal of something still waiting counts as recorded
                // here: the scan did find it, and the card is in the queue.
                Recorded::Proposed | Recorded::Refreshed => recorded += 1,
                Recorded::AlreadyDeadline => duplicates += 1,
                Recorded::DismissedBefore => dismissed_skips += 1,
            }
        }

        if recorded == 0 && duplicates == 0 && dismissed_skips == 0 && !skipped.is_empty() {
            bail!(
                "no valid proposals in the scan output — skipped: {}{}",
                skipped.join("; "),
                recorded_parts
                    .iter()
                    .map(|part| format!(" · {part}"))
                    .collect::<String>()
            );
        }
        let mut known = Vec::new();
        if duplicates > 0 {
            known.push(format!("{duplicates} already recorded"));
        }
        if dismissed_skips > 0 {
            known.push(format!("{dismissed_skips} skipped earlier"));
        }
        if !superseded.is_empty() {
            known.push(format!(
                "{} kept with the stated time",
                superseded.len()
            ));
        }
        let mut summary = if recorded == 0 {
            if known.is_empty() {
                "no date-bearing items found".to_string()
            } else {
                format!("nothing new — {}", known.join(" · "))
            }
        } else {
            let mut s = format!("{recorded} deadline proposal(s) awaiting review");
            for part in &known {
                s.push_str(&format!(" · {part}"));
            }
            s
        };
        if !skipped.is_empty() {
            summary.push_str(&format!(" · skipped {}", skipped.join("; ")));
        }
        Ok(summary)
    })?;
    emit_hub_change(app, "deadlineProposals");
    let summary = std::iter::once(summary)
        .chain(recorded_parts)
        .collect::<Vec<_>>()
        .join(" · ");
    Ok(summary)
}

/// Splits the scan's output into its three parts.
///
/// The contract is one object holding `deadlines`, `units` and `grading`,
/// because the weekly schedule, the due dates and the grade breakdown live in
/// the same document and cost one read between them. A bare array is still
/// accepted as the deadline list alone: the model does occasionally answer the
/// older shape, and dropping a whole scan's findings over the wrapper would be
/// an expensive way to be strict about punctuation.
fn split_output(result_text: &str) -> Result<ScanOutput> {
    if let Ok(record) = crate::jobs::parse_object(result_text) {
        if ["deadlines", "units", "grading"]
            .iter()
            .any(|key| record.get(key).is_some())
        {
            // An absent key is a real answer — a syllabus may hold no dated
            // items, or no structure. A key that is present and not a list is
            // not: collapsing that to an empty vec reported "no date-bearing
            // items found", the same words a genuinely dateless syllabus earns,
            // and the scan's whole deadline part vanished without a trace.
            let array = |key: &str| -> Result<Vec<serde_json::Value>> {
                match record.get(key) {
                    None | Some(serde_json::Value::Null) => Ok(Vec::new()),
                    Some(serde_json::Value::Array(items)) => Ok(items.clone()),
                    Some(other) => bail!(
                        "the scan's `{key}` came back as {} rather than a list",
                        match other {
                            serde_json::Value::Object(_) => "an object",
                            serde_json::Value::String(_) => "a string",
                            _ => "a value",
                        }
                    ),
                }
            };
            return Ok(ScanOutput {
                deadlines: array("deadlines")?,
                units: array("units")?,
                grading: array("grading")?,
            });
        }
    }
    Ok(ScanOutput {
        deadlines: crate::jobs::parse_entries(result_text)?,
        units: Vec::new(),
        grading: Vec::new(),
    })
}

/// Records the divisions the syllabus declared, as SPEC §7.2's middle source.
///
/// Failures here are reported, never propagated: the parts of a scan are
/// independent findings out of one document, and losing the deadlines because
/// a week entry was malformed would be the wrong trade. Run before the
/// deadlines block for the same reason in reverse — a scan whose deadline part
/// is unusable has still read the schedule, and that is worth keeping.
fn record_units(app: &AppHandle, class_id: i64, raw: &[serde_json::Value]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let recorded = with_conn(app, |conn| {
        let mut added = 0usize;
        let mut updated = 0usize;
        let mut seen = 0usize;
        // A division is matched on its label (SPEC §7.2), and a model can
        // report two entries under one — two "Week 7" rows, or one topic twice
        // under no label. The second would find the row the first just wrote
        // and rename it, so the batch claims each row once and the entries it
        // set aside are named.
        let mut collapsed: Vec<String> = Vec::new();
        let mut skipped: Vec<String> = Vec::new();
        let mut batch = crate::units::Batch::default();
        for (index, value) in raw.iter().enumerate() {
            let Ok(entry) = serde_json::from_value::<RawUnit>(value.clone()) else {
                continue;
            };
            let name = entry.name.trim();
            if name.is_empty() {
                continue;
            }
            seen += 1;
            let kind = entry
                .kind
                .as_deref()
                .map(str::to_lowercase)
                .filter(|k| crate::units::UNIT_KINDS.contains(&k.as_str()))
                .unwrap_or_else(|| crate::units::kind_for_name(name).to_string());
            let stated = match (entry.first_week, entry.last_week) {
                (Some(first), Some(last)) => Some((first, last)),
                _ => None,
            };
            let unit = crate::units::NewUnit {
                ordinal: entry.ordinal.unwrap_or(index as i64 + 1),
                // The range the model stated, else the one the name carries;
                // none for a week, whose number says it (SPEC §8.5).
                weeks: crate::units::declared_weeks(&kind, name, stated),
                kind,
                name: name.to_string(),
                canvas_id: None,
                rel_path: None,
                // A syllabus week without a date is ordinary — two of the four
                // courses number their weeks and never date them — so an
                // unparseable date drops the date, not the unit. Truncated to
                // the day because that is what the column holds: `valid_due_at`
                // also accepts a time, and a `starts_on` carrying one renders
                // as nothing at all in the workspace.
                starts_on: entry.starts_on.filter(|d| valid_due_at(d)).map(day_of),
                ends_on: entry.ends_on.filter(|d| valid_due_at(d)).map(day_of),
                // A stated list replaces the column; none stated keeps it.
                objectives: stated_objectives(entry.objectives.as_deref()),
                source: "syllabus",
            };
            match crate::units::upsert(conn, class_id, &unit, &mut batch) {
                Ok(written) => match written.outcome {
                    crate::units::Outcome::Inserted => added += 1,
                    crate::units::Outcome::Updated => updated += 1,
                    crate::units::Outcome::Unchanged => {}
                    crate::units::Outcome::Claimed => collapsed.push(name.to_string()),
                },
                Err(e) => {
                    eprintln!("syllabus: skipping unit '{name}': {e:#}");
                    skipped.push(format!("{name} ({e})"));
                }
            }
        }
        // A claim is decided by what a pass wrote, so it is said once, by the
        // pass that changed a row; a rescan that wrote nothing repeats nothing.
        let claims = if added > 0 || updated > 0 {
            crate::units::week_claims(conn, class_id)?
        } else {
            Vec::new()
        };
        Ok((added, updated, seen, collapsed, skipped, batch, claims))
    });
    match recorded {
        Ok((_, _, 0, ..)) => None,
        Ok((added, updated, seen, collapsed, skipped, batch, claims)) => {
            // A renamed division's corpus folder and guide follow it once the
            // rows are committed and the lock is released; `files` is what
            // refreshes the guides and the lecture listing that name them.
            let moved = batch.moves_anything();
            batch.apply();
            if added > 0 || updated > 0 {
                crate::db::emit_hub_change(app, "units");
            }
            if moved {
                crate::db::emit_hub_change(app, "files");
            }
            let mut summary = match (added, updated) {
                (0, 0) => format!("{seen} division(s) already recorded"),
                (0, _) => format!("{updated} of {seen} division(s) updated in place"),
                (_, 0) => format!("{added} of {seen} division(s) recorded"),
                _ => format!("{added} of {seen} division(s) recorded · {updated} updated in place"),
            };
            if !collapsed.is_empty() {
                summary.push_str(&format!(
                    " · {} share a label with an earlier one and were not recorded separately: {}",
                    collapsed.len(),
                    collapsed.join(", ")
                ));
            }
            if !skipped.is_empty() {
                summary.push_str(&format!(
                    " · {} could not be recorded: {}",
                    skipped.len(),
                    skipped.join("; ")
                ));
            }
            for claim in &claims {
                eprintln!("units: {claim}");
            }
            if !claims.is_empty() {
                summary.push_str(&format!(
                    " · {} week(s) claimed twice: {}",
                    claims.len(),
                    claims.join("; ")
                ));
            }
            Some(summary)
        }
        Err(e) => {
            eprintln!("syllabus: units not recorded for class {class_id}: {e:#}");
            Some("divisions could not be recorded".to_string())
        }
    }
}

/// How many objectives a division may state, and how long one line may run:
/// a model copying a week's whole reading list into the field should not
/// cost the scan the week.
const MAX_OBJECTIVES: usize = 20;
const MAX_OBJECTIVE_CHARS: usize = 300;

/// The scan's objectives for one division as `NewUnit` takes them: the
/// strings of the list, trimmed and capped; `None` when the entry stated
/// none, so the column keeps what an earlier scan read.
fn stated_objectives(raw: Option<&[serde_json::Value]>) -> Option<Vec<String>> {
    let lines: Vec<String> = raw?
        .iter()
        .filter_map(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| crate::db::truncate(s, MAX_OBJECTIVE_CHARS))
        .take(MAX_OBJECTIVES)
        .collect();
    (!lines.is_empty()).then_some(lines)
}

/// What a scan's grading part did, for the job summary.
#[derive(Default)]
struct WeightsRecorded {
    /// Weights written: `Assignments 50`, `Peer Design Sessions 20 (new)`.
    set: Vec<String>,
    /// Weights already agreeing with the syllabus.
    unchanged: usize,
    /// Typed weights the syllabus disagrees with, left alone and named.
    kept: Vec<String>,
    /// Categories still at zero after the pass, which the syllabus did not
    /// weight — the ≠100% warning will name them, and so does the summary.
    unweighted: Vec<String>,
    skipped: Vec<String>,
}

impl WeightsRecorded {
    fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.set.is_empty() {
            parts.push(format!("weights set: {}", self.set.join(", ")));
        }
        if self.unchanged > 0 {
            parts.push(format!(
                "{} weight(s) already as the syllabus states",
                self.unchanged
            ));
        }
        parts.extend(self.kept.iter().cloned());
        if !self.unweighted.is_empty() {
            parts.push(format!(
                "{} left at 0 — the syllabus does not weight {}",
                self.unweighted.join(", "),
                if self.unweighted.len() == 1 { "it" } else { "them" }
            ));
        }
        if !self.skipped.is_empty() {
            parts.push(format!("weights skipped: {}", self.skipped.join("; ")));
        }
        parts.join(" · ")
    }
}

/// Records the grade breakdown the syllabus states (SPEC §11): a category
/// whose weight is still zero takes the syllabus's, one the class lacks is
/// created, and one already set is left alone and named. Same posture as
/// `record_units` — reported, never propagated, and run before the deadline
/// part so a scan whose deadlines are unusable still keeps what it read.
fn record_weights(app: &AppHandle, class_id: i64, raw: &[serde_json::Value]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    match with_conn(app, |conn| apply_weights(conn, class_id, raw)) {
        Ok(recorded) => {
            if !recorded.set.is_empty() {
                emit_hub_change(app, "grades");
            }
            Some(recorded.summary())
        }
        Err(e) => {
            eprintln!("syllabus: weights not recorded for class {class_id}: {e:#}");
            Some("weights could not be recorded".to_string())
        }
    }
}

fn apply_weights(
    conn: &Connection,
    class_id: i64,
    raw: &[serde_json::Value],
) -> Result<WeightsRecorded> {
    use crate::grades::{set_syllabus_weight, trim_num, WeightWrite, MAX_NAME_CHARS};
    let mut out = WeightsRecorded::default();
    // Names this scan has handled, lowercased: a second entry under one name
    // is the model repeating itself, and the first reading stands.
    let mut claimed: Vec<String> = Vec::new();
    for (index, value) in raw.iter().enumerate() {
        let entry: RawWeight = match serde_json::from_value(value.clone()) {
            Ok(entry) => entry,
            Err(e) => {
                out.skipped.push(format!("entry {} (malformed: {e})", index + 1));
                continue;
            }
        };
        // The untrusted boundary, as in the deadline part: model-supplied text
        // is capped before it reaches the summary, at the bound the stored
        // name has, so the summary names the category the class holds.
        let name = truncate(entry.name.trim(), MAX_NAME_CHARS);
        if name.is_empty() {
            out.skipped.push(format!("entry {} (empty name)", index + 1));
            continue;
        }
        let Some(weight) = percent_of(&entry.weight) else {
            out.skipped.push(format!(
                "{name} (bad weight {})",
                truncate(&entry.weight.to_string(), MAX_NAME_CHARS)
            ));
            continue;
        };
        // ASCII folding, to match SQLite's LOWER() in the lookup below — the
        // deadline part's rule, for the same reason: Unicode folding here
        // would let the two layers disagree on a non-ASCII name.
        let lowered = name.to_ascii_lowercase();
        if claimed.contains(&lowered) {
            out.skipped.push(format!("{name} (repeated)"));
            continue;
        }
        claimed.push(lowered);
        match set_syllabus_weight(conn, class_id, &name, weight) {
            Ok(WeightWrite::Set) => out.set.push(format!("{name} {}", trim_num(weight))),
            Ok(WeightWrite::Created) => {
                out.set.push(format!("{name} {} (new)", trim_num(weight)))
            }
            Ok(WeightWrite::Unchanged) => out.unchanged += 1,
            Ok(WeightWrite::Kept(current)) => out.kept.push(format!(
                "{name} kept at {} — the syllabus says {}",
                trim_num(current),
                trim_num(weight)
            )),
            Err(e) => out.skipped.push(format!("{name} ({e})")),
        }
    }
    // Best-effort: every entry above has committed on its own, so a failure
    // here costs the summary one line and must not read as "not recorded".
    match still_at_zero(conn, class_id) {
        Ok(names) => {
            out.unweighted = names
                .into_iter()
                .filter(|name| !claimed.contains(&name.to_ascii_lowercase()))
                .collect();
        }
        Err(e) => eprintln!(
            "syllabus: the categories still at zero for class {class_id} were not listed: {e:#}"
        ),
    }
    Ok(out)
}

/// The class's categories whose weight nobody has stated yet, in list order —
/// zero within the epsilon `set_syllabus_weight` reads it with.
fn still_at_zero(conn: &Connection, class_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM grade_categories
         WHERE class_id = ?1 AND ABS(weight) < ?2 ORDER BY id",
    )?;
    let names = stmt
        .query_map(params![class_id, crate::grades::WEIGHT_EPSILON], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(names)
}

/// A weight as the scan reported it: a number, or the `50%` the syllabus's
/// own table prints when the model copies it as a string. A bare value
/// between 0 and 1 is a share written the other way round (`0.5` for 50%)
/// rather than half a percent, and is refused so the summary names it; with
/// an explicit `%` it is what it says.
fn percent_of(value: &serde_json::Value) -> Option<f64> {
    let (explicit, weight) = match value {
        serde_json::Value::Number(n) => (false, n.as_f64()),
        serde_json::Value::String(s) => {
            let text = s.trim();
            let bare = text.strip_suffix('%');
            (bare.is_some(), bare.unwrap_or(text).trim().parse().ok())
        }
        _ => (false, None),
    };
    weight
        .filter(|w| w.is_finite())
        .filter(|w| explicit || !(0.0 < *w && *w < 1.0))
}

/// The day half of a validated ISO timestamp. `valid_due_at` guarantees at
/// least ten ASCII characters, so the slice is safe.
fn day_of(iso: String) -> String {
    iso[..10].to_string()
}

/// What recording a proposal did, so a caller can say so rather than report a
/// count that hides three different outcomes.
pub(crate) enum Recorded {
    /// A card that was not in the queue before.
    Proposed,
    /// A card already waiting, refreshed in place rather than stacked. A
    /// re-sync of unchanged Canvas assignments is all of these, which is why
    /// it is worth telling apart from a fresh proposal.
    Refreshed,
    /// This deadline is already on the list — added by hand, by chat, or by an
    /// earlier approval.
    AlreadyDeadline,
    /// The card was declined before. That is a decision already made, and a
    /// re-scan or a re-sync must not put it back; adding it by hand is the way
    /// back.
    DismissedBefore,
}

/// The (title, calendar day) identity both readers share, bound to ?2 (the
/// title) and ?3 (the due date). One definition, so the rule the comment on
/// `record_proposal` promises cannot drift between the five queries that ask
/// it.
/// Titles compare with `#` dropped, so Canvas's `Homework #1` is the
/// syllabus's `Homework 1` on the same day rather than a row beside it.
const SAME_TITLE_AND_DAY: &str =
    "LOWER(REPLACE(title, '#', '')) = LOWER(REPLACE(?2, '#', ''))
     AND substr(due_at, 1, 10) = substr(?3, 1, 10)";

/// Keeps a (title, day) match away from rows that are some *other* Canvas
/// assignment, bound to ?4 — the reader's Canvas id, or NULL for a reader
/// without one, which then matches every row.
const NOT_ANOTHER_ASSIGNMENT: &str =
    "(?4 IS NULL OR canvas_assignment_id IS NULL OR canvas_assignment_id = ?4)";

/// How far apart two readings of one assignment's due date may fall: a
/// syllabus names the week's Sunday and Canvas the Monday at 11:59 pm.
const SAME_ASSIGNMENT_DAYS: i64 = 2;

/// A title reduced to what names the assignment (SPEC §7.2): lowercased,
/// `#` and punctuation dropped, `hw` and `assignment` read as `homework` —
/// the syllabus's `Homework 1`, Canvas's `Homework Assignment 1` and a
/// `HW #1` are one key — and repeats collapsed.
pub(crate) fn assignment_key(title: &str) -> String {
    let mut words: Vec<&str> = Vec::new();
    let lowered = title.to_lowercase();
    for word in lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = match word {
            "hw" | "assignment" | "assignments" | "homeworks" => "homework",
            // Connectives carry no name: `Problem Statement and AI Sketch`
            // and `Problem Statement + AI Pitch` differ in one word, not two.
            "and" | "the" | "a" | "an" | "of" | "for" | "to" | "in" | "on" | "with" => continue,
            other => other,
        };
        if words.last() != Some(&word) {
            words.push(word);
        }
    }
    words.join(" ")
}

/// Whether two readings name one assignment: the same key, due within
/// `SAME_ASSIGNMENT_DAYS` of each other. Pure, since the wrong answer is
/// silent either way — a row beside its own duplicate, or two homeworks
/// folded into one.
pub(crate) fn same_assignment(title_a: &str, due_a: &str, title_b: &str, due_b: &str) -> bool {
    let (key_a, key_b) = (assignment_key(title_a), assignment_key(title_b));
    if key_a != key_b && !similar_keys(&key_a, &key_b) {
        return false;
    }
    let day = |due: &str| chrono::NaiveDate::parse_from_str(due.get(..10).unwrap_or(""), "%Y-%m-%d").ok();
    match (day(due_a), day(due_b)) {
        (Some(a), Some(b)) => (a - b).num_days().abs() <= SAME_ASSIGNMENT_DAYS,
        _ => false,
    }
}

/// How much of two keys' words must be shared for them to name one thing
/// when the keys differ: the syllabus's `Problem Statement + AI Pitch` and
/// Canvas's `Problem Statement and AI Sketch` share three words of five.
const SIMILAR_KEY_SHARE: f64 = 0.6;

/// Two keys that differ but name one assignment: every number in either is
/// in both — `Homework 1 Draft` is never `Homework 2` — and the words they
/// share are at least `SIMILAR_KEY_SHARE` of the words they use between them.
fn similar_keys(key_a: &str, key_b: &str) -> bool {
    let (a, b) = (key_words(key_a), key_words(key_b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if key_numbers(&a) != key_numbers(&b) {
        return false;
    }
    let shared = a.intersection(&b).count() as f64;
    let used = a.union(&b).count() as f64;
    shared / used >= SIMILAR_KEY_SHARE
}

/// Whether an untracked row of `source` is the Canvas assignment: the
/// syllabus's rows by the looser reading above, since they are a model's
/// reading of prose about it; a row the owner typed or asked chat for only
/// on the same key and the same calendar day, so a personal marker two days
/// before an assignment is never folded into it.
fn names_assignment(source: &str, canvas_title: &str, canvas_due: &str, title: &str, due: &str) -> bool {
    if source == "syllabus" {
        return same_assignment(canvas_title, canvas_due, title, due);
    }
    assignment_key(canvas_title) == assignment_key(title)
        && canvas_due.get(..10).is_some_and(|d| due.get(..10) == Some(d))
}

fn key_words(key: &str) -> std::collections::BTreeSet<&str> {
    key.split(' ').filter(|w| !w.is_empty()).collect()
}

fn key_numbers<'a>(words: &std::collections::BTreeSet<&'a str>) -> Vec<&'a str> {
    words
        .iter()
        .filter(|w| w.chars().all(|c| c.is_ascii_digit()))
        .copied()
        .collect()
}

/// canvas > syllabus, for a card both readers can propose.
///
/// Canvas returns the assignment's own `due_at`; a syllabus scan returns a
/// model's reading of prose about it, which routinely has the day and rarely
/// the hour. Where they disagree the first is what the course committed to.
fn source_rank(source: &str) -> u8 {
    match source {
        "canvas" => 2,
        _ => 1,
    }
}

/// The one path a proposed deadline takes into the queue, whatever proposed it.
///
/// Both producers — the syllabus scan reading a PDF and the Canvas sync reading
/// assignments — land here, so the deduplication rules cannot drift apart
/// between them. Identity is the Canvas assignment id where the producer has
/// one, and (title, calendar day) otherwise: the same item proposed from both
/// sources is one card, not two, and an assignment whose due date moved is the
/// same card on a new day rather than a second one.
pub(crate) fn record_proposal(
    conn: &Connection,
    class_id: i64,
    title: &str,
    kind: &str,
    due_at: &str,
    notes: Option<&str>,
    source: &str,
    canvas_id: Option<&str>,
) -> Result<Recorded> {
    // The untrusted boundary for both producers: a Canvas assignment title is
    // as unbounded as a model-written one, and a stored title feeds the next
    // scan's prompt.
    let title = &truncate(title.trim(), MAX_TITLE_CHARS);
    let notes = notes.map(|n| truncate(n.trim(), MAX_NOTES_CHARS));
    let notes = notes.as_deref().filter(|n| !n.is_empty());
    if title.is_empty() {
        bail!("a proposed deadline needs a title");
    }
    if !valid_due_at(due_at) {
        bail!("due_at must be ISO — YYYY-MM-DD or YYYY-MM-DDTHH:MM, got '{due_at}'");
    }

    if let Some(canvas_id) = canvas_id {
        let on_list: i64 = conn.query_row(
            "SELECT COUNT(*) FROM deadlines WHERE class_id = ?1 AND canvas_assignment_id = ?2",
            params![class_id, canvas_id],
            |row| row.get(0),
        )?;
        if on_list > 0 {
            return Ok(Recorded::AlreadyDeadline);
        }
        let card: Option<(i64, String, String)> = conn
            .query_row(
                "SELECT id, status, source FROM deadline_proposals
                 WHERE class_id = ?1 AND canvas_assignment_id = ?2",
                params![class_id, canvas_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        match card
            .as_ref()
            .map(|(id, status, held)| (*id, status.as_str(), held.as_str()))
        {
            Some((_, "dismissed", _)) => return Ok(Recorded::DismissedBefore),
            // The card is waiting: the current reading of the assignment
            // replaces the last one, title and day included, since the id is
            // what says they are the same item — under the same rank rule the
            // (title, day) path keeps below, so the guard lives in both.
            Some((_, "pending", held)) if source_rank(source) < source_rank(held) => {
                return Ok(Recorded::Refreshed);
            }
            Some((id, "pending", _)) => {
                conn.execute(
                    "UPDATE deadline_proposals
                     SET title = ?1, kind = ?2, due_at = ?3, notes = ?4, created_at = ?5,
                         source = ?6
                     WHERE id = ?7",
                    params![title, kind, due_at, notes, now(), source, id],
                )?;
                return Ok(Recorded::Refreshed);
            }
            // Approved once, and the deadline it made is gone — deleted by
            // hand. The row is reused as a fresh card rather than a second row
            // under the same id, which one row per (class, assignment) forbids.
            Some((id, _, _)) => {
                conn.execute(
                    "UPDATE deadline_proposals
                     SET title = ?1, kind = ?2, due_at = ?3, notes = ?4, created_at = ?5,
                         source = ?6, status = 'pending', resolved_at = NULL
                     WHERE id = ?7",
                    params![title, kind, due_at, notes, now(), source, id],
                )?;
                return Ok(Recorded::Proposed);
            }
            None => {}
        }
    }

    // The (title, calendar day) rules. With a Canvas id in hand a row that
    // already carries a *different* id is a different assignment that happens
    // to share a title and a day, not this one; without an id every row
    // matches, so a syllabus rescan still recognizes a Canvas-linked deadline.
    let existing: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM deadlines
             WHERE class_id = ?1 AND {SAME_TITLE_AND_DAY} AND {NOT_ANOTHER_ASSIGNMENT}"
        ),
        params![class_id, title, due_at, canvas_id],
        |row| row.get(0),
    )?;
    if existing > 0 {
        return Ok(Recorded::AlreadyDeadline);
    }
    // A reader without an id — the syllabus scan — proposing what the list
    // already holds under Canvas's words and Canvas's day, or what a card was
    // declined for under its own earlier words (SPEC §7.2): the same
    // assignment by the same reading the fold uses, not a card beside it.
    if canvas_id.is_none() {
        let names = |sql: &str| -> Result<bool> {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt
                .query_map([class_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows.into_iter().any(|(t, d)| same_assignment(title, due_at, &t, &d)))
        };
        if names("SELECT title, due_at FROM deadlines WHERE class_id = ?1")? {
            return Ok(Recorded::AlreadyDeadline);
        }
        if names(
            "SELECT title, due_at FROM deadline_proposals
             WHERE class_id = ?1 AND status = 'dismissed'",
        )? {
            return Ok(Recorded::DismissedBefore);
        }
    }
    let dismissed_before: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM deadline_proposals
             WHERE class_id = ?1 AND {SAME_TITLE_AND_DAY} AND status = 'dismissed'
               AND {NOT_ANOTHER_ASSIGNMENT}"
        ),
        params![class_id, title, due_at, canvas_id],
        |row| row.get(0),
    )?;
    if dismissed_before > 0 {
        return Ok(Recorded::DismissedBefore);
    }
    let pending: Option<(i64, String)> = conn
        .query_row(
            &format!(
                "SELECT id, source FROM deadline_proposals
                 WHERE class_id = ?1 AND {SAME_TITLE_AND_DAY} AND status = 'pending'
                   AND {NOT_ANOTHER_ASSIGNMENT}"
            ),
            params![class_id, title, due_at, canvas_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, held)) = pending {
        // The card is already waiting; the only question is whether this reader
        // knows better than the one that put it there. Canvas reads the
        // assignment's own due date and a syllabus scan reads prose about it,
        // so a rescan must not overwrite a stated time with a bare date, or
        // relabel a card as something a PDF said. Same rule `units::rank`
        // keeps, for the same reason.
        if source_rank(source) < source_rank(&held) {
            return Ok(Recorded::Refreshed);
        }
        // A card the syllabus put there and Canvas now recognizes takes the
        // id, so the next sync finds it by identity rather than by name.
        conn.execute(
            "UPDATE deadline_proposals
             SET kind = ?1, due_at = ?2, notes = ?3, created_at = ?4, source = ?5,
                 canvas_assignment_id = COALESCE(?6, canvas_assignment_id)
             WHERE id = ?7",
            params![kind, due_at, notes, now(), source, canvas_id, id],
        )?;
        return Ok(Recorded::Refreshed);
    }
    conn.execute(
        "INSERT INTO deadline_proposals
         (class_id, title, kind, due_at, notes, status, created_at, source, canvas_assignment_id)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7, ?8)",
        params![class_id, title, kind, due_at, notes, now(), source, canvas_id],
    )?;
    Ok(Recorded::Proposed)
}

// ---------------------------------------------------------------------------
// Deadlines Canvas tracks (SPEC §11)

/// A Canvas assignment as the sync reads it, for the deadline it corresponds to.
pub(crate) struct CanvasAssignment<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// Local wall-clock ISO, already converted (canvas_sync::local_iso). An
    /// assignment Canvas dates nothing for has none, and a deadline tracked by
    /// id is still closed by its submission.
    pub due_at: Option<&'a str>,
    /// When the reader handed it in, local wall-clock ISO.
    pub submitted_at: Option<&'a str>,
}

/// What settling a Canvas-tracked deadline changed.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Settled {
    pub due_moved: bool,
    pub completed: bool,
    /// A card waiting for the assignment left the queue with the settle,
    /// which the sync's `deadlineProposals` push has to follow.
    pub card_resolved: bool,
    /// The titles of the untracked rows that named this assignment beside
    /// the tracked one — the syllabus's reading of it — and were folded in.
    pub merged: Vec<String>,
}

/// Brings the deadline for a Canvas assignment up to date, if there is one.
///
/// The deadline is found by its assignment id, or — once — by (title, calendar
/// day) among rows that carry no id, which is how a deadline the syllabus scan
/// proposed before this existed becomes the same item Canvas reports on; it
/// takes the id then and is found by it after. Canvas's due date replaces the
/// row's, since the assignment's own `due_at` outranks a reading of prose
/// about it, and a submission closes the deadline with an audit row naming it.
/// Nothing reopens: a deadline done by hand stays done, and a submission
/// Canvas later un-submits is still a decision the reader made.
///
/// `None` when no deadline corresponds — the caller proposes one instead.
pub(crate) fn settle_canvas_deadline(
    conn: &Connection,
    class_id: i64,
    assignment: &CanvasAssignment,
) -> Result<Option<Settled>> {
    type Row = (i64, String, String, String, Option<String>, String);
    let read = |row: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?))
    };
    let title = truncate(assignment.title.trim(), MAX_TITLE_CHARS);
    let canvas_due = assignment.due_at.filter(|d| valid_due_at(d));
    if title.is_empty() {
        return Ok(None);
    }
    let by_id: Option<Row> = conn
        .query_row(
            "SELECT id, title, due_at, status, canvas_assignment_id, source FROM deadlines
             WHERE class_id = ?1 AND canvas_assignment_id = ?2",
            params![class_id, assignment.id],
            read,
        )
        .optional()?;
    // One transaction from the link on: taking the id is an ownership
    // transfer — from here the sync moves the row's date and can close it —
    // so it lands with its own audit row and never without the move that
    // follows it.
    let tx = conn.unchecked_transaction()?;
    // The class's rows no assignment tracks, an open one before a done one:
    // what a syllabus scan or a hand put on the list, and what this
    // assignment may be the Canvas reading of.
    let untracked = |tx: &Connection| -> rusqlite::Result<Vec<Row>> {
        let mut stmt = tx.prepare(
            "SELECT id, title, due_at, status, canvas_assignment_id, source FROM deadlines
             WHERE class_id = ?1 AND canvas_assignment_id IS NULL
             ORDER BY (status = 'open') DESC, id",
        )?;
        let rows = stmt.query_map([class_id], read)?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    };
    let row = match (by_id, canvas_due) {
        (Some(row), _) => row,
        // Without a day there is no reading to match a legacy row on.
        (None, None) => return Ok(None),
        (None, Some(canvas_due)) => {
            // The first untracked row that names this assignment (SPEC §7.2):
            // the open one is the one a submission has something to close.
            let legacy = untracked(&tx)?
                .into_iter()
                .find(|r| names_assignment(&r.5, &title, canvas_due, &r.1, &r.2));
            let Some(row) = legacy else {
                return Ok(None);
            };
            tx.execute(
                "UPDATE deadlines SET canvas_assignment_id = ?1 WHERE id = ?2",
                params![assignment.id, row.0],
            )?;
            audit(
                &tx,
                "canvas.link_deadline",
                json!({ "id": row.0, "classId": class_id, "canvasAssignmentId": assignment.id,
                        "title": row.1, "dueAt": row.2, "status": row.3 }),
            )?;
            row
        }
    };
    let (id, row_title, due_at, status, _, _) = row;
    // A card waiting for an assignment the list now tracks leaves the queue
    // with it: the deadline is on the list, so the card has nothing to ask.
    let cards = tx.execute(
        "UPDATE deadline_proposals SET status = 'approved', resolved_at = ?1
         WHERE class_id = ?2 AND canvas_assignment_id = ?3 AND status = 'pending'",
        params![now(), class_id, assignment.id],
    )?;

    let mut settled = Settled { card_resolved: cards > 0, ..Settled::default() };
    // Canvas's own words for the assignment and its own date: the row follows
    // both, and one audit row carries whatever moved.
    let mut before = serde_json::Map::new();
    let mut after = serde_json::Map::new();
    if row_title != title {
        tx.execute("UPDATE deadlines SET title = ?1 WHERE id = ?2", params![title, id])?;
        before.insert("title".into(), json!(row_title));
        after.insert("title".into(), json!(title));
    }
    if let Some(canvas_due) = canvas_due.filter(|d| *d != due_at) {
        tx.execute(
            "UPDATE deadlines SET due_at = ?1 WHERE id = ?2",
            params![canvas_due, id],
        )?;
        before.insert("dueAt".into(), json!(due_at));
        after.insert("dueAt".into(), json!(canvas_due));
        settled.due_moved = true;
    }
    if !before.is_empty() {
        audit(
            &tx,
            "canvas.update_deadline",
            json!({ "id": id, "classId": class_id, "canvasAssignmentId": assignment.id,
                    "title": title, "before": before, "after": after }),
        )?;
    }
    if let Some(submitted_at) = assignment.submitted_at.filter(|_| status == "open") {
        tx.execute("UPDATE deadlines SET status = 'done' WHERE id = ?1", [id])?;
        audit(
            &tx,
            "canvas.complete_deadline",
            json!({ "id": id, "classId": class_id, "canvasAssignmentId": assignment.id,
                    "title": title, "submittedAt": submitted_at }),
        )?;
        settled.completed = true;
    }
    // Any other untracked row that names this assignment — the syllabus's
    // reading of it, left beside the tracked row — is folded in.
    let due_now = canvas_due.unwrap_or(due_at.as_str());
    settled.merged = fold_into(&tx, class_id, id, &title, due_now, assignment.id)?;
    tx.commit()?;
    Ok(Some(settled))
}

/// Folds every untracked row of the class that names the tracked row's
/// assignment into it (SPEC §7.2): their notes carried onto the row where
/// they add anything, their own rows removed, each fold audited. Not
/// reversible, as no `canvas.*` row is: the next sync would fold it again.
/// Answers with the folded titles.
fn fold_into(
    tx: &Connection,
    class_id: i64,
    kept_id: i64,
    kept_title: &str,
    kept_due: &str,
    canvas_id: &str,
) -> Result<Vec<String>> {
    let mut stmt = tx.prepare(
        "SELECT id, title, due_at, status, kind, notes, source FROM deadlines
         WHERE class_id = ?1 AND canvas_assignment_id IS NULL AND id != ?2
         ORDER BY id",
    )?;
    type Dup = (i64, String, String, String, String, Option<String>, String);
    let dups: Vec<Dup> = stmt
        .query_map(params![class_id, kept_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut folded = Vec::new();
    for (dup_id, dup_title, dup_due, dup_status, kind, notes, source) in dups {
        if !names_assignment(&source, kept_title, kept_due, &dup_title, &dup_due) {
            continue;
        }
        if let Some(theirs) = notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            let mine: Option<String> =
                tx.query_row("SELECT notes FROM deadlines WHERE id = ?1", [kept_id], |r| r.get(0))?;
            let kept = match mine.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                Some(mine) if mine.contains(theirs) => None,
                Some(mine) => Some(format!("{mine} · {theirs}")),
                None => Some(theirs.to_string()),
            };
            if let Some(kept) = kept {
                tx.execute("UPDATE deadlines SET notes = ?1 WHERE id = ?2", params![kept, kept_id])?;
            }
        }
        tx.execute("DELETE FROM deadlines WHERE id = ?1", [dup_id])?;
        audit(
            tx,
            "canvas.merge_deadline",
            json!({ "id": dup_id, "classId": class_id, "mergedInto": kept_id,
                    "canvasAssignmentId": canvas_id, "title": dup_title, "kind": kind,
                    "dueAt": dup_due, "notes": notes, "status": dup_status, "source": source }),
        )?;
        folded.push(dup_title);
    }
    Ok(folded)
}

/// One fold a launch made: whose class, what was folded, into what.
pub struct Fold {
    pub class_id: i64,
    pub theirs: String,
    pub canvas_title: String,
}

/// Folds, for every tracked deadline the list holds, the untracked rows that
/// name its assignment — what an earlier sync left beside Canvas's row, or a
/// syllabus scan added after it (SPEC §7.2). Needs no Canvas read: the
/// tracked row carries Canvas's words and day. One transaction for the lot.
pub fn fold_duplicates(conn: &Connection) -> Result<Vec<Fold>> {
    let tx = conn.unchecked_transaction()?;
    let mut stmt = tx.prepare(
        "SELECT id, class_id, title, due_at, canvas_assignment_id FROM deadlines
         WHERE canvas_assignment_id IS NOT NULL ORDER BY class_id, id",
    )?;
    let tracked: Vec<(i64, i64, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    let mut folds = Vec::new();
    for (id, class_id, title, due_at, canvas_id) in tracked {
        for theirs in fold_into(&tx, class_id, id, &title, &due_at, &canvas_id)? {
            folds.push(Fold { class_id, theirs, canvas_title: title.clone() });
        }
    }
    tx.commit()?;
    Ok(folds)
}

/// The launch's fold (SPEC §7.2): what it folded is said per class on the
/// window, with nothing to undo, and the lists refetch.
pub fn fold_at_launch(app: &AppHandle) {
    let folds = match with_conn(app, fold_duplicates) {
        Ok(folds) => folds,
        Err(e) => {
            eprintln!("deadlines: the launch fold failed — {e:#}");
            return;
        }
    };
    if folds.is_empty() {
        return;
    }
    let mut by_class: std::collections::BTreeMap<i64, Vec<&Fold>> = std::collections::BTreeMap::new();
    for fold in &folds {
        by_class.entry(fold.class_id).or_default().push(fold);
    }
    for (class_id, folds) in by_class {
        let text = match folds.as_slice() {
            [one] => format!("Merged {} into {} from Canvas", one.theirs, one.canvas_title),
            many => format!("Merged {} syllabus deadlines into their Canvas rows", many.len()),
        };
        eprintln!("deadlines: {text} (class {class_id})");
        notify(app, text, Vec::new(), Some(class_id));
    }
    emit_hub_change(app, "deadlines");
}

// ---------------------------------------------------------------------------
// Resolution: the confirm cards

/// Approve inserts the deadline (source='syllabus') and audits it; dismiss
/// parks the row — either way the card leaves the queue.
pub fn resolve_proposal(app: &AppHandle, proposal_id: i64, approve: bool) -> Result<String> {
    let resolved = with_conn(app, |conn| resolve_in_conn(conn, proposal_id, approve))?;
    emit_hub_change(app, "deadlineProposals");
    if approve {
        emit_hub_change(app, "deadlines");
        notify(
            app,
            format!("Added {}", resolved.title),
            resolved.audit_id.into_iter().collect(),
            Some(resolved.class_id),
        );
    }
    Ok(resolved.summary)
}

/// What a batch did: which cards can leave the queue — approved, or
/// dismissed by a series' Skip — and one line per card that could not be
/// resolved (its proposal row stays as it was).
#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BatchOutcome {
    pub approved: Vec<i64>,
    pub skipped: Vec<String>,
}

/// The Add all button and a series card's Add the series: approves each
/// proposal independently — one rejection (e.g. an identical deadline added
/// by hand since the scan) costs that card, never the rest — and pushes
/// hub-changed once at the end instead of per card. Each approval keeps its
/// own transaction inside resolve_in_conn; the batch is one notice with one
/// `Undo` over every row it wrote.
pub fn approve_proposals(app: &AppHandle, proposal_ids: &[i64]) -> Result<BatchOutcome> {
    if proposal_ids.is_empty() {
        bail!("no proposals to approve");
    }
    let batch = format!("deadlines-{}", now());
    let (outcome, audit_ids, titles, class_id) = with_conn(app, |conn| {
        let mut approved = Vec::new();
        let mut skipped = Vec::new();
        let mut audit_ids = Vec::new();
        let mut titles = Vec::new();
        let mut class_id = None;
        for &id in proposal_ids {
            match resolve_with_batch(conn, id, Some(&batch)) {
                Ok(resolved) => {
                    approved.push(id);
                    audit_ids.extend(resolved.audit_id);
                    titles.push(resolved.title);
                    class_id = Some(resolved.class_id);
                }
                Err(e) => skipped.push(format!("{e:#}")),
            }
        }
        Ok((BatchOutcome { approved, skipped }, audit_ids, titles, class_id))
    })?;
    emit_hub_change(app, "deadlineProposals");
    if !outcome.approved.is_empty() {
        emit_hub_change(app, "deadlines");
        let text = match titles.as_slice() {
            [one] => format!("Added {one}"),
            many => {
                let stems: std::collections::BTreeSet<String> =
                    many.iter().map(|t| title_stem(t)).collect();
                match stems.len() {
                    1 => format!("Added {} dates of {}", many.len(), stems.iter().next().unwrap()),
                    _ => format!("Added {} deadlines", many.len()),
                }
            }
        };
        notify(app, text, audit_ids, class_id);
    }
    Ok(outcome)
}

/// A series card's Skip: every member card dismissed, one hub push. A card
/// that cannot be — resolved from another window since — costs itself and
/// is named, never the rest, the way `approve_proposals` treats a batch;
/// the queue is refetched whatever happened, so the card reads what stands.
pub fn dismiss_proposals(app: &AppHandle, proposal_ids: &[i64]) -> Result<BatchOutcome> {
    let outcome = with_conn(app, |conn| Ok(dismiss_in_conn(conn, proposal_ids)))?;
    emit_hub_change(app, "deadlineProposals");
    Ok(outcome)
}

fn dismiss_in_conn(conn: &Connection, proposal_ids: &[i64]) -> BatchOutcome {
    let mut outcome = BatchOutcome { approved: Vec::new(), skipped: Vec::new() };
    for &id in proposal_ids {
        match resolve_in_conn(conn, id, false) {
            Ok(_) => outcome.approved.push(id),
            Err(e) => outcome.skipped.push(format!("{e:#}")),
        }
    }
    outcome
}

/// What resolving one card did.
#[derive(Debug)]
struct Resolved {
    summary: String,
    title: String,
    class_id: i64,
    /// The `<source>.insert_deadline` row an approval wrote; none on a skip.
    audit_id: Option<i64>,
}

fn resolve_in_conn(conn: &Connection, proposal_id: i64, approve: bool) -> Result<Resolved> {
    resolve_with_batch(conn, proposal_id, approve.then_some(""))
}

/// `batch` is `Some` for an approval — empty for a card approved on its own,
/// the batch's id for one of several — and `None` for a skip.
fn resolve_with_batch(conn: &Connection, proposal_id: i64, batch: Option<&str>) -> Result<Resolved> {
    let approve = batch.is_some();
    let row = conn
        .query_row(
            "SELECT class_id, title, kind, due_at, notes, status, source, canvas_assignment_id
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
                    r.get::<_, String>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?
        .context("proposal not found")?;
    let (class_id, title, kind, due_at, notes, status, source, canvas_id) = row;
    if status != "pending" {
        bail!("this proposal was already resolved");
    }

    if !approve {
        conn.execute(
            "UPDATE deadline_proposals SET status = 'dismissed', resolved_at = ?1
             WHERE id = ?2",
            params![now(), proposal_id],
        )?;
        return Ok(Resolved {
            summary: format!("skipped — {title}"),
            title,
            class_id,
            audit_id: None,
        });
    }

    // Insert + audit + status flip land as one unit — interrupted midway
    // they would leave a deadline behind a still-pending card.
    let tx = conn.unchecked_transaction()?;
    // A matching deadline may have appeared since the scan (added by hand
    // or through chat) — approving would silently duplicate it.
    let existing: i64 = tx.query_row(
        &format!("SELECT COUNT(*) FROM deadlines WHERE class_id = ?1 AND {SAME_TITLE_AND_DAY}"),
        params![class_id, title, due_at],
        |row| row.get(0),
    )?;
    if existing > 0 {
        bail!("'{title}' is already recorded for that date — skip this card instead");
    }
    // The same assignment may already be on the list under another title — a
    // syllabus row the sync linked by day — and one row per assignment is what
    // lets a submission find the deadline it closes.
    let linked: i64 = tx.query_row(
        "SELECT COUNT(*) FROM deadlines
         WHERE class_id = ?1 AND canvas_assignment_id IS NOT NULL AND canvas_assignment_id = ?2",
        params![class_id, canvas_id],
        |row| row.get(0),
    )?;
    if linked > 0 {
        bail!("'{title}' is already on the list as its Canvas assignment — skip this card instead");
    }
    // The deadline records which reader proposed it, so a due date that turns
    // out to be wrong can be traced to the syllabus PDF or to Canvas, and the
    // Canvas id that lets a submission close it.
    tx.execute(
        "INSERT INTO deadlines
         (class_id, title, kind, due_at, notes, status, source, canvas_assignment_id)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6, ?7)",
        params![class_id, title, kind, due_at, notes, source, canvas_id],
    )?;
    let audit_id = audit(
        &tx,
        &format!("{source}.insert_deadline"),
        json!({ "proposalId": proposal_id, "deadlineId": tx.last_insert_rowid(),
                "classId": class_id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes, "canvasAssignmentId": canvas_id,
                "batch": batch.filter(|b| !b.is_empty()) }),
    )?;
    tx.execute(
        "UPDATE deadline_proposals SET status = 'approved', resolved_at = ?1
         WHERE id = ?2",
        params![now(), proposal_id],
    )?;
    tx.commit()?;
    Ok(Resolved {
        summary: format!("added — {title} due {due_at}"),
        title,
        class_id,
        audit_id: Some(audit_id),
    })
}

// ---------------------------------------------------------------------------
// Canvas assignments are deadlines (SPEC §7.2): the direct write

/// What the sync's direct write did for one assignment.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DirectWrite {
    /// A deadline now on the list, done at once when Canvas holds a submission.
    Recorded { completed: bool },
    /// A Canvas card for it was declined earlier; the decision stands.
    DeclinedBefore,
}

/// A dated assignment no deadline tracks becomes its deadline directly —
/// source `canvas`, keyed on the assignment id — with `canvas.insert_deadline`
/// as its audit row, and done at once where Canvas holds a submission. A
/// Canvas card waiting for it is resolved as approved; one declined earlier
/// keeps the assignment off the list, since that was a decision. The claims
/// `settle_canvas_deadline` makes — by id, then by title and day — run before
/// this, so the row arrives here only when nothing tracks the assignment.
pub(crate) fn insert_canvas_deadline(
    conn: &Connection,
    class_id: i64,
    assignment: &CanvasAssignment,
    kind: &str,
    notes: &str,
) -> Result<DirectWrite> {
    let title = truncate(assignment.title.trim(), MAX_TITLE_CHARS);
    let due_at = assignment.due_at.context("an undated assignment is not a deadline")?;
    if !valid_due_at(due_at) {
        bail!("due_at must be ISO — YYYY-MM-DD or YYYY-MM-DDTHH:MM, got '{due_at}'");
    }
    let card: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, status FROM deadline_proposals
             WHERE class_id = ?1 AND canvas_assignment_id = ?2",
            params![class_id, assignment.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if matches!(card.as_ref(), Some((_, status)) if status == "dismissed") {
        return Ok(DirectWrite::DeclinedBefore);
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO deadlines
         (class_id, title, kind, due_at, notes, status, source, canvas_assignment_id)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', 'canvas', ?6)",
        params![class_id, title, kind, due_at, notes, assignment.id],
    )?;
    let id = tx.last_insert_rowid();
    audit(
        &tx,
        "canvas.insert_deadline",
        json!({ "deadlineId": id, "classId": class_id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes, "canvasAssignmentId": assignment.id,
                "proposalId": card.as_ref().map(|(id, _)| *id) }),
    )?;
    if let Some((proposal_id, _)) = card {
        tx.execute(
            "UPDATE deadline_proposals SET status = 'approved', resolved_at = ?1 WHERE id = ?2",
            params![now(), proposal_id],
        )?;
    }
    let completed = match assignment.submitted_at {
        Some(submitted_at) => {
            tx.execute("UPDATE deadlines SET status = 'done' WHERE id = ?1", [id])?;
            audit(
                &tx,
                "canvas.complete_deadline",
                json!({ "id": id, "classId": class_id, "canvasAssignmentId": assignment.id,
                        "title": title, "submittedAt": submitted_at }),
            )?;
            true
        }
        None => false,
    };
    tx.commit()?;
    Ok(DirectWrite::Recorded { completed })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real schema, classes included — `0001_init.sql` seeds them.
    fn db() -> rusqlite::Connection {
        crate::db::memory_db()
    }

    fn deadline(
        conn: &rusqlite::Connection,
        class_id: i64,
        title: &str,
        due_at: &str,
        source: &str,
        notes: Option<&str>,
    ) -> i64 {
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
             VALUES (?1, ?2, 'assignment', ?3, ?4, 'open', ?5)",
            params![class_id, title, due_at, notes, source],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// One assignment under two readers' words (SPEC §7.2): the syllabus's
    /// `Homework 1` on the Sunday and Canvas's `Homework Assignment 1` on the
    /// Monday at 11:59 pm are one; another number, another kind or a week
    /// apart are not.
    #[test]
    fn two_readings_name_one_assignment_by_key_and_day() {
        assert!(same_assignment("Homework Assignment 1", "2026-09-14T23:59", "Homework 1", "2026-09-13"));
        assert!(same_assignment("HW #1", "2026-09-14", "Homework 1", "2026-09-16"));
        assert!(same_assignment("Assignment 1", "2026-09-14", "Homework 1", "2026-09-14"));
        assert!(!same_assignment("Homework 2", "2026-09-14", "Homework 1", "2026-09-14"));
        assert!(!same_assignment("Quiz 1", "2026-09-14", "Homework 1", "2026-09-14"));
        assert!(!same_assignment("Homework 1", "2026-09-21", "Homework 1", "2026-09-14"), "a week apart");
        assert!(!same_assignment("Homework 1", "soon", "Homework 1", "2026-09-14"));
        assert_eq!(assignment_key("Homework Assignment #1"), "homework 1");
        assert_eq!(assignment_key("Problem Set 2"), "problem set 2");
        // Keys that differ but share most of their words on one day are one
        // deliverable named twice; a different number never is.
        assert!(same_assignment("Problem Statement and AI Sketch", "2026-09-09T23:59", "Problem Statement + AI Pitch", "2026-09-09"));
        assert!(!same_assignment("Homework 1 Draft", "2026-09-14", "Homework 2", "2026-09-14"));
        assert!(!same_assignment("AI Design Project Presentations", "2026-10-28", "AI Teaming Log", "2026-10-28"));
    }

    /// The launch's fold needs no Canvas read: every tracked row folds the
    /// untracked rows that name its assignment, across classes, in one
    /// transaction, and names each fold by class.
    #[test]
    fn the_launch_fold_covers_every_tracked_row_across_classes() {
        let conn = db();
        let hw = deadline(&conn, 3, "Homework Assignment 1", "2026-09-14T23:59", "canvas", None);
        conn.execute("UPDATE deadlines SET canvas_assignment_id = '7317246' WHERE id = ?1", [hw]).unwrap();
        deadline(&conn, 3, "Homework 1", "2026-09-13", "syllabus", None);
        let sketch = deadline(&conn, 2, "Problem Statement and AI Sketch", "2026-09-09T23:59", "canvas", None);
        conn.execute("UPDATE deadlines SET canvas_assignment_id = '7300444' WHERE id = ?1", [sketch]).unwrap();
        deadline(&conn, 2, "Problem Statement + AI Pitch", "2026-09-09", "syllabus", None);
        let stays = deadline(&conn, 2, "AI Solution Architecture", "2026-09-16", "syllabus", None);
        let folds = fold_duplicates(&conn).unwrap();
        let named: Vec<(i64, &str, &str)> = folds
            .iter()
            .map(|f| (f.class_id, f.theirs.as_str(), f.canvas_title.as_str()))
            .collect();
        assert_eq!(
            named,
            vec![
                (2, "Problem Statement + AI Pitch", "Problem Statement and AI Sketch"),
                (3, "Homework 1", "Homework Assignment 1"),
            ]
        );
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM deadlines", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 3);
        let kept: i64 = conn.query_row("SELECT COUNT(*) FROM deadlines WHERE id = ?1", [stays], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1);
        assert!(fold_duplicates(&conn).unwrap().is_empty(), "a second launch folds nothing");
    }

    /// A syllabus row is the Canvas assignment's on first contact by that
    /// rule, and follows Canvas's words and day from then on.
    #[test]
    fn a_settle_links_the_syllabus_reading_and_takes_canvas_words() {
        let conn = db();
        let id = deadline(&conn, 1, "Homework 1", "2026-09-13", "syllabus", Some("Chapter 3 problems"));
        let assignment = CanvasAssignment {
            id: "77",
            title: "Homework Assignment 1",
            due_at: Some("2026-09-14T23:59"),
            submitted_at: None,
        };
        let settled = settle_canvas_deadline(&conn, 1, &assignment).unwrap().expect("linked");
        assert!(settled.due_moved);
        assert!(settled.merged.is_empty());
        let (title, due, canvas_id): (String, String, Option<String>) = conn
            .query_row(
                "SELECT title, due_at, canvas_assignment_id FROM deadlines WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (title.as_str(), due.as_str(), canvas_id.as_deref()),
            ("Homework Assignment 1", "2026-09-14T23:59", Some("77"))
        );
        let update: String = conn
            .query_row(
                "SELECT payload FROM audit_log WHERE action = 'canvas.update_deadline' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(update.contains("\"title\":\"Homework 1\"") && update.contains("\"dueAt\":\"2026-09-13\""), "{update}");
    }

    /// A syllabus reading left beside a tracked row is folded into it on the
    /// next sync — its notes carried, its row gone, the fold audited and named
    /// — and a second sync folds nothing. `Homework 2` is another assignment
    /// and stays.
    #[test]
    fn a_settle_folds_the_syllabus_duplicate_into_the_tracked_row() {
        let conn = db();
        let canvas = deadline(&conn, 1, "Homework Assignment 1", "2026-09-14T23:59", "canvas", Some("From Canvas · 10 points"));
        conn.execute("UPDATE deadlines SET canvas_assignment_id = '77' WHERE id = ?1", [canvas]).unwrap();
        let dup = deadline(&conn, 1, "Homework 1", "2026-09-13", "syllabus", Some("Chapter 3 problems"));
        let other = deadline(&conn, 1, "Homework 2", "2026-10-04", "syllabus", None);
        let assignment = CanvasAssignment {
            id: "77",
            title: "Homework Assignment 1",
            due_at: Some("2026-09-14T23:59"),
            submitted_at: None,
        };
        let settled = settle_canvas_deadline(&conn, 1, &assignment).unwrap().expect("tracked");
        assert_eq!(settled.merged, vec!["Homework 1".to_string()]);
        let count = |id: i64| -> i64 {
            conn.query_row("SELECT COUNT(*) FROM deadlines WHERE id = ?1", [id], |r| r.get(0)).unwrap()
        };
        assert_eq!((count(dup), count(other), count(canvas)), (0, 1, 1));
        let notes: String = conn
            .query_row("SELECT notes FROM deadlines WHERE id = ?1", [canvas], |r| r.get(0))
            .unwrap();
        assert_eq!(notes, "From Canvas · 10 points · Chapter 3 problems");
        let folds: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log WHERE action = 'canvas.merge_deadline'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(folds, 1);
        let again = settle_canvas_deadline(&conn, 1, &assignment).unwrap().expect("tracked");
        assert!(again.merged.is_empty());
    }

    /// The looser reading is the syllabus's alone: a row the owner typed two
    /// days before Canvas's assignment is a marker of their own and stays,
    /// unlinked and unfolded, while one typed with the same key on the same
    /// day is the assignment and links as it did before.
    #[test]
    fn a_hand_typed_row_links_only_on_the_same_key_and_day() {
        let conn = db();
        let marker = deadline(&conn, 1, "Homework 3", "2026-09-14", "manual", Some("start early"));
        let assignment = CanvasAssignment {
            id: "78",
            title: "Assignment 3",
            due_at: Some("2026-09-16T23:59"),
            submitted_at: None,
        };
        assert!(settle_canvas_deadline(&conn, 1, &assignment).unwrap().is_none(), "not linked");
        let canvas = deadline(&conn, 1, "Assignment 3", "2026-09-16T23:59", "canvas", None);
        conn.execute("UPDATE deadlines SET canvas_assignment_id = '78' WHERE id = ?1", [canvas]).unwrap();
        let settled = settle_canvas_deadline(&conn, 1, &assignment).unwrap().expect("tracked");
        assert!(settled.merged.is_empty(), "the marker is not folded");
        let kept: i64 = conn.query_row("SELECT COUNT(*) FROM deadlines WHERE id = ?1", [marker], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1);
        let same_day = deadline(&conn, 1, "HW 4", "2026-09-23", "manual", None);
        let four = CanvasAssignment {
            id: "79",
            title: "Homework #4",
            due_at: Some("2026-09-23T23:59"),
            submitted_at: None,
        };
        settle_canvas_deadline(&conn, 1, &four).unwrap().expect("linked on the same key and day");
        let linked: Option<String> = conn
            .query_row("SELECT canvas_assignment_id FROM deadlines WHERE id = ?1", [same_day], |r| r.get(0))
            .unwrap();
        assert_eq!(linked.as_deref(), Some("79"));
    }

    /// A syllabus rescan proposing what Canvas already tracks under its own
    /// words is told the deadline exists rather than given a card.
    #[test]
    fn a_syllabus_proposal_for_a_tracked_assignment_is_already_a_deadline() {
        let conn = db();
        let canvas = deadline(&conn, 1, "Homework Assignment 1", "2026-09-14T23:59", "canvas", None);
        conn.execute("UPDATE deadlines SET canvas_assignment_id = '77' WHERE id = ?1", [canvas]).unwrap();
        let recorded = record_proposal(&conn, 1, "Homework 1", "assignment", "2026-09-13", None, "syllabus", None).unwrap();
        assert!(matches!(recorded, Recorded::AlreadyDeadline));
        let recorded = record_proposal(&conn, 1, "Homework 2", "assignment", "2026-10-04", None, "syllabus", None).unwrap();
        assert!(matches!(recorded, Recorded::Proposed));
        // A card declined as `Homework 2` covers the scan's next reading of
        // it, `HW #2` a day later, by the same rule.
        let card: i64 = conn.query_row("SELECT id FROM deadline_proposals", [], |r| r.get(0)).unwrap();
        conn.execute(
            "UPDATE deadline_proposals SET status = 'dismissed', resolved_at = 1 WHERE id = ?1",
            [card],
        )
        .unwrap();
        let recorded = record_proposal(&conn, 1, "HW #2", "assignment", "2026-10-05", None, "syllabus", None).unwrap();
        assert!(matches!(recorded, Recorded::DismissedBefore));
    }

    fn propose(conn: &rusqlite::Connection, due_at: &str, source: &str) -> Recorded {
        record_proposal(conn, 1, "Problem Set 2", "assignment", due_at, None, source, None)
            .expect("record")
    }

    fn stored(conn: &rusqlite::Connection) -> (String, String) {
        conn.query_row(
            "SELECT due_at, source FROM deadline_proposals WHERE class_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("one row")
    }

    /// Both readers land in one queue, so the same item proposed from both is
    /// one card rather than two.
    #[test]
    fn a_second_reading_of_the_same_item_refreshes_one_card() {
        let conn = db();
        assert!(matches!(propose(&conn, "2026-09-03", "syllabus"), Recorded::Proposed));
        assert!(matches!(
            propose(&conn, "2026-09-03T23:59", "canvas"),
            Recorded::Refreshed
        ));
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM deadline_proposals", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 1, "the queue stacked a duplicate card");
    }

    /// Canvas reads the assignment's own due date; a syllabus scan reads prose
    /// about it. A rescan must not replace a stated time with a bare day, nor
    /// relabel the card as something a PDF said.
    #[test]
    fn a_syllabus_rescan_does_not_overwrite_what_canvas_said() {
        let conn = db();
        propose(&conn, "2026-09-03T23:59", "canvas");
        assert!(matches!(
            propose(&conn, "2026-09-03", "syllabus"),
            Recorded::Refreshed
        ));
        assert_eq!(
            stored(&conn),
            ("2026-09-03T23:59".to_string(), "canvas".to_string())
        );
        // Canvas correcting its own earlier reading still lands — identity is
        // (title, calendar day), so this is the same card with a new time.
        propose(&conn, "2026-09-03T09:00", "canvas");
        assert_eq!(
            stored(&conn),
            ("2026-09-03T09:00".to_string(), "canvas".to_string())
        );
    }

    /// The card's PROPOSED badge counts one class's pending rows and nothing
    /// else: not another class's, not a resolved one.
    #[test]
    fn the_badge_counts_only_the_class_s_pending_proposals() {
        let conn = db();
        assert_eq!(pending_count(&conn, 1).unwrap(), 0);
        propose(&conn, "2026-09-07", "syllabus");
        record_proposal(&conn, 1, "Quiz 1", "quiz", "2026-09-03", None, "canvas", Some("7"))
            .unwrap();
        record_proposal(&conn, 2, "Form Teams", "project", "2026-09-02", None, "syllabus", None)
            .unwrap();
        assert_eq!(pending_count(&conn, 1).unwrap(), 2);
        assert_eq!(pending_count(&conn, 2).unwrap(), 1);
        conn.execute(
            "UPDATE deadline_proposals SET status = 'approved' WHERE title = 'Quiz 1'",
            [],
        )
        .unwrap();
        assert_eq!(pending_count(&conn, 1).unwrap(), 1);
    }

    /// A decision already made. A re-scan or a re-sync must not put a declined
    /// card back; adding it by hand is the way back.
    #[test]
    fn a_declined_card_is_not_proposed_again() {
        let conn = db();
        propose(&conn, "2026-09-03", "syllabus");
        conn.execute(
            "UPDATE deadline_proposals SET status = 'dismissed' WHERE class_id = 1",
            [],
        )
        .expect("dismiss");
        assert!(matches!(
            propose(&conn, "2026-09-03", "canvas"),
            Recorded::DismissedBefore
        ));
    }

    #[test]
    fn an_item_already_on_the_list_is_not_proposed() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
             VALUES (1, 'Problem Set 2', 'assignment', '2026-09-03T23:59', 'open', 'manual')",
            [],
        )
        .expect("deadline");
        assert!(matches!(
            propose(&conn, "2026-09-03", "syllabus"),
            Recorded::AlreadyDeadline
        ));
    }

    /// An absent key is a real answer; a key that is present and not a list is
    /// a malformed scan. Collapsing the second to an empty vec reported "no
    /// date-bearing items found" and lost the scan's whole deadline part.
    #[test]
    fn a_malformed_half_fails_the_scan_rather_than_reading_as_empty() {
        let output = split_output(r#"{"deadlines": [{"title": "x"}]}"#).expect("valid shape");
        assert_eq!(output.deadlines.len(), 1);
        assert!(output.units.is_empty(), "an absent key is an empty list");
        assert!(output.grading.is_empty(), "an absent key is an empty list");

        assert!(split_output(r#"{"deadlines": {"title": "x"}, "units": []}"#).is_err());
        assert!(split_output(r#"{"units": "Week 1"}"#).is_err());
        assert!(split_output(r#"{"grading": {"Quizzes": 20}}"#).is_err());
        // A breakdown alone is a valid answer for a syllabus with no dates.
        let output = split_output(r#"{"grading": [{"name": "Quizzes", "weight": 20}]}"#)
            .expect("grading alone");
        assert_eq!(output.grading.len(), 1);
        assert!(output.deadlines.is_empty());
        // A bare array is still the deadline list alone.
        let output = split_output(r#"[{"title": "x"}]"#).expect("bare array");
        assert_eq!(output.deadlines.len(), 1);
        assert!(output.grading.is_empty());
    }

    fn weights(conn: &rusqlite::Connection, class_id: i64) -> Vec<(String, f64, Option<String>)> {
        let mut stmt = conn
            .prepare(
                "SELECT name, weight, canvas_group_id FROM grade_categories
                 WHERE class_id = ?1 ORDER BY id",
            )
            .expect("prepare");
        stmt.query_map([class_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .expect("query")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("rows")
    }

    fn audits(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'syllabus.set_grade_weight'",
            [],
            |row| row.get(0),
        )
        .expect("count")
    }

    /// The Design Studio shape: Canvas supplied the categories at zero, the
    /// syllabus supplies the weights, and one component the class did not
    /// track becomes a category with no Canvas id. A second pass of the same
    /// breakdown writes nothing.
    #[test]
    fn a_scan_fills_zero_weights_and_creates_what_the_class_lacks() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight, canvas_group_id) VALUES
               (2, 'Studio Participation', 0, '1'), (2, 'Quizzes', 0, '2'),
               (2, 'AI Design Project', 0, '3');",
        )
        .expect("fixture");
        let raw = vec![
            json!({ "name": "AI Design Project", "weight": 60 }),
            json!({ "name": "studio participation", "weight": "20%" }),
            json!({ "name": "Peer Design Sessions", "weight": 20 }),
        ];
        let recorded = apply_weights(&conn, 2, &raw).expect("apply");
        assert_eq!(
            recorded.summary(),
            "weights set: AI Design Project 60, studio participation 20, \
             Peer Design Sessions 20 (new) · Quizzes left at 0 — the syllabus does not weight it"
        );
        assert_eq!(
            weights(&conn, 2),
            vec![
                ("Studio Participation".to_string(), 20.0, Some("1".to_string())),
                ("Quizzes".to_string(), 0.0, Some("2".to_string())),
                ("AI Design Project".to_string(), 60.0, Some("3".to_string())),
                ("Peer Design Sessions".to_string(), 20.0, None),
            ]
        );
        assert_eq!(audits(&conn), 3);

        let again = apply_weights(&conn, 2, &raw).expect("again");
        assert!(again.set.is_empty(), "{}", again.summary());
        assert_eq!(again.unchanged, 3);
        assert_eq!(audits(&conn), 3, "a rescan of an unchanged syllabus leaves no row");
        assert_eq!(weights(&conn, 2).len(), 4);
    }

    /// The Biostatistics shape: three weights typed by hand and one Canvas
    /// category at zero. A syllabus agreeing with the typed numbers writes
    /// nothing; one disagreeing changes nothing either and says so.
    #[test]
    fn a_scan_never_replaces_a_weight_already_set() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight, canvas_group_id) VALUES
               (3, 'Assignments', 50, '1'), (3, 'Quizzes', 20, '2'),
               (3, 'Project', 30, '3'), (3, 'Survey', 0, '4');",
        )
        .expect("fixture");
        let agreeing = vec![
            json!({ "name": "Assignments", "weight": 50 }),
            json!({ "name": "Quizzes", "weight": 20.0 }),
            json!({ "name": "Project", "weight": 30 }),
        ];
        let recorded = apply_weights(&conn, 3, &agreeing).expect("apply");
        assert!(recorded.set.is_empty());
        assert_eq!(recorded.unchanged, 3);
        assert_eq!(recorded.unweighted, vec!["Survey".to_string()]);
        assert!(
            recorded.summary().contains("Survey left at 0 — the syllabus does not weight it"),
            "{}",
            recorded.summary()
        );
        assert_eq!(audits(&conn), 0);

        let disagreeing = vec![
            json!({ "name": "Assignments", "weight": 40 }),
            json!({ "name": "Quizzes", "weight": 20 }),
            json!({ "name": "Quizzes", "weight": 25 }),
            json!({ "name": "Project", "weight": "thirty" }),
            json!({ "weight": 10 }),
        ];
        let recorded = apply_weights(&conn, 3, &disagreeing).expect("apply");
        assert!(recorded.set.is_empty());
        assert_eq!(recorded.unchanged, 1);
        assert_eq!(
            recorded.kept,
            vec!["Assignments kept at 50 — the syllabus says 40".to_string()]
        );
        assert_eq!(
            recorded.skipped,
            vec![
                "Quizzes (repeated)".to_string(),
                "Project (bad weight \"thirty\")".to_string(),
                "entry 5 (malformed: missing field `name`)".to_string(),
            ]
        );
        assert_eq!(audits(&conn), 0);
        assert_eq!(weights(&conn, 3)[0].1, 50.0, "the typed weight stands");
    }

    /// The shapes a model writes a share in, and the one it must not.
    #[test]
    fn a_weight_is_a_number_of_percent_however_it_is_written() {
        assert_eq!(percent_of(&json!(50)), Some(50.0));
        assert_eq!(percent_of(&json!(100)), Some(100.0));
        assert_eq!(percent_of(&json!("20%")), Some(20.0));
        assert_eq!(percent_of(&json!(" 20 % ")), Some(20.0));
        assert_eq!(percent_of(&json!("0.5%")), Some(0.5), "explicit is what it says");
        assert_eq!(percent_of(&json!(0.5)), None, "a bare fraction is a share, not a percent");
        assert_eq!(percent_of(&json!("0.5")), None);
        assert_eq!(percent_of(&json!(0)), Some(0.0));
        // Out of range is the write's refusal, not the parser's.
        assert_eq!(percent_of(&json!(-5)), Some(-5.0));
        assert_eq!(percent_of(&json!("thirty")), None);
        assert_eq!(percent_of(&json!({ "value": 50 })), None);
        assert_eq!(percent_of(&json!(null)), None);
    }

    /// The edges of the write, through the pass: the name bound, the weight
    /// bounds, and the folding rule both layers share.
    #[test]
    fn a_scan_keeps_names_and_weights_within_the_category_rules() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight) VALUES (4, 'Übungen', 0);",
        )
        .expect("fixture");
        let long = "Weekly Live Coding Sessions ".repeat(4); // 112 chars
        let raw = vec![
            json!({ "name": long, "weight": 100 }),
            json!({ "name": "Attendance", "weight": -5 }),
            json!({ "name": "übungen", "weight": 10 }),
            json!({ "name": "ÜBUNGEN", "weight": 10 }),
        ];
        let recorded = apply_weights(&conn, 4, &raw).expect("apply");
        let capped = format!("{}…", &long[..80]);
        // The summary names the category as stored, capped at the column's
        // bound, and the stored row is exactly that.
        assert_eq!(
            recorded.set,
            vec![
                format!("{capped} 100 (new)"),
                "übungen 10 (new)".to_string(),
                "ÜBUNGEN 10".to_string(),
            ]
        );
        assert_eq!(
            recorded.skipped,
            vec!["Attendance (a weight is a percentage between 0 and 100)".to_string()]
        );
        let rows = weights(&conn, 4);
        assert_eq!(rows[1].0, capped);
        assert_eq!(rows[1].0.chars().count(), 81);
        // Both layers fold ASCII only, the deadline part's rule for titles:
        // `ÜBUNGEN` is the fixture's `Übungen` to the lookup and to the repeat
        // check alike, and `übungen` is a second category to both.
        assert_eq!(rows[0], ("Übungen".to_string(), 10.0, None));
        assert_eq!(rows[2], ("übungen".to_string(), 10.0, None));
        assert!(recorded.unweighted.is_empty());

        // A second pass of the same breakdown finds the capped name and
        // writes nothing.
        let again = apply_weights(&conn, 4, &raw[..1]).expect("again");
        assert!(again.set.is_empty());
        assert_eq!(again.unchanged, 1);
        assert_eq!(weights(&conn, 4).len(), 3);
    }

    /// The prompt names the class's categories, or says there are none.
    #[test]
    fn the_prompt_lists_the_categories_the_class_tracks() {
        let conn = db();
        assert_eq!(
            categories_block(&conn, 2).expect("block"),
            "The class has no grade categories yet."
        );
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight) VALUES
               (2, 'Studio Participation', 0), (2, 'Quizzes', 0), (3, 'Project', 30);",
        )
        .expect("fixture");
        let block = categories_block(&conn, 2).expect("block");
        assert!(block.ends_with("- Studio Participation\n- Quizzes"), "{block}");
        assert!(!block.contains("Project"), "another class's category is not listed");
        assert!(PROMPT_TEMPLATE.contains("{categories}"), "the template has the slot");
    }

    use super::valid_due_at;

    /// The one gate between model-invented dates and storage: a date that is
    /// merely well-shaped sorts and dedupes as text but renders as a different
    /// day, so the calendar itself has to be checked.
    #[test]
    fn accepts_the_stored_shapes() {
        assert!(valid_due_at("2026-09-03"));
        assert!(valid_due_at("2026-09-03T23:59"));
        assert!(valid_due_at("2026-09-03T23:59:59"));
    }

    #[test]
    fn rejects_malformed_shapes() {
        for s in [
            "", "2026-9-3", "2026/09/03", "26-09-03", "2026-09-03 23:59",
            "2026-09-03T23:59:", "2026-09-03T2359", "2026-09-03Textra",
        ] {
            assert!(!valid_due_at(s), "should reject {s:?}");
        }
    }

    #[test]
    fn rejects_days_the_month_does_not_have() {
        assert!(!valid_due_at("2026-09-31"));
        assert!(!valid_due_at("2026-02-30"));
        assert!(!valid_due_at("2026-13-01"));
        assert!(!valid_due_at("2026-00-10"));
        assert!(!valid_due_at("2026-01-00"));
    }

    #[test]
    fn follows_the_leap_year_rules() {
        assert!(valid_due_at("2024-02-29"), "2024 is a leap year");
        assert!(!valid_due_at("2026-02-29"), "2026 is not");
        assert!(valid_due_at("2000-02-29"), "divisible by 400");
        assert!(!valid_due_at("1900-02-29"), "divisible by 100, not 400");
    }

    #[test]
    fn rejects_out_of_range_times() {
        assert!(!valid_due_at("2026-09-03T24:00"));
        assert!(!valid_due_at("2026-09-03T23:60"));
        assert!(!valid_due_at("2026-09-03T23:59:60"));
        assert!(valid_due_at("2026-09-03T00:00:00"));
    }

    /// A deadline the syllabus scan put on the list before Canvas could is the
    /// same item Canvas now reports on: it takes the assignment's id on first
    /// contact, follows the due date Canvas states, closes on the submission,
    /// and a second sync leaves it — and the audit log — alone.
    #[test]
    fn a_syllabus_deadline_is_linked_closed_and_left_alone_after() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
             VALUES (3, 'Quiz 1', 'quiz', '2026-09-03', 'open', 'syllabus'),
                    (1, 'Quiz 1', 'quiz', '2026-09-03', 'open', 'manual')",
            [],
        )
        .expect("fixture");
        let row = || -> (Option<String>, String, String) {
            conn.query_row(
                "SELECT canvas_assignment_id, due_at, status FROM deadlines WHERE class_id = 3",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("the row")
        };
        let unsubmitted = CanvasAssignment {
            id: "5001",
            title: "quiz 1",
            due_at: Some("2026-09-03T23:59"),
            submitted_at: None,
        };
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &unsubmitted).expect("settle"),
            Some(Settled { due_moved: true, completed: false, card_resolved: false, merged: Vec::new() })
        );
        assert_eq!(
            row(),
            (Some("5001".to_string()), "2026-09-03T23:59".to_string(), "open".to_string())
        );
        let linked: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'canvas.link_deadline'
                   AND payload LIKE '%\"canvasAssignmentId\":\"5001\"%'",
                [],
                |r| r.get(0),
            )
            .expect("audit");
        assert_eq!(linked, 1, "the link leaves its own row");
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &unsubmitted).expect("again"),
            Some(Settled::default())
        );

        let submitted = CanvasAssignment {
            submitted_at: Some("2026-09-03T14:12"),
            ..unsubmitted
        };
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &submitted).expect("submitted"),
            Some(Settled { due_moved: false, completed: true, card_resolved: false, merged: Vec::new() })
        );
        assert_eq!(row().2, "done");
        let named: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'canvas.complete_deadline'
                   AND payload LIKE '%2026-09-03T14:12%'",
                [],
                |r| r.get(0),
            )
            .expect("audit");
        assert_eq!(named, 1, "the audit row names the submission");

        let audits: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .expect("count");
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &submitted).expect("third"),
            Some(Settled::default())
        );
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM audit_log", [], |r| r.get(0))
            .expect("count");
        assert_eq!(after, audits, "a second sync left a row");

        // The other class's same-titled deadline was never this one's.
        let elsewhere: Option<String> = conn
            .query_row(
                "SELECT canvas_assignment_id FROM deadlines WHERE class_id = 1",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(elsewhere, None);
        // Nothing on the list for it means the caller proposes instead.
        let unknown = CanvasAssignment {
            id: "5002",
            title: "Quiz 2",
            due_at: Some("2026-09-24T23:59"),
            submitted_at: None,
        };
        assert_eq!(settle_canvas_deadline(&conn, 3, &unknown).expect("none"), None);
    }

    /// A card whose assignment already sits on the list under another title —
    /// a syllabus row the sync linked by day after the card was proposed — is
    /// refused at approval: one row per assignment is what lets a submission
    /// find the deadline it closes, and a second quiz row is what the reader
    /// would otherwise see.
    #[test]
    fn approval_refuses_a_card_for_an_assignment_already_on_the_list() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines
               (class_id, title, kind, due_at, status, source, canvas_assignment_id)
             VALUES (3, 'Week 3 quiz', 'quiz', '2026-09-03', 'open', 'syllabus', '5001')",
            [],
        )
        .expect("linked deadline");
        conn.execute(
            "INSERT INTO deadline_proposals
               (class_id, title, kind, due_at, status, created_at, source, canvas_assignment_id)
             VALUES (3, 'Quiz 1', 'quiz', '2026-09-03T23:59', 'pending', 0, 'canvas', '5001')",
            [],
        )
        .expect("card");
        let id: i64 = conn
            .query_row("SELECT id FROM deadline_proposals", [], |r| r.get(0))
            .expect("id");
        let err = resolve_in_conn(&conn, id, true).unwrap_err().to_string();
        assert!(err.contains("already on the list"), "{err}");
        let deadlines: i64 = conn
            .query_row("SELECT COUNT(*) FROM deadlines", [], |r| r.get(0))
            .expect("count");
        assert_eq!(deadlines, 1, "a second row landed");
        let status: String = conn
            .query_row("SELECT status FROM deadline_proposals WHERE id = ?1", [id], |r| r.get(0))
            .expect("status");
        assert_eq!(status, "pending", "the card can still be skipped");
    }

    /// Two rows sharing a title and a day — one already done by hand, one
    /// open — and the open one is the one Canvas's submission has something
    /// to close, so it is the one that takes the id; the done one is the
    /// same quiz twice and is folded into it.
    #[test]
    fn the_open_row_is_linked_when_two_share_a_title_and_day() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
             VALUES (3, 'Quiz 1', 'quiz', '2026-09-03', 'done', 'manual'),
                    (3, 'Quiz 1', 'quiz', '2026-09-03T11:45', 'open', 'syllabus')",
            [],
        )
        .expect("fixture");
        let submitted = CanvasAssignment {
            id: "5001",
            title: "Quiz 1",
            due_at: Some("2026-09-03T23:59"),
            submitted_at: Some("2026-09-03T12:30"),
        };
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &submitted).expect("settle"),
            Some(Settled { due_moved: true, completed: true, card_resolved: false, merged: vec!["Quiz 1".to_string()] })
        );
        let (source, status): (String, String) = conn
            .query_row(
                "SELECT source, status FROM deadlines WHERE canvas_assignment_id = '5001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("the linked row");
        assert_eq!((source.as_str(), status.as_str()), ("syllabus", "done"));
    }

    /// An assignment Canvas dates nothing for still closes the deadline it is
    /// tracked by; without a day there is nothing to link a legacy row on and
    /// nothing to move.
    #[test]
    fn an_undated_assignment_still_closes_its_tracked_deadline() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, status, source, canvas_assignment_id)
             VALUES (3, 'Survey', 'other', '2026-09-10', 'open', 'canvas', '6001'),
                    (3, 'Reflection', 'other', '2026-09-12', 'open', 'syllabus', NULL)",
            [],
        )
        .expect("fixture");
        let tracked = CanvasAssignment {
            id: "6001",
            title: "Survey",
            due_at: None,
            submitted_at: Some("2026-09-05T09:00"),
        };
        assert_eq!(
            settle_canvas_deadline(&conn, 3, &tracked).expect("settle"),
            Some(Settled { due_moved: false, completed: true, card_resolved: false, merged: Vec::new() })
        );
        let (due_at, status): (String, String) = conn
            .query_row(
                "SELECT due_at, status FROM deadlines WHERE canvas_assignment_id = '6001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!((due_at.as_str(), status.as_str()), ("2026-09-10", "done"));

        let untracked = CanvasAssignment {
            id: "6002",
            title: "Reflection",
            due_at: None,
            submitted_at: Some("2026-09-05T09:00"),
        };
        assert_eq!(settle_canvas_deadline(&conn, 3, &untracked).expect("settle"), None);
        let linked: Option<String> = conn
            .query_row(
                "SELECT canvas_assignment_id FROM deadlines WHERE title = 'Reflection'",
                [],
                |r| r.get(0),
            )
            .expect("row");
        assert_eq!(linked, None, "linked on a title alone");
    }

    /// A Canvas card is one row per assignment: a moved due date refreshes it
    /// rather than stacking a second one, approval carries the id onto the
    /// deadline, and a deadline deleted by hand comes back as the same card.
    #[test]
    fn a_canvas_card_is_one_row_per_assignment() {
        let conn = db();
        let propose = |due: &str| {
            record_proposal(&conn, 1, "Homework 1", "assignment", due, None, "canvas", Some("7001"))
                .expect("record")
        };
        let rows = || -> (i64, String, String) {
            conn.query_row(
                "SELECT COUNT(*), MIN(due_at), MIN(status) FROM deadline_proposals
                 WHERE canvas_assignment_id = '7001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("rows")
        };
        assert!(matches!(propose("2026-09-07T23:59"), Recorded::Proposed));
        assert!(matches!(propose("2026-09-09T23:59"), Recorded::Refreshed));
        assert_eq!(rows(), (1, "2026-09-09T23:59".to_string(), "pending".to_string()));
        // The rank rule holds on the id path too: a lower-ranked reader with
        // the same id leaves the card as Canvas wrote it.
        assert!(matches!(
            record_proposal(&conn, 1, "Homework 1", "assignment", "2026-09-10", None, "syllabus", Some("7001"))
                .expect("record"),
            Recorded::Refreshed
        ));
        assert_eq!(rows(), (1, "2026-09-09T23:59".to_string(), "pending".to_string()));

        let id: i64 = conn
            .query_row("SELECT id FROM deadline_proposals", [], |r| r.get(0))
            .expect("id");
        resolve_in_conn(&conn, id, true).expect("approve");
        let linked: Option<String> = conn
            .query_row("SELECT canvas_assignment_id FROM deadlines", [], |r| r.get(0))
            .expect("deadline");
        assert_eq!(linked.as_deref(), Some("7001"));
        assert!(matches!(propose("2026-09-09T23:59"), Recorded::AlreadyDeadline));

        conn.execute("DELETE FROM deadlines", []).expect("delete by hand");
        assert!(matches!(propose("2026-09-09T23:59"), Recorded::Proposed));
        assert_eq!(rows(), (1, "2026-09-09T23:59".to_string(), "pending".to_string()));

        conn.execute("UPDATE deadline_proposals SET status = 'dismissed'", []).expect("skip");
        assert!(matches!(propose("2026-09-09T23:59"), Recorded::DismissedBefore));
    }

    /// The id joins the two readers rather than splitting them: a syllabus card
    /// Canvas recognizes takes the id, a syllabus rescan still sees the
    /// Canvas-linked deadline as recorded, and a different assignment that
    /// happens to share a title and a day is its own card.
    #[test]
    fn the_id_joins_the_readers_rather_than_splitting_them() {
        let conn = db();
        let quiz = |due: &str, source: &str, canvas_id: Option<&str>| {
            record_proposal(&conn, 3, "Quiz 1", "quiz", due, None, source, canvas_id).expect("record")
        };
        assert!(matches!(quiz("2026-09-03", "syllabus", None), Recorded::Proposed));
        assert!(matches!(
            quiz("2026-09-03T23:59", "canvas", Some("5001")),
            Recorded::Refreshed
        ));
        let (id, due_at, canvas_id): (i64, String, Option<String>) = conn
            .query_row(
                "SELECT id, due_at, canvas_assignment_id FROM deadline_proposals",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("one card");
        assert_eq!((due_at.as_str(), canvas_id.as_deref()), ("2026-09-03T23:59", Some("5001")));

        resolve_in_conn(&conn, id, true).expect("approve");
        assert!(matches!(quiz("2026-09-03", "syllabus", None), Recorded::AlreadyDeadline));
        assert!(matches!(
            quiz("2026-09-03T23:59", "canvas", Some("5002")),
            Recorded::Proposed
        ));
    }
    /// The objectives a scan reports for a division (SPEC §11): the strings
    /// of its list, trimmed and capped; none stated keeps the column.
    #[test]
    fn a_scan_s_objectives_are_read_per_division() {
        let entry: RawUnit = serde_json::from_value(serde_json::json!({
            "name": "Week 3 — Data Exploration", "kind": "week", "ordinal": 3,
            "objectives": [" Missing data mechanisms ", "", 7, "Data quality"]
        }))
        .expect("entry");
        assert_eq!(
            stated_objectives(entry.objectives.as_deref()),
            Some(vec!["Missing data mechanisms".to_string(), "Data quality".to_string()])
        );
        let none: RawUnit = serde_json::from_value(serde_json::json!({"name": "Week 4"})).expect("entry");
        assert_eq!(stated_objectives(none.objectives.as_deref()), None);
        assert_eq!(stated_objectives(Some(&[])), None);
    }
}

#[cfg(test)]
mod instant_and_series_tests {
    use super::*;

    /// A date-only value is the end of its day; a timed one is its own time.
    #[test]
    fn a_due_date_names_one_instant() {
        assert_eq!(due_instant("2026-09-08"), "2026-09-08T23:59:59");
        assert_eq!(due_instant("2026-09-08T17:00"), "2026-09-08T17:00");
        assert_eq!(due_instant("2026-09-08T17:00:30"), "2026-09-08T17:00:30");
        // The SQL expression and the function agree, so a listing and the
        // card sort as the function reads.
        let conn = crate::db::memory_db();
        for (id, due) in [(1, "2026-09-08"), (2, "2026-09-08T11:59"), (3, "2026-09-07T23:59")] {
            conn.execute(
                "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
                 VALUES (?1, 1, 'x', 'other', ?2, 'open', 'manual')",
                params![id, due],
            )
            .unwrap();
        }
        let order: Vec<i64> = conn
            .prepare(&format!("SELECT id FROM deadlines ORDER BY {DUE_INSTANT_SQL}"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(order, [3, 2, 1], "the date-only row is last on its day");
        // Due today at noon and at 23:59:30 the instant is ahead; the next
        // morning it is behind — the strip's and the tab's `overdue`.
        let due = due_instant("2026-09-08");
        assert!(due.as_str() > "2026-09-08T12:00:00");
        assert!(due.as_str() > "2026-09-08T23:59:30");
        assert!(due.as_str() < "2026-09-09T07:00:00");
    }

    fn card(id: i64, title: &str, due_at: &str) -> DeadlineProposal {
        DeadlineProposal {
            id,
            class_id: 1,
            title: title.to_string(),
            kind: "assignment".to_string(),
            due_at: due_at.to_string(),
            notes: None,
            created_at: 0,
            source: "syllabus".to_string(),
        }
    }

    /// Three dates on one weekday under one stem group; two do not; a date
    /// on another weekday splits off.
    #[test]
    fn a_series_is_three_dates_of_one_stem_on_one_weekday() {
        let tuesdays = vec![
            card(1, "Live coding session 09/08", "2026-09-08"),
            card(2, "Live coding session 09/15", "2026-09-15"),
            card(3, "Live coding session 09/22", "2026-09-22"),
            card(4, "Homework 1", "2026-09-13"),
            card(5, "Homework 2", "2026-09-20"),
        ];
        let series = series_of(&tuesdays);
        assert_eq!(series.len(), 1, "{series:?}");
        assert_eq!(series[0].stem, "Live coding session");
        assert_eq!(series[0].weekday, "Tuesday");
        assert_eq!(series[0].ids, [1, 2, 3]);
        assert_eq!((series[0].first_due.as_str(), series[0].last_due.as_str()), ("2026-09-08", "2026-09-22"));

        let split = vec![
            card(1, "Live coding session 09/08", "2026-09-08"),
            card(2, "Live coding session 09/15", "2026-09-15"),
            card(3, "Live coding session 09/23", "2026-09-23"),
        ];
        assert!(series_of(&split).is_empty(), "a Wednesday splits the third off");
        // The series joins a thirteenth date when the queue is listed again.
        let mut more = tuesdays;
        more.push(card(6, "Live coding session 12/01", "2026-12-01"));
        assert_eq!(series_of(&more)[0].ids.len(), 4);
    }

    #[test]
    fn a_title_s_stem_drops_its_date_token() {
        assert_eq!(title_stem("Live coding session 09/08"), "Live coding session");
        assert_eq!(title_stem("Homework #1"), "Homework");
        assert_eq!(title_stem("Quiz 3 - 2026-10-08"), "Quiz");
        assert_eq!(title_stem("Peer Feedback Session"), "Peer Feedback Session");
        assert_eq!(title_stem("09/08"), "09/08", "a title that is a date keeps itself");
    }
}

#[cfg(test)]
mod tracked_card_tests {
    use super::*;

    /// A Canvas card waiting for an assignment the list now tracks — by id,
    /// or by the title-and-day claim with `#` dropped — leaves the queue as
    /// approved when the sync settles the deadline.
    #[test]
    fn a_settled_assignment_resolves_its_waiting_card() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (31, 1, 'Homework 1', 'assignment', '2026-09-07T23:59', 'open', 'syllabus')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO deadline_proposals
             (id, class_id, title, kind, due_at, status, source, canvas_assignment_id, created_at)
             VALUES (41, 1, 'Homework #1', 'assignment', '2026-09-07T23:59', 'pending', 'canvas',
                     '7251558', 1)",
            [],
        )
        .unwrap();
        let settled = settle_canvas_deadline(
            &conn,
            1,
            &CanvasAssignment {
                id: "7251558",
                title: "Homework #1",
                due_at: Some("2026-09-07T23:59"),
                submitted_at: Some("2026-09-07T20:00"),
            },
        )
        .expect("settle")
        .expect("the syllabus row is claimed");
        assert!(settled.completed && settled.card_resolved, "{settled:?}");
        let (linked, status): (Option<String>, String) = conn
            .query_row("SELECT canvas_assignment_id, status FROM deadlines WHERE id = 31", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((linked.as_deref(), status.as_str()), (Some("7251558"), "done"));
        let card: String = conn
            .query_row("SELECT status FROM deadline_proposals WHERE id = 41", [], |r| r.get(0))
            .unwrap();
        assert_eq!(card, "approved", "the card has nothing left to ask");
    }
}

#[cfg(test)]
mod declined_delete_tests {
    use super::*;

    /// Deleting a deadline Canvas tracks records the decline, so the next
    /// sync's direct write leaves the assignment off the list; a row Canvas
    /// does not track writes no card.
    #[test]
    fn a_deleted_tracked_deadline_stays_off_the_list() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source, canvas_assignment_id)
             VALUES (9, 3, 'Homework Assignment 1', 'assignment', '2026-09-14T23:59', 'open', 'canvas', '7317246')",
            [],
        )
        .unwrap();
        let row = deadline_row(&conn, 9).unwrap().unwrap();
        conn.execute("DELETE FROM deadlines WHERE id = 9", []).unwrap();
        decline_assignment(&conn, &row).expect("declined");
        let assignment = CanvasAssignment {
            id: "7317246",
            title: "Homework Assignment 1",
            due_at: Some("2026-09-14T23:59"),
            submitted_at: None,
        };
        assert_eq!(
            insert_canvas_deadline(&conn, 3, &assignment, "assignment", "From Canvas").unwrap(),
            DirectWrite::DeclinedBefore
        );
        let deadlines: i64 = conn.query_row("SELECT COUNT(*) FROM deadlines", [], |r| r.get(0)).unwrap();
        assert_eq!(deadlines, 0, "not written back");
        let untracked = serde_json::json!({ "id": 10, "classId": 3, "title": "Quiz 1", "canvasAssignmentId": null });
        decline_assignment(&conn, &untracked).expect("nothing to decline");
        let cards: i64 =
            conn.query_row("SELECT COUNT(*) FROM deadline_proposals", [], |r| r.get(0)).unwrap();
        assert_eq!(cards, 1, "one declined card, none for the untracked row");
    }
}

#[cfg(test)]
mod dismiss_tests {
    use super::*;

    /// A series' Skip dismisses every card it can and names the one it
    /// cannot, rather than stopping at it with the rest half done.
    #[test]
    fn a_skip_dismisses_what_it_can_and_names_the_rest() {
        let conn = crate::db::memory_db();
        for (id, status) in [(1, "pending"), (2, "approved"), (3, "pending")] {
            conn.execute(
                "INSERT INTO deadline_proposals
                 (id, class_id, title, kind, due_at, status, source, created_at)
                 VALUES (?1, 1, 'Live coding session', 'assignment', '2026-09-08', ?2, 'syllabus', 1)",
                params![id, status],
            )
            .unwrap();
        }
        let outcome = dismiss_in_conn(&conn, &[1, 2, 3]);
        assert_eq!(outcome.approved, [1, 3]);
        assert_eq!(outcome.skipped.len(), 1, "{outcome:?}");
        assert!(outcome.skipped[0].contains("already resolved"), "{outcome:?}");
        let dismissed: i64 = conn
            .query_row("SELECT COUNT(*) FROM deadline_proposals WHERE status = 'dismissed'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(dismissed, 2);
    }
}
