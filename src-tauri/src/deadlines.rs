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

use crate::db::{audit, emit_hub_change, now, truncate, with_conn};

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
    let title = truncate(title.trim(), MAX_TITLE_CHARS);
    validate_fields(&title, kind, due_at)?;
    let notes = notes
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| truncate(n, MAX_NOTES_CHARS));
    // Write + audit land as one unit — a history entry must not be lost to a
    // failure between the two statements.
    with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        match id {
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
                )?;
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
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "deadlines");
    Ok(())
}

/// done ↔ open toggle — the row's checkbox. Reopening is as cheap as closing.
pub fn set_deadline_status(app: &AppHandle, id: i64, done: bool) -> Result<()> {
    let status = if done { "done" } else { "open" };
    with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let changed = tx.execute(
            "UPDATE deadlines SET status = ?1 WHERE id = ?2",
            params![status, id],
        )?;
        if changed == 0 {
            bail!("no deadline #{id}");
        }
        audit(&tx, "ui.set_deadline_status", json!({ "id": id, "status": status }))?;
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "deadlines");
    Ok(())
}

/// The full row rides the audit entry, so a deletion is recoverable — that
/// stands in for a confirmation prompt. Delete and audit commit together:
/// the audit row IS the undo, so the delete must never outlive it.
pub fn delete_deadline(app: &AppHandle, id: i64) -> Result<()> {
    with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let row = tx
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
        tx.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        audit(&tx, "ui.delete_deadline", row)?;
        tx.commit()?;
        Ok(())
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
    /// syllabus | canvas — the card says where the date came from, because
    /// "Canvas says this is due then" and "a PDF seemed to say so" warrant
    /// different amounts of scrutiny before approving.
    pub source: String,
}

pub fn syllabus_proposals(conn: &Connection, class_id: i64) -> Result<Vec<DeadlineProposal>> {
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

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{today}", today)
        .replace("{semester}", &semester_label(today))
        .replace("{target}", &target)
        .replace("{existing}", &existing))
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
}

/// Parses and records the scan's proposals. Unlike the sort job, an empty
/// array is a legitimate success (the material may hold no dated items) —
/// only unparseable output or an all-invalid batch is an error the caller
/// demotes to job failure.
pub fn finalize_job(app: &AppHandle, class_id: i64, result_text: &str) -> Result<String> {
    let (entries, raw_units) = split_output(result_text)?;
    let unit_summary = record_units(app, class_id, &raw_units);
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
            )? {
                // A re-proposal of something still waiting counts as recorded
                // here: the scan did find it, and the card is in the queue.
                Recorded::Proposed | Recorded::Refreshed => recorded += 1,
                Recorded::AlreadyDeadline => duplicates += 1,
                Recorded::DismissedBefore => dismissed_skips += 1,
            }
        }

        if recorded == 0 && duplicates == 0 && dismissed_skips == 0 && !skipped.is_empty() {
            bail!("no valid proposals in the scan output — skipped: {}", skipped.join("; "));
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
    emit_hub_change(app, "syllabus");
    let summary = match unit_summary {
        Some(units) => format!("{summary} · {units}"),
        None => summary,
    };
    Ok(summary)
}

/// Splits the scan's output into its two halves.
///
/// The contract is one object holding `deadlines` and `units`, because the
/// weekly schedule and the due dates live in the same document and cost one
/// read between them. A bare array is still accepted as the deadline list
/// alone: the model does occasionally answer the older shape, and dropping a
/// whole scan's findings over the wrapper would be an expensive way to be
/// strict about punctuation.
fn split_output(result_text: &str) -> Result<(Vec<serde_json::Value>, Vec<serde_json::Value>)> {
    if let Ok(record) = crate::jobs::parse_object(result_text) {
        if record.get("deadlines").is_some() || record.get("units").is_some() {
            let array = |key: &str| {
                record
                    .get(key)
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
            };
            return Ok((array("deadlines"), array("units")));
        }
    }
    Ok((crate::jobs::parse_entries(result_text)?, Vec::new()))
}

