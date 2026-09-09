//! SPEC §8.6 — the homework brief: one document per assignment that maps it
//! to where it was taught — the divisions in its window, their notes, their
//! extracts and what the professor flagged — with anchors and citations, and
//! never solves it. Written from a deadline's row, or by the shift for an
//! assignment due within a few days (SPEC §6). Scoped `brief:<deadline id>`
//! in `guides`, so a retitled assignment keeps its brief and the honest
//! manifest reads stale when its sources change.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{bail, Context, Result};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::AppHandle;

use crate::db::{with_conn, BRIEFS_DIR, BRIEF_SCOPE_PREFIX};
use crate::extract::ManifestEntry;
use crate::guides::{md_twin, synthesis_context, DocumentPayload};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/assignment_brief.md");
pub const KIND: &str = "assignment_brief";
/// How far ahead the shift writes a brief (SPEC §6): an assignment due
/// within this many days of the night gets one.
pub(crate) const DAYS_AHEAD: i64 = 5;
/// A description in the prompt; the longest on record is under 1 KB.
const MAX_DESCRIPTION_CHARS: usize = 4000;

pub(crate) fn brief_scope(deadline_id: i64) -> String {
    format!("{BRIEF_SCOPE_PREFIX}{deadline_id}")
}

/// The deadline is leaving the list — deleted, folded into its Canvas row,
/// or its creation undone — and its brief goes with it: `deadlines.id` is a
/// plain rowid SQLite reissues, so a brief left behind would answer for the
/// next assignment under that id. The row leaves inside the caller's
/// transaction; the files are the caller's to remove after the commit.
pub(crate) fn forget(conn: &Connection, class_id: i64, deadline_id: i64) -> Result<Vec<String>> {
    crate::guides::forget_document(conn, class_id, &brief_scope(deadline_id))
}

/// The kinds a brief is written for: the work that is not an exam.
pub(crate) fn briefable(kind: &str) -> bool {
    matches!(kind, "assignment" | "project")
}

