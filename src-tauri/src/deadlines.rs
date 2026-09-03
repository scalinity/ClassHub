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
    let mut stmt = conn.prepare(
        "SELECT d.id, d.class_id, c.display_name, c.color, d.title, d.kind,
                d.due_at, d.notes, d.status, d.source, d.canvas_assignment_id
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
    // The category names the syllabus breakdown is mapped onto — names only,
    // so the model reads the weights out of the document rather than echoing
    // what was typed. Canvas supplied most of them (§7.2), so its wording is
    // what the syllabus's has to be matched to.
    let mut stmt = conn.prepare(
        "SELECT name FROM grade_categories WHERE class_id = ?1 ORDER BY id",
    )?;
    let category_rows = stmt
        .query_map([class_id], |row| Ok(format!("- {}", row.get::<_, String>(0)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let categories = if category_rows.is_empty() {
        "The class has no grade categories yet.".to_string()
    } else {
        format!(
            "Grade categories the class already tracks (map the syllabus breakdown onto \
             these names where they mean the same thing):\n\n{}",
            category_rows.join("\n")
        )
    };

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{today}", today)
        .replace("{semester}", &semester_label(today))
        .replace("{target}", &target)
        .replace("{existing}", &existing)
        .replace("{categories}", &categories))
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
        let mut seen = 0usize;
        // Names claimed by this scan. `units` holds one row per (class, name),
        // and courses do repeat a topic — Design Studio runs four separate
        // "AI Design Project Presentations" weeks. Two entries under one name
        // become one row and `upsert` reports `Ok(false)`, which reads exactly
        // like "already recorded": a schedule that quietly lost four of its
        // fifteen weeks. Counted so the summary can say so.
        let mut claimed: Vec<String> = Vec::new();
        let mut collapsed: Vec<String> = Vec::new();
        for (index, value) in raw.iter().enumerate() {
            let Ok(entry) = serde_json::from_value::<RawUnit>(value.clone()) else {
                continue;
            };
            let name = entry.name.trim();
            if name.is_empty() {
                continue;
            }
            seen += 1;
            let lowered = name.to_lowercase();
            if claimed.contains(&lowered) {
                collapsed.push(name.to_string());
                continue;
            }
            claimed.push(lowered);
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
                // unparseable date drops the date, not the unit. Truncated to
                // the day because that is what the column holds: `valid_due_at`
                // also accepts a time, and a `starts_on` carrying one renders
                // as nothing at all in the workspace.
                starts_on: entry.starts_on.filter(|d| valid_due_at(d)).map(day_of),
                ends_on: entry.ends_on.filter(|d| valid_due_at(d)).map(day_of),
                source: "syllabus",
            };
            match crate::units::upsert(conn, class_id, &unit) {
                Ok(true) => added += 1,
                Ok(false) => {}
                Err(e) => eprintln!("syllabus: skipping unit '{name}': {e:#}"),
            }
        }
        Ok((added, seen, collapsed))
    });
    match recorded {
        Ok((_, 0, _)) => None,
        Ok((added, seen, collapsed)) => {
            if added > 0 {
                crate::db::emit_hub_change(app, "units");
            }
            let mut summary = if added == 0 {
                format!("{seen} division(s) already recorded")
            } else {
                format!("{added} of {seen} division(s) recorded")
            };
            if !collapsed.is_empty() {
                summary.push_str(&format!(
                    " · {} share a name with an earlier one and were not recorded separately: {}",
                    collapsed.len(),
                    collapsed.join(", ")
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
/// own table prints when the model copies it as a string.
fn percent_of(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().trim_end_matches('%').trim().parse().ok(),
        _ => None,
    }
    .filter(|w| w.is_finite())
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
const SAME_TITLE_AND_DAY: &str =
    "LOWER(title) = LOWER(?2) AND substr(due_at, 1, 10) = substr(?3, 1, 10)";

/// Keeps a (title, day) match away from rows that are some *other* Canvas
/// assignment, bound to ?4 — the reader's Canvas id, or NULL for a reader
/// without one, which then matches every row.
const NOT_ANOTHER_ASSIGNMENT: &str =
    "(?4 IS NULL OR canvas_assignment_id IS NULL OR canvas_assignment_id = ?4)";

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
    type Row = (i64, String, String, String, Option<String>);
    let read = |row: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
    };
    let title = truncate(assignment.title.trim(), MAX_TITLE_CHARS);
    let canvas_due = assignment.due_at.filter(|d| valid_due_at(d));
    if title.is_empty() {
        return Ok(None);
    }
    let by_id: Option<Row> = conn
        .query_row(
            "SELECT id, title, due_at, status, canvas_assignment_id FROM deadlines
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
    let row = match (by_id, canvas_due) {
        (Some(row), _) => row,
        // Without a day there is no (title, day) to match a legacy row on.
        (None, None) => return Ok(None),
        (None, Some(canvas_due)) => {
            // An open row over a done one, when two share a title and a day:
            // the open one is the one a submission has something to close.
            let legacy: Option<Row> = tx
                .query_row(
                    &format!(
                        "SELECT id, title, due_at, status, canvas_assignment_id FROM deadlines
                         WHERE class_id = ?1 AND {SAME_TITLE_AND_DAY}
                           AND canvas_assignment_id IS NULL
                         ORDER BY (status = 'open') DESC, id LIMIT 1"
                    ),
                    params![class_id, title, canvas_due],
                    read,
                )
                .optional()?;
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
    let (id, title, due_at, status, _) = row;

    let mut settled = Settled::default();
    if let Some(canvas_due) = canvas_due.filter(|d| *d != due_at) {
        tx.execute(
            "UPDATE deadlines SET due_at = ?1 WHERE id = ?2",
            params![canvas_due, id],
        )?;
        audit(
            &tx,
            "canvas.update_deadline",
            json!({ "id": id, "classId": class_id, "canvasAssignmentId": assignment.id,
                    "title": title, "before": { "dueAt": due_at },
                    "after": { "dueAt": canvas_due } }),
        )?;
        settled.due_moved = true;
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
    tx.commit()?;
    Ok(Some(settled))
}

// ---------------------------------------------------------------------------
// Resolution: the confirm cards

/// Approve inserts the deadline (source='syllabus') and audits it; dismiss
/// parks the row — either way the card leaves the queue.
pub fn resolve_proposal(app: &AppHandle, proposal_id: i64, approve: bool) -> Result<String> {
    let summary = with_conn(app, |conn| resolve_in_conn(conn, proposal_id, approve))?;
    emit_hub_change(app, "deadlineProposals");
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
    emit_hub_change(app, "deadlineProposals");
    if !outcome.approved.is_empty() {
        emit_hub_change(app, "deadlines");
    }
    Ok(outcome)
}

fn resolve_in_conn(conn: &Connection, proposal_id: i64, approve: bool) -> Result<String> {
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
        return Ok(format!("skipped — {title}"));
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
    audit(
        &tx,
        &format!("{source}.insert_deadline"),
        json!({ "proposalId": proposal_id, "deadlineId": tx.last_insert_rowid(),
                "classId": class_id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes, "canvasAssignmentId": canvas_id }),
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
    use super::*;

    /// The real schema, classes included — `0001_init.sql` seeds them.
    fn db() -> rusqlite::Connection {
        crate::db::memory_db()
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
        assert_eq!(
            recorded.summary(),
            "3 weight(s) already as the syllabus states · Survey left at 0 — the syllabus does not weight it"
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
        assert_eq!(
            recorded.summary(),
            "1 weight(s) already as the syllabus states · Assignments kept at 50 — the syllabus says 40 \
             · Survey left at 0 — the syllabus does not weight it \
             · weights skipped: Quizzes (repeated); Project (bad weight \"thirty\"); \
             entry 5 (malformed: missing field `name`)"
        );
        assert_eq!(audits(&conn), 0);
        assert_eq!(weights(&conn, 3)[0].1, 50.0, "the typed weight stands");
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
            Some(Settled { due_moved: true, completed: false })
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
            Some(Settled { due_moved: false, completed: true })
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
    /// to close, so it is the one that takes the id.
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
            Some(Settled { due_moved: true, completed: true })
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
            Some(Settled { due_moved: false, completed: true })
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
}