/// Records the divisions the syllabus declared, as SPEC §7.2's middle source.
///
/// Failures here are reported, never propagated: the deadlines half of the
/// scan has already succeeded by this point, and losing it because a week
/// entry was malformed would be the wrong trade.
fn record_units(app: &AppHandle, class_id: i64, raw: &[serde_json::Value]) -> Option<String> {
    if raw.is_empty() {
        return None;
    }
    let recorded = with_conn(app, |conn| {
        let mut added = 0usize;
        let mut seen = 0usize;
        for (index, value) in raw.iter().enumerate() {
            let Ok(entry) = serde_json::from_value::<RawUnit>(value.clone()) else {
                continue;
            };
            let name = entry.name.trim();
            if name.is_empty() {
                continue;
            }
            seen += 1;
            let unit = crate::units::NewUnit {
                ordinal: entry.ordinal.unwrap_or(index as i64 + 1),
                kind: entry
                    .kind
                    .as_deref()
                    .map(str::to_lowercase)
                    .filter(|k| crate::units::UNIT_KINDS.contains(&k.as_str()))
                    .unwrap_or_else(|| crate::units::kind_for_name(name).to_string()),
                name: name.to_string(),
                canvas_id: None,
                rel_path: None,
                // A syllabus week without a date is ordinary — two of the four
                // courses number their weeks and never date them — so an
                // unparseable date drops the date, not the unit.
                starts_on: entry.starts_on.filter(|d| valid_due_at(d)),
                ends_on: entry.ends_on.filter(|d| valid_due_at(d)),
                source: "syllabus",
            };
            match crate::units::upsert(conn, class_id, &unit) {
                Ok(true) => added += 1,
                Ok(false) => {}
                Err(e) => eprintln!("syllabus: skipping unit '{name}': {e:#}"),
            }
        }
        Ok((added, seen))
    });
    match recorded {
        Ok((_, 0)) => None,
        Ok((added, seen)) if added == 0 => Some(format!("{seen} division(s) already recorded")),
        Ok((added, seen)) => {
            crate::db::emit_hub_change(app, "units");
            Some(format!("{added} of {seen} division(s) recorded"))
        }
        Err(e) => {
            eprintln!("syllabus: units not recorded for class {class_id}: {e:#}");
            Some("divisions could not be recorded".to_string())
        }
    }
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

/// The one path a proposed deadline takes into the queue, whatever proposed it.
///
/// Both producers — the syllabus scan reading a PDF and the Canvas sync reading
/// assignments — land here, so the deduplication rules cannot drift apart
/// between them. Identity is (title, calendar day): the same item proposed from
/// both sources is one card, not two.
pub(crate) fn record_proposal(
    conn: &Connection,
    class_id: i64,
    title: &str,
    kind: &str,
    due_at: &str,
    notes: Option<&str>,
    source: &str,
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

    let existing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deadlines
         WHERE class_id = ?1 AND LOWER(title) = LOWER(?2)
           AND substr(due_at, 1, 10) = substr(?3, 1, 10)",
        params![class_id, title, due_at],
        |row| row.get(0),
    )?;
    if existing > 0 {
        return Ok(Recorded::AlreadyDeadline);
    }
    let dismissed_before: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deadline_proposals
         WHERE class_id = ?1 AND LOWER(title) = LOWER(?2)
           AND substr(due_at, 1, 10) = substr(?3, 1, 10)
           AND status = 'dismissed'",
        params![class_id, title, due_at],
        |row| row.get(0),
    )?;
    if dismissed_before > 0 {
        return Ok(Recorded::DismissedBefore);
    }
    let updated = conn.execute(
        "UPDATE deadline_proposals
         SET kind = ?1, due_at = ?2, notes = ?3, created_at = ?4, source = ?5
         WHERE class_id = ?6 AND LOWER(title) = LOWER(?7)
           AND substr(due_at, 1, 10) = substr(?8, 1, 10) AND status = 'pending'",
        params![kind, due_at, notes, now(), source, class_id, title, due_at],
    )?;
    if updated > 0 {
        return Ok(Recorded::Refreshed);
    }
    conn.execute(
        "INSERT INTO deadline_proposals
         (class_id, title, kind, due_at, notes, status, created_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?7)",
        params![class_id, title, kind, due_at, notes, now(), source],
    )?;
    Ok(Recorded::Proposed)
}

