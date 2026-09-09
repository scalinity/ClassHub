//! SPEC §7.2 — the announcement scan: a light-tier, read-only job over the
//! class's Canvas announcements nothing has read yet, answering per notice
//! with the deadlines it commits to, the to-dos it asks for and the changes
//! it announces. Dated items become proposals — a model's reading, so a
//! card, through the one path every proposal takes — and the to-dos and
//! changes become lines under the notice, a to-do with a checkbox. A notice
//! is read once: its `scanned_at` is stamped by the finalize, and cleared
//! again when a sync updates an edited notice in place.

use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::db::{audit, emit_hub_change, now, truncate, with_conn};
use crate::deadlines::{record_proposal, valid_due_at, Recorded, DEADLINE_KINDS, MAX_NOTES_CHARS, MAX_TITLE_CHARS};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/announcements.md");
pub const KIND: &str = "announcement_scan";
/// A notice's body in the prompt; the longest on record is under 2 KB.
const MAX_BODY_CHARS: usize = 4000;
/// A to-do or a change as stored: a line, not a paragraph.
const MAX_ACTION_CHARS: usize = 300;
const MAX_ACTIONS_PER_KIND: usize = 8;
/// A proposed date this far from today is a year the prompt's typo rule did
/// not catch — "9/5/2027" in a Fall 2026 course sits a year out — and is
/// named rather than carded. A term's dates run four months either way
/// of any day in it; a final in December is well inside this from August.
const MAX_DAYS_FROM_TODAY: i64 = 200;

/// Queues a scan over the class's unread announcements, or answers `None`
/// when there are none or one is already queued or running.
pub fn enqueue_scan(app: &AppHandle, class_id: i64) -> Result<Option<i64>> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let prompt = with_conn(app, |conn| {
        let active: i64 = conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE kind = ?1 AND class_id = ?2
               AND status IN ('queued', 'running')",
            params![KIND, class_id],
            |row| row.get(0),
        )?;
        if active > 0 {
            return Ok(None);
        }
        build_prompt(conn, class_id, &today)
    })?;
    match prompt {
        Some((prompt, listed)) => {
            // The notices the prompt carries ride the job, so the finalize can
            // stamp every one of them read — an answer that leaves one out is
            // the model saying it holds nothing, which the prompt allows.
            let payload = serde_json::to_string(&ScanPayload { announcement_ids: listed })?;
            crate::jobs::enqueue_announcement_scan(app, class_id, &prompt, payload)
        }
        None => Ok(None),
    }
}

/// Carried across the job: the notices the prompt listed.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanPayload {
    announcement_ids: Vec<i64>,
}