/// The family a title belongs to: its assignment key with every number
/// dropped, so `Homework 2` follows `Homework #1` and Canvas's `Homework
/// Assignment 1`, and a fourth `AI Design Project Presentations` follows the
/// third. The window is keyed on this rather than on any earlier row of the
/// kind, since the Canvas sync types a quiz `assignment` and a homework's
/// window is what was taught since the last homework.
pub(crate) fn title_family(title: &str) -> String {
    crate::deadlines::assignment_key(title)
        .split(' ')
        .filter(|word| !word.chars().all(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// A due date as the instant it names (SPEC §11): a date-only value is the
/// end of its day.
pub(crate) fn due_datetime(due_at: &str) -> Option<NaiveDateTime> {
    if due_at.len() == 10 {
        return NaiveDate::parse_from_str(due_at, "%Y-%m-%d").ok()?.and_hms_opt(23, 59, 59);
    }
    NaiveDateTime::parse_from_str(due_at, "%Y-%m-%dT%H:%M:%S")
        .or_else(|_| NaiveDateTime::parse_from_str(due_at, "%Y-%m-%dT%H:%M"))
        .ok()
}

/// The divisions an assignment's brief draws on (SPEC §8.6): those whose
/// meeting falls after the previous deadline of the family and on or before
/// the due date; a first of its family takes every division that met by the
/// due date; a window nothing met in takes the last division that met before
/// the due date. An undated division has no meeting to place, and is never
/// in a window. Pure, since the wrong answer is silent — a brief that maps a
/// homework to the wrong weeks reads like a right one.
pub(crate) fn window_units(
    units: &[crate::units::UnitInfo],
    meetings: &[(u32, NaiveTime)],
    previous_due: Option<NaiveDateTime>,
    due: NaiveDateTime,
) -> Vec<crate::units::UnitInfo> {
    let met: Vec<(NaiveDateTime, &crate::units::UnitInfo)> = units
        .iter()
        .filter(|unit| unit.starts_on.is_some())
        .filter_map(|unit| {
            let end = crate::shift::meeting_end(unit.starts_on.as_deref(), meetings, due)?;
            Some((end, unit))
        })
        .filter(|(end, _)| *end <= due)
        .collect();
    let inside: Vec<crate::units::UnitInfo> = met
        .iter()
        .filter(|(end, _)| previous_due.is_none_or(|previous| *end > previous))
        .map(|(_, unit)| (*unit).clone())
        .collect();
    if !inside.is_empty() {
        return inside;
    }
    met.iter()
        .max_by_key(|(end, unit)| (*end, unit.ordinal))
        .map(|(_, unit)| vec![(*unit).clone()])
        .unwrap_or_default()
}

/// One of a class's dated items, as the brief reads it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Assignment {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub due_at: String,
    pub status: String,
    pub notes: Option<String>,
    pub description: Option<String>,
    pub from_canvas: bool,
}

fn class_assignments(conn: &Connection, class_id: i64) -> Result<Vec<Assignment>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, title, kind, due_at, status, notes, description, canvas_assignment_id IS NOT NULL
         FROM deadlines WHERE class_id = ?1 ORDER BY {}, id",
        crate::deadlines::DUE_INSTANT_SQL
    ))?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(Assignment {
                id: row.get(0)?,
                title: row.get(1)?,
                kind: row.get(2)?,
                due_at: row.get(3)?,
                status: row.get(4)?,
                notes: row.get(5)?,
                description: row.get(6)?,
                from_canvas: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The window of one assignment: its divisions, and the earlier deadline of
/// its family the window opens after, where there is one.
pub(crate) struct Window {
    pub assignment: Assignment,
    pub units: Vec<crate::units::UnitInfo>,
    pub previous: Option<Assignment>,
}

pub(crate) fn window_for(conn: &Connection, class_id: i64, deadline_id: i64) -> Result<Window> {
    let all = class_assignments(conn, class_id)?;
    let assignment = all
        .iter()
        .find(|a| a.id == deadline_id)
        .cloned()
        .context("that deadline is no longer on the list")?;
    let due = due_datetime(&assignment.due_at)
        .with_context(|| format!("{} carries no readable due date", assignment.title))?;
    let family = title_family(&assignment.title);
    let previous = all
        .iter()
        .filter(|a| a.id != assignment.id && briefable(&a.kind) && title_family(&a.title) == family)
        .filter_map(|a| due_datetime(&a.due_at).map(|at| (at, a)))
        .filter(|(at, _)| *at < due)
        .max_by_key(|(at, a)| (*at, a.id))
        .map(|(_, a)| a.clone());
    let units = crate::units::list_units(conn, class_id)?;
    let meetings = crate::shift::meeting_ends(conn, class_id)?;
    let previous_due = previous.as_ref().and_then(|p| due_datetime(&p.due_at));
    Ok(Window {
        units: window_units(&units, &meetings, previous_due, due),
        assignment,
        previous,
    })
}

/// The brief's manifest (SPEC §7 step 5): the union of its window's
/// divisions' sets — what `current_manifest` answers for `brief:<id>`.
pub fn window_manifest(conn: &Connection, class_id: i64, deadline_id: i64) -> Result<Vec<ManifestEntry>> {
    let Ok(window) = window_for(conn, class_id, deadline_id) else {
        return Ok(Vec::new());
    };
    let slots = crate::units::week_slots(conn, class_id)?;
    let filed = crate::extract::filed_under_weeks(conn, class_id)?;
    let mut entries = Vec::new();
    for unit in &window.units {
        entries.extend(crate::extract::unit_manifest(
            conn,
            class_id,
            unit.id,
            unit.rel_path.as_deref(),
            &slots,
            &filed,
        )?);
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    entries.dedup_by(|a, b| a.rel_path == b.rel_path);
    Ok(entries)
}

/// Where a brief lands, before its extension: `Study Guides/Briefs/<due date>
/// — <title>`; `guides::document_output_rel` settles the name.
pub(crate) fn output_base(due_at: &str, title: &str) -> String {
    format!(
        "{BRIEFS_DIR}/{} \u{2014} {}",
        due_at.chars().take(10).collect::<String>(),
        crate::units::folder_segment(title)
    )
}

/// The `{window}` block: each division with its meeting date and objectives.
fn window_block(conn: &Connection, window: &Window) -> Result<String> {
    if window.units.is_empty() {
        return Ok("(none — the course publishes no dated division that met before this is due, \
                   so map the assignment to the material listed below and to its own description)"
            .to_string());
    }
    let mut out = String::new();
    for unit in &window.units {
        out.push_str(&format!("- {}", unit.name));
        if let Some(date) = &unit.starts_on {
            out.push_str(&format!(" (week of {date})"));
        }
        out.push('\n');
        let objectives = crate::guides::unit_objectives(conn, unit.id)?;
        for line in objectives {
            out.push_str(&format!("  - objective: {line}\n"));
        }
    }
    Ok(out.trim_end().to_string())
}

/// `Write the brief` (SPEC §8.6): the prompt over the assignment's window,
/// enqueued for the runner.
pub fn write_brief(
    app: &AppHandle,
    class_id: i64,
    deadline_id: i64,
    generated_at_label: &str,
) -> Result<i64> {
    let scope = brief_scope(deadline_id);
    let (class_dir, prompt, payload) = with_conn(app, |conn| {
        if crate::guides::has_active_job(conn, class_id, KIND, &scope)? {
            bail!("a brief for this assignment is already queued or running");
        }
        let window = window_for(conn, class_id, deadline_id)?;
        if !briefable(&window.assignment.kind) {
            bail!(
                "{} is a {} — a brief maps an assignment or a project item; the practice exam is \
                 the quiz's document",
                window.assignment.title,
                window.assignment.kind
            );
        }
        let unit_ids: Vec<i64> = window.units.iter().map(|u| u.id).collect();
        let mut notes = Vec::new();
        let mut listed_apart = BTreeSet::new();
        for unit_id in &unit_ids {
            notes.extend(crate::lectures::corpus_notes(conn, class_id, *unit_id)?);
            listed_apart.extend(crate::lectures::contributing_paths(conn, class_id, *unit_id)?);
        }
        let nothing = format!(
            "nothing to map {} to yet — no file is filed under its window's weeks and no lecture \
             there has been distilled",
            window.assignment.title
        );
        let ctx = synthesis_context(conn, class_id, &scope, listed_apart, false, &nothing)?;
        let output = crate::guides::document_output_rel(
            conn,
            class_id,
            &scope,
            &output_base(&window.assignment.due_at, &window.assignment.title),
        )?;
        let output_md = md_twin(&output);
        let a = &window.assignment;
        let description = match a.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
            Some(text) => crate::db::truncate(text, MAX_DESCRIPTION_CHARS),
            None => "(none on record — Canvas carries no description for this row, or it is the \
                     syllabus's reading; say so in the header and work from the title and the \
                     notes below, never from an invented assignment)"
                .to_string(),
        };
        let previous = match &window.previous {
            Some(p) => format!("{} (due {})", p.title, p.due_at.chars().take(10).collect::<String>()),
            None => "none — this is the first of its kind, so the window opens at the semester's start".to_string(),
        };
        let prompt = PROMPT_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{title}", &a.title)
            .replace("{kind}", &a.kind)
            .replace("{due}", &a.due_at)
            .replace("{description}", &description)
            .replace("{notes}", a.notes.as_deref().unwrap_or("(none)"))
            .replace("{previous}", &previous)
            .replace("{window}", &window_block(conn, &window)?)
            .replace("{files}", &ctx.files_block)
            .replace("{corpus}", &crate::lectures::corpus_block(&notes))
            .replace("{hints}", &crate::guides::hints_for(conn, class_id, Some(&unit_ids))?)
            .replace("{output}", &output)
            .replace("{output_md}", &output_md)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&DocumentPayload {
            scope: scope.clone(),
            rel_path: output,
            md_rel_path: Some(output_md),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        Ok((ctx.class_dir, prompt, payload))
    })?;
    fs::create_dir_all(class_dir.join(BRIEFS_DIR))
        .with_context(|| format!("creating {BRIEFS_DIR}"))?;
    crate::jobs::enqueue_document(app, KIND, class_id, &scope, &prompt, payload)
}

/// An assignment the shift would write a brief for (SPEC §6): open, of a
/// briefable kind, due within `DAYS_AHEAD` of today, with no brief or a
/// stale one, and not failed within the rest.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BriefCandidate {
    pub class_id: i64,
    pub deadline_id: i64,
    pub title: String,
    pub due_at: String,
}

pub(crate) fn candidates(conn: &Connection, today: NaiveDate, now_secs: i64) -> Result<Vec<BriefCandidate>> {
    let last = today + chrono::Days::new(DAYS_AHEAD as u64);
    let mut stmt = conn.prepare(&format!(
        "SELECT d.id, d.class_id, d.title, d.kind, d.due_at FROM deadlines d
         WHERE d.status = 'open' AND d.kind IN ('assignment', 'project')
           AND substr(d.due_at, 1, 10) >= ?1 AND substr(d.due_at, 1, 10) <= ?2
         ORDER BY {}, d.id",
        crate::deadlines::DUE_INSTANT_SQL
    ))?;
    let rows = stmt
        .query_map(params![today.to_string(), last.to_string()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    let mut briefs_by_class: std::collections::HashMap<i64, Vec<crate::guides::GuideInfo>> =
        std::collections::HashMap::new();
    for (id, class_id, title, kind, due_at) in rows {
        if !briefable(&kind) {
            continue;
        }
        let scope = brief_scope(id);
        if crate::shift::recently_failed(conn, KIND, class_id, &scope, now_secs)? {
            continue;
        }
        let briefs = match briefs_by_class.get(&class_id) {
            Some(b) => b,
            None => {
                let listed = crate::guides::guides_of_family(conn, class_id, "brief")?;
                briefs_by_class.entry(class_id).or_insert(listed)
            }
        };
        if briefs.iter().any(|g| g.scope == scope && !g.stale) {
            continue;
        }
        out.push(BriefCandidate { class_id, deadline_id: id, title, due_at });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;
    use crate::db::memory_db;

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn week(id: i64, number: i64, starts_on: Option<&str>) -> crate::units::UnitInfo {
        crate::units::UnitInfo {
            id,
            ordinal: number,
            kind: "week".into(),
            name: format!("Week {number}"),
            number: Some(number),
            rel_path: None,
            starts_on: starts_on.map(Into::into),
            ends_on: None,
            first_week: None,
            last_week: None,
            source: "syllabus".into(),
            materials: None,
        }
    }

    /// The family a title belongs to drops its numbers and reads `hw` and
    /// `assignment` as homework, so a homework follows the previous homework
    /// whatever Canvas or the syllabus called it; a quiz is its own family.
    #[test]
    fn a_title_s_family_drops_its_numbers() {
        assert_eq!(title_family("Homework 2"), "homework");
        assert_eq!(title_family("Homework #1"), "homework");
        assert_eq!(title_family("Homework Assignment 1"), "homework");
        assert_eq!(title_family("AI Design Project Presentations"), "ai design project presentations");
        assert_eq!(title_family("Draft: Methods"), "draft methods");
        assert_ne!(title_family("Conceptual Quiz 1 (Actual quiz)"), title_family("Homework 1"));
        assert!(briefable("assignment") && briefable("project") && !briefable("quiz"));
    }

    /// The window (SPEC §8.6): the divisions that met after the previous
    /// homework and by the due date; a first homework from the start; a
    /// window nothing met in takes the last division that met.
    #[test]
    fn the_window_is_what_was_taught_since_the_previous_homework() {
        // Fundamentals meets Tuesday 16:05–19:05.
        let meetings = vec![(2, NaiveTime::parse_from_str("19:05", "%H:%M").unwrap())];
        let units = vec![
            week(1, 1, Some("2026-08-25")),
            week(2, 2, Some("2026-09-01")),
            week(3, 3, Some("2026-09-08")),
            week(4, 4, Some("2026-09-15")),
            week(5, 5, Some("2026-09-22")),
            week(9, 9, None),
        ];
        // Homework 2 due Sept 21 after Homework 1 due Sept 7: Weeks 3 and 4.
        let ids = |list: Vec<crate::units::UnitInfo>| list.iter().map(|u| u.id).collect::<Vec<_>>();
        assert_eq!(
            ids(window_units(&units, &meetings, Some(at("2026-09-07 23:59")), at("2026-09-21 23:59"))),
            vec![3, 4]
        );
        // The first homework draws on everything that met by its due date.
        assert_eq!(
            ids(window_units(&units, &meetings, None, at("2026-09-14 23:59"))),
            vec![1, 2, 3]
        );
        // Nothing met between a live-coding row on Sept 1 and a homework due
        // Sept 7: the last division that met stands in.
        assert_eq!(
            ids(window_units(&units, &meetings, Some(at("2026-09-01 23:59")), at("2026-09-07 23:59"))),
            vec![2]
        );
        // No meeting at all before the due date: nothing.
        assert!(window_units(&units, &meetings, None, at("2026-08-20 23:59")).is_empty());
        assert_eq!(due_datetime("2026-10-04"), Some(at("2026-10-04 23:59").with_second(59).unwrap()));
        assert_eq!(due_datetime("2026-09-14T23:59"), Some(at("2026-09-14 23:59")));
    }

    /// A brief goes with its deadline (SPEC §8.6): `forget` drops the row and
    /// names both files for the caller to remove; and two documents are never
    /// handed one name — a second scope on a taken base takes the suffix,
    /// while a rewrite keeps its row's own path.
    #[test]
    fn a_brief_goes_with_its_deadline_and_no_two_share_a_file() {
        let conn = memory_db();
        let base = "Study Guides/Briefs/2026-09-14 \u{2014} Homework 1";
        let first = crate::guides::document_output_rel(&conn, 3, &brief_scope(7), base).unwrap();
        assert_eq!(first, format!("{base}.html"));
        crate::guides::upsert_guide(&conn, 3, &brief_scope(7), &first, "[]").unwrap();
        // The syllabus's twin of the same title and day, before the fold.
        let second = crate::guides::document_output_rel(&conn, 3, &brief_scope(8), base).unwrap();
        assert_eq!(second, format!("{base} (2).html"));
        // A queued document claims its name too.
        let payload = serde_json::to_string(&DocumentPayload {
            scope: brief_scope(9),
            rel_path: second.clone(),
            md_rel_path: Some(md_twin(&second)),
            source_manifest: "[]".into(),
        })
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, payload, created_at, owner_pid)
             VALUES ('assignment_brief', 3, ?1, 'queued', ?2, 1, 1)",
            params![brief_scope(9), payload],
        )
        .unwrap();
        assert_eq!(
            crate::guides::document_output_rel(&conn, 3, &brief_scope(10), base).unwrap(),
            format!("{base} (3).html")
        );
        // A rewrite lands on its own row's path, whatever else is taken.
        assert_eq!(crate::guides::document_output_rel(&conn, 3, &brief_scope(7), base).unwrap(), first);

        let files = forget(&conn, 3, 7).unwrap();
        assert_eq!(files, vec![format!("{base}.md"), first.clone()]);
        assert!(forget(&conn, 3, 7).unwrap().is_empty(), "nothing twice");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM guides WHERE scope = ?1", [brief_scope(7)], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    /// Against rows: a quiz the Canvas sync typed `assignment` is not the
    /// previous homework; the first homework's window opens at the start.
    #[test]
    fn a_quiz_row_is_not_the_previous_homework() {
        let conn = memory_db();
        crate::db::set_setting(&conn, "aibhs_root", "/nonexistent/classhub-briefs").unwrap();
        for (number, starts) in [(1, "2026-08-20"), (2, "2026-08-27"), (3, "2026-09-03"), (4, "2026-09-10"), (5, "2026-09-17")] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
                 VALUES (3, ?1, 'week', ?2, ?1, ?3, 'syllabus')",
                params![number, format!("Week {number}"), starts],
            )
            .unwrap();
        }
        for (title, kind, due, status) in [
            ("Conceptual Quiz 1 (Actual quiz)", "assignment", "2026-09-06T23:59", "done"),
            ("Homework Assignment 1", "assignment", "2026-09-14T23:59", "open"),
            ("Homework 2", "assignment", "2026-10-04", "open"),
        ] {
            conn.execute(
                "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
                 VALUES (3, ?1, ?2, ?3, ?4, 'syllabus')",
                params![title, kind, due, status],
            )
            .unwrap();
        }
        let hw1: i64 = conn.query_row("SELECT id FROM deadlines WHERE title = 'Homework Assignment 1'", [], |r| r.get(0)).unwrap();
        let hw2: i64 = conn.query_row("SELECT id FROM deadlines WHERE title = 'Homework 2'", [], |r| r.get(0)).unwrap();
        let first = window_for(&conn, 3, hw1).unwrap();
        assert!(first.previous.is_none(), "the quiz is not a homework");
        assert_eq!(first.units.iter().map(|u| u.number).collect::<Vec<_>>(), vec![Some(1), Some(2), Some(3), Some(4)]);
        let second = window_for(&conn, 3, hw2).unwrap();
        assert_eq!(second.previous.as_ref().map(|p| p.id), Some(hw1));
        assert_eq!(second.units.iter().map(|u| u.number).collect::<Vec<_>>(), vec![Some(5)]);
        assert_eq!(output_base("2026-09-14T23:59", "Homework Assignment 1"), "Study Guides/Briefs/2026-09-14 \u{2014} Homework Assignment 1");

        // The shift's list: due within five days, no brief yet; a done row
        // and one further out are not listed, and a fresh brief clears it.
        let listed = candidates(&conn, NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(), 1_000_000).unwrap();
        assert_eq!(listed.iter().map(|c| c.deadline_id).collect::<Vec<_>>(), vec![hw1]);
        crate::guides::upsert_guide(&conn, 3, &brief_scope(hw1), "Study Guides/Briefs/x.html", "[]").unwrap();
        assert!(candidates(&conn, NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(), 1_000_000).unwrap().is_empty());
    }
}