// ---------------------------------------------------------------------------
// Resolution: the confirm cards

/// Approve inserts the deadline (source='syllabus') and audits it; dismiss
/// parks the row — either way the card leaves the queue.
pub fn resolve_proposal(app: &AppHandle, proposal_id: i64, approve: bool) -> Result<String> {
    let summary = with_conn(app, |conn| resolve_in_conn(conn, proposal_id, approve))?;
    emit_hub_change(app, "syllabus");
    if approve {
        emit_hub_change(app, "deadlines");
    }
    Ok(summary)
}

/// What a batch approval did: which cards can leave the queue, and one line
/// per card that could not be added (its proposal row stays pending).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOutcome {
    pub approved: Vec<i64>,
    pub skipped: Vec<String>,
}

/// The ADD ALL button: approves each proposal independently — one rejection
/// (e.g. an identical deadline added by hand since the scan) costs that card,
/// never the rest — and pushes hub-changed once at the end instead of per
/// card. Each approval keeps its own transaction inside resolve_in_conn.
pub fn approve_proposals(app: &AppHandle, proposal_ids: &[i64]) -> Result<BatchOutcome> {
    if proposal_ids.is_empty() {
        bail!("no proposals to approve");
    }
    let outcome = with_conn(app, |conn| {
        let mut approved = Vec::new();
        let mut skipped = Vec::new();
        for &id in proposal_ids {
            match resolve_in_conn(conn, id, true) {
                Ok(_) => approved.push(id),
                Err(e) => skipped.push(format!("{e:#}")),
            }
        }
        Ok(BatchOutcome { approved, skipped })
    })?;
    emit_hub_change(app, "syllabus");
    if !outcome.approved.is_empty() {
        emit_hub_change(app, "deadlines");
    }
    Ok(outcome)
}

fn resolve_in_conn(conn: &Connection, proposal_id: i64, approve: bool) -> Result<String> {
    let row = conn
        .query_row(
            "SELECT class_id, title, kind, due_at, notes, status, source
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
                ))
            },
        )
        .optional()?
        .context("proposal not found")?;
    let (class_id, title, kind, due_at, notes, status, source) = row;
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

    // Insert + audit + status flip land as one unit — interrupted midway
    // they would leave a deadline behind a still-pending card.
    let tx = conn.unchecked_transaction()?;
    // A matching deadline may have appeared since the scan (added by hand
    // or through chat) — approving would silently duplicate it.
    let existing: i64 = tx.query_row(
        "SELECT COUNT(*) FROM deadlines
         WHERE class_id = ?1 AND LOWER(title) = LOWER(?2)
           AND substr(due_at, 1, 10) = substr(?3, 1, 10)",
        params![class_id, title, due_at],
        |row| row.get(0),
    )?;
    if existing > 0 {
        bail!("'{title}' is already recorded for that date — skip this card instead");
    }
    // The deadline records which reader proposed it, so a due date that turns
    // out to be wrong can be traced to the syllabus PDF or to Canvas.
    tx.execute(
        "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
        params![class_id, title, kind, due_at, notes, source],
    )?;
    audit(
        &tx,
        &format!("{source}.insert_deadline"),
        json!({ "proposalId": proposal_id, "deadlineId": tx.last_insert_rowid(),
                "classId": class_id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes }),
    )?;
    tx.execute(
        "UPDATE deadline_proposals SET status = 'approved', resolved_at = ?1
         WHERE id = ?2",
        params![now(), proposal_id],
    )?;
    tx.commit()?;
    Ok(format!("added — {title} due {due_at}"))
}

#[cfg(test)]
mod tests {
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
}