/// The prompt over the class's unread notices and their ids, or `None`
/// with none to read.
fn build_prompt(conn: &Connection, class_id: i64, today: &str) -> Result<Option<(String, Vec<i64>)>> {
    let mut stmt = conn.prepare(
        "SELECT id, canvas_id, title, body, posted_at FROM announcements
         WHERE class_id = ?1 AND scanned_at IS NULL ORDER BY posted_at, id",
    )?;
    let unread = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if unread.is_empty() {
        return Ok(None);
    }
    let class_name: String = conn.query_row(
        "SELECT display_name FROM classes WHERE id = ?1",
        [class_id],
        |row| row.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT due_at, title, kind FROM deadlines WHERE class_id = ?1 ORDER BY due_at",
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
        existing_rows.join("\n")
    };
    let listed: Vec<i64> = unread.iter().map(|(id, ..)| *id).collect();
    let announcements = unread
        .iter()
        .map(|(_, canvas_id, title, body, posted_at)| {
            let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
            format!(
                "--- canvas_id: {canvas_id} · posted {} · \"{}\"\n{}\n",
                posted_at.get(..10).unwrap_or(posted_at),
                one_line(title),
                truncate(body.trim(), MAX_BODY_CHARS)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(Some((
        PROMPT_TEMPLATE
            .replace("{class}", &class_name)
            .replace("{today}", today)
            .replace("{semester}", &crate::deadlines::semester_label(today))
            .replace("{existing}", &existing)
            .replace("{announcements}", &announcements),
        listed,
    )))
}

/// One entry of the job's contracted answer.
#[derive(Deserialize)]
struct RawEntry {
    canvas_id: serde_json::Value,
    #[serde(default)]
    deadlines: Vec<serde_json::Value>,
    #[serde(default)]
    todos: Vec<serde_json::Value>,
    #[serde(default)]
    changes: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawDeadline {
    title: String,
    #[serde(default)]
    kind: Option<String>,
    due_at: String,
    #[serde(default)]
    notes: Option<String>,
}

/// The entries of the scan's answer: the `announcements` list of its object,
/// or a bare list.
fn split_output(result_text: &str) -> Result<Vec<serde_json::Value>> {
    if let Ok(record) = crate::jobs::parse_object(result_text) {
        match record.get("announcements") {
            Some(serde_json::Value::Array(items)) => return Ok(items.clone()),
            Some(serde_json::Value::Null) | None => {}
            Some(_) => bail!("the scan's `announcements` came back as something other than a list"),
        }
    }
    crate::jobs::parse_entries(result_text)
}

/// What the scan recorded, for the job's summary.
#[derive(Default, Debug, PartialEq, Eq)]
pub(crate) struct ScanRecord {
    pub notices: usize,
    /// Notices the prompt listed and the answer left out: read, with nothing
    /// to report.
    pub omitted: usize,
    pub proposed: usize,
    pub known: usize,
    pub todos: usize,
    pub changes: usize,
    pub skipped: Vec<String>,
    /// The notices a to-do was read out of — what the notification names.
    pub todo_titles: Vec<String>,
}

/// Records the scan's answer (SPEC §7.2): a proposal per dated item through
/// `record_proposal`, a row per to-do and change, and the read stamp on
/// each notice it answered for. A malformed entry costs that notice, never
/// the scan; a notice already read, or one the class does not hold, is
/// named and left alone.
pub fn finalize_job(
    app: &AppHandle,
    class_id: i64,
    payload: Option<&str>,
    result_text: &str,
) -> Result<String> {
    let entries = split_output(result_text)?;
    let today = chrono::Local::now().date_naive();
    let listed: Vec<i64> = payload
        .and_then(|p| serde_json::from_str::<ScanPayload>(p).ok())
        .map(|p| p.announcement_ids)
        .unwrap_or_default();
    let mut recorded = with_conn(app, |conn| {
        let mut recorded = record_entries(conn, class_id, &entries, today)?;
        recorded.omitted = stamp_listed(conn, class_id, &listed)?;
        Ok(recorded)
    })?;
    if recorded.notices == 0 && recorded.omitted == 0 && !recorded.skipped.is_empty() {
        bail!(
            "no announcement of this class was read — {}",
            recorded.skipped.join("; ")
        );
    }
    recorded.notices += recorded.omitted;
    if recorded.proposed > 0 {
        emit_hub_change(app, "deadlineProposals");
    }
    emit_hub_change(app, "announcements");
    // A notice that asks for something is worth a word the moment it is
    // read (SPEC §12), whether a sync or the shift brought it across.
    if !recorded.todo_titles.is_empty() {
        let class_name = with_conn(app, |conn| {
            Ok(conn.query_row(
                "SELECT display_name FROM classes WHERE id = ?1",
                [class_id],
                |r| r.get::<_, String>(0),
            )?)
        })
        .unwrap_or_default();
        crate::notifications::notify(
            app,
            crate::settings::NOTIFY_ANNOUNCEMENT_ACTION,
            &format!("{class_name} · a notice asks for something"),
            &format!(
                "{} · {}",
                plural(recorded.todos, "to-do", "to-dos"),
                recorded.todo_titles.join(" · ")
            ),
        );
    }
    Ok(recorded.summary())
}

impl ScanRecord {
    fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} read",
            plural(self.notices, "notice", "notices")
        )];
        if self.proposed > 0 {
            parts.push(format!(
                "{} awaiting review",
                plural(self.proposed, "deadline proposal", "deadline proposals")
            ));
        }
        if self.known > 0 {
            parts.push(format!("{} already on the list", self.known));
        }
        if self.todos > 0 {
            parts.push(plural(self.todos, "to-do", "to-dos"));
        }
        if self.changes > 0 {
            parts.push(plural(self.changes, "change", "changes"));
        }
        let mut summary = parts.join(" · ");
        if !self.skipped.is_empty() {
            summary.push_str(&format!(" · skipped {}", self.skipped.join("; ")));
        }
        summary
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub(crate) fn record_entries(
    conn: &Connection,
    class_id: i64,
    entries: &[serde_json::Value],
    today: NaiveDate,
) -> Result<ScanRecord> {
    let mut out = ScanRecord::default();
    for (index, raw) in entries.iter().enumerate() {
        let entry: RawEntry = match serde_json::from_value(raw.clone()) {
            Ok(entry) => entry,
            Err(e) => {
                out.skipped.push(format!("entry {} (malformed: {e})", index + 1));
                continue;
            }
        };
        let canvas_id = match &entry.canvas_id {
            serde_json::Value::String(s) => s.trim().to_string(),
            serde_json::Value::Number(n) => n.to_string(),
            _ => {
                out.skipped.push(format!("entry {} (no canvas id)", index + 1));
                continue;
            }
        };
        let row: Option<(i64, Option<i64>, String)> = conn
            .query_row(
                "SELECT id, scanned_at, title FROM announcements WHERE class_id = ?1 AND canvas_id = ?2",
                params![class_id, canvas_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((announcement_id, scanned_at, title)) = row else {
            out.skipped.push(format!("canvas id {canvas_id} (not one of this class's notices)"));
            continue;
        };
        if scanned_at.is_some() {
            out.skipped.push(format!("{title} (already read)"));
            continue;
        }
        let tx = conn.unchecked_transaction()?;
        for (i, raw) in entry.deadlines.iter().enumerate() {
            let deadline: RawDeadline = match serde_json::from_value(raw.clone()) {
                Ok(d) => d,
                Err(e) => {
                    out.skipped.push(format!("{title}: deadline {} (malformed: {e})", i + 1));
                    continue;
                }
            };
            let dtitle = truncate(deadline.title.trim(), MAX_TITLE_CHARS);
            let due_at = deadline.due_at.trim();
            if dtitle.is_empty() || !valid_due_at(due_at) {
                out.skipped.push(format!("{title}: '{dtitle}' (bad date '{due_at}')"));
                continue;
            }
            if !plausible(due_at, today) {
                out.skipped.push(format!("{title}: '{dtitle}' dated {due_at}, too far from today"));
                continue;
            }
            let kind = deadline
                .kind
                .as_deref()
                .map(str::to_lowercase)
                .filter(|k| DEADLINE_KINDS.contains(&k.as_str()))
                .unwrap_or_else(|| "other".to_string());
            let notes = deadline
                .notes
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(|n| truncate(n, MAX_NOTES_CHARS));
            match record_proposal(&tx, class_id, &dtitle, &kind, due_at, notes.as_deref(), "announcement", None)? {
                Recorded::Proposed | Recorded::Refreshed => out.proposed += 1,
                Recorded::AlreadyDeadline | Recorded::DismissedBefore => out.known += 1,
            }
        }
        let todos = record_actions(&tx, announcement_id, "todo", &entry.todos)?;
        if todos > 0 {
            out.todo_titles.push(title.clone());
        }
        out.todos += todos;
        out.changes += record_actions(&tx, announcement_id, "change", &entry.changes)?;
        tx.execute(
            "UPDATE announcements SET scanned_at = ?1 WHERE id = ?2",
            params![now(), announcement_id],
        )?;
        tx.commit()?;
        out.notices += 1;
    }
    Ok(out)
}

/// Whether a proposed date sits within `MAX_DAYS_FROM_TODAY` of today; past
/// that it is a typo, as "9/5/2027" in a Fall 2026 course was (SPEC §1).
fn plausible(due_at: &str, today: NaiveDate) -> bool {
    NaiveDate::parse_from_str(due_at.get(..10).unwrap_or(""), "%Y-%m-%d")
        .is_ok_and(|d| (d - today).num_days().abs() <= MAX_DAYS_FROM_TODAY)
}

/// Stamps read every listed notice the answer left out — the model's "nothing
/// here", which the prompt allows — so a notice is offered once and the
/// next sync spends nothing on it again. Answers with how many.
fn stamp_listed(conn: &Connection, class_id: i64, listed: &[i64]) -> Result<usize> {
    let mut stamped = 0usize;
    for id in listed {
        stamped += conn.execute(
            "UPDATE announcements SET scanned_at = ?1
             WHERE id = ?2 AND class_id = ?3 AND scanned_at IS NULL",
            params![now(), id, class_id],
        )?;
    }
    Ok(stamped)
}

/// The lines of one kind under a notice, trimmed, capped, deduplicated and
/// written once each; answers with how many were new.
fn record_actions(
    conn: &Connection,
    announcement_id: i64,
    kind: &str,
    lines: &[serde_json::Value],
) -> Result<usize> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut written = 0usize;
    for line in lines {
        let Some(text) = line.as_str() else { continue };
        let text = truncate(text.split_whitespace().collect::<Vec<_>>().join(" ").trim(), MAX_ACTION_CHARS);
        if text.is_empty() || !seen.insert(text.to_lowercase()) {
            continue;
        }
        if seen.len() > MAX_ACTIONS_PER_KIND {
            break;
        }
        written += conn.execute(
            "INSERT OR IGNORE INTO announcement_actions (announcement_id, kind, text, done, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![announcement_id, kind, text, now()],
        )?;
    }
    Ok(written)
}

// ---------------------------------------------------------------------------
// What the workspace shows

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ActionInfo {
    pub id: i64,
    pub announcement_id: i64,
    /// todo | change
    pub kind: String,
    pub text: String,
    pub done: bool,
}

/// Every to-do and change under the class's notices, in the order written.
pub fn list_actions(conn: &Connection, class_id: i64) -> Result<Vec<ActionInfo>> {
    let mut stmt = conn.prepare(
        "SELECT a.id, a.announcement_id, a.kind, a.text, a.done
         FROM announcement_actions a JOIN announcements n ON n.id = a.announcement_id
         WHERE n.class_id = ?1 ORDER BY a.announcement_id, a.id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(ActionInfo {
                id: row.get(0)?,
                announcement_id: row.get(1)?,
                kind: row.get(2)?,
                text: row.get(3)?,
                done: row.get::<_, i64>(4)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The checkbox on a to-do: its own way back, so no notice follows.
pub fn set_action_done(app: &AppHandle, id: i64, done: bool) -> Result<()> {
    with_conn(app, |conn| {
        let (text, was): (String, i64) = conn
            .query_row(
                "SELECT text, done FROM announcement_actions WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .context("no such to-do")?;
        if (was != 0) == done {
            return Ok(());
        }
        conn.execute(
            "UPDATE announcement_actions SET done = ?1 WHERE id = ?2",
            params![i64::from(done), id],
        )?;
        audit(
            conn,
            "announcement.action_done",
            serde_json::json!({ "id": id, "text": text, "done": done }),
        )?;
        Ok(())
    })?;
    emit_hub_change(app, "announcements");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(conn: &Connection, class_id: i64, canvas_id: &str, title: &str) -> i64 {
        conn.execute(
            "INSERT INTO announcements (class_id, canvas_id, title, body, posted_at)
             VALUES (?1, ?2, ?3, 'body', '2026-09-02T15:03')",
            params![class_id, canvas_id, title],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// The contract's shape, recorded: a dated item becomes a card carrying
    /// `announcement`, one the list holds is counted as known, the to-dos and
    /// changes land under the notice, and the notice is stamped read.
    #[test]
    fn a_scan_records_proposals_and_actions_and_stamps_the_notice() {
        let conn = crate::db::memory_db();
        let id = notice(&conn, 2, "5352500", "Today's Office Hours Postponed");
        conn.execute(
            "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
             VALUES (2, 'Problem Statement and AI Sketch', 'assignment', '2026-09-09T23:59', 'open', 'canvas')",
            [],
        )
        .unwrap();
        let entries: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"canvas_id": "5352500",
                 "deadlines": [{"title": "Office hours moved to 6 pm", "kind": "other", "due_at": "2026-09-02T18:00"},
                               {"title": "Problem Statement and AI Sketch", "kind": "assignment", "due_at": "2026-09-09T23:59"}],
                 "todos": ["Reach out on Teams to schedule a Zoom meeting", "  Reach out on Teams to schedule a Zoom meeting "],
                 "changes": ["The Module 2 page is now live"]}]"#,
        )
        .unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let recorded = record_entries(&conn, 2, &entries, today).unwrap();
        assert_eq!(
            recorded,
            ScanRecord {
                notices: 1,
                omitted: 0,
                proposed: 1,
                known: 1,
                todos: 1,
                changes: 1,
                skipped: vec![],
                todo_titles: vec!["Today's Office Hours Postponed".to_string()],
            }
        );
        let (source, title): (String, String) = conn
            .query_row(
                "SELECT source, title FROM deadline_proposals WHERE class_id = 2 AND status = 'pending'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((source.as_str(), title.as_str()), ("announcement", "Office hours moved to 6 pm"));
        let actions = list_actions(&conn, 2).unwrap();
        assert_eq!(actions.len(), 2);
        assert_eq!((actions[0].kind.as_str(), actions[0].done), ("todo", false));
        let scanned: Option<i64> = conn
            .query_row("SELECT scanned_at FROM announcements WHERE id = ?1", [id], |row| row.get(0))
            .unwrap();
        assert!(scanned.is_some());
        // A second scan over the same notice writes nothing.
        let again = record_entries(&conn, 2, &entries, today).unwrap();
        assert_eq!(again.notices, 0);
        assert_eq!(again.skipped, vec!["Today's Office Hours Postponed (already read)".to_string()]);
        assert_eq!(list_actions(&conn, 2).unwrap().len(), 2);
    }

    /// A malformed entry, an unknown notice and an implausible year each
    /// cost their own item and never the scan.
    #[test]
    fn a_malformed_entry_costs_that_announcement_never_the_scan() {
        let conn = crate::db::memory_db();
        notice(&conn, 3, "5353667", "Quizzes and Homework 1");
        let entries: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"deadlines": []},
                {"canvas_id": "999", "deadlines": [], "todos": ["x"], "changes": []},
                {"canvas_id": 5353667,
                 "deadlines": [{"title": "Programming Quiz 1", "kind": "quiz", "due_at": "2027-09-05T23:59"},
                               {"title": "Homework 1", "kind": "assignment", "due_at": "2026-09-13T23:59"},
                               {"title": "", "due_at": "2026-09-13"}],
                 "todos": [], "changes": []}]"#,
        )
        .unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let recorded = record_entries(&conn, 3, &entries, today).unwrap();
        assert_eq!((recorded.notices, recorded.proposed), (1, 1));
        assert_eq!(recorded.skipped.len(), 4);
        assert!(recorded.skipped[2].contains("too far from today"), "{:?}", recorded.skipped);
        assert_eq!(list_actions(&conn, 3).unwrap().len(), 0);
    }

    /// The answer's wrapper, or a bare list, both read.
    #[test]
    fn the_answer_is_read_wrapped_or_bare() {
        let wrapped = split_output(r#"Here you go: {"announcements": [{"canvas_id": "1", "deadlines": []}]}"#).unwrap();
        assert_eq!(wrapped.len(), 1);
        let bare = split_output(r#"[{"canvas_id": "1", "deadlines": []}]"#).unwrap();
        assert_eq!(bare.len(), 1);
        assert!(split_output(r#"{"announcements": "none"}"#).is_err());
    }

    /// The prompt lists only unread notices, names them for the job, and
    /// fills every placeholder; a listed notice the answer leaves out is
    /// stamped read all the same, so it is offered once.
    #[test]
    fn the_prompt_carries_the_unread_notices_alone() {
        let conn = crate::db::memory_db();
        let read = notice(&conn, 1, "1", "Read already");
        conn.execute("UPDATE announcements SET scanned_at = 1 WHERE id = ?1", [read]).unwrap();
        assert!(build_prompt(&conn, 1, "2026-09-08").unwrap().is_none());
        let unread = notice(&conn, 1, "2", "Hipergator access");
        let (prompt, listed) = build_prompt(&conn, 1, "2026-09-08").unwrap().unwrap();
        assert_eq!(listed, vec![unread]);
        assert_eq!(stamp_listed(&conn, 1, &listed).unwrap(), 1);
        assert_eq!(stamp_listed(&conn, 1, &listed).unwrap(), 0);
        assert!(build_prompt(&conn, 1, "2026-09-08").unwrap().is_none());
        assert!(prompt.contains("canvas_id: 2"));
        assert!(!prompt.contains("Read already"));
        assert!(prompt.contains("Fall 2026"));
        assert!(!prompt.contains('{') || !prompt.contains("{class}"));
        for placeholder in ["{class}", "{today}", "{semester}", "{existing}", "{announcements}"] {
            assert!(!prompt.contains(placeholder), "{placeholder} left in the prompt");
        }
    }
}
