//! SPEC §12 — Today: what the app knows about the day, from the tables and
//! nothing else. Each meeting today with its division, what the last shift
//! did with what is still reversible, the announcements since the app was
//! last opened, and every decision waiting — cards, a series, a recording
//! that could not be filed, a meeting with no transcript, a Canvas sign-in.
//! What is due rides the deadlines query the strip already reads.

use anyhow::{Context, Result};
use chrono::{Days, NaiveDate};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::db::{set_setting, setting};

/// The latest launch or focus of the window, and the open before it — what
/// "since the app was last opened" is measured from. A focus within the gap
/// of the last one is the same open: a switch to Finder and back must not
/// reset what counts as new.
const OPENED_AT: &str = "opened_at";
const PREVIOUS_OPENED_AT: &str = "previous_opened_at";
const OPEN_GAP: i64 = 30 * 60;
/// A meeting this many days back with no transcript is still worth a line.
const TRANSCRIPT_LOOKBACK: u64 = 7;
/// How many notices the block lists; past this it says how many more.
const MAX_NOTICES: usize = 12;

/// Stamps a launch or a focus (SPEC §12): on a launch, or a focus after the
/// gap, the last stamp becomes the previous open.
pub fn note_opened(conn: &Connection, now: i64, launch: bool) -> Result<()> {
    let last = setting(conn, OPENED_AT)?.and_then(|v| v.parse::<i64>().ok());
    if let Some(last) = last {
        if launch || now - last >= OPEN_GAP {
            set_setting(conn, PREVIOUS_OPENED_AT, &last.to_string())?;
        }
    }
    set_setting(conn, OPENED_AT, &now.to_string())
}

/// The open before this one, unix seconds; none until the app has been
/// opened twice.
pub(crate) fn previous_open(conn: &Connection) -> Result<Option<i64>> {
    Ok(setting(conn, PREVIOUS_OPENED_AT)?.and_then(|v| v.parse::<i64>().ok()))
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TodayMeeting {
    pub class_id: i64,
    pub class_name: String,
    pub class_color: String,
    pub start_time: String,
    pub end_time: String,
    /// Where the course is today (SPEC §8.5), and the week where the reading
    /// is a filed lecture's.
    pub unit_name: Option<String>,
    pub week: Option<i64>,
    /// A pre-read written for today's meeting (SPEC §8.6), by its scope.
    pub preread_scope: Option<String>,
}

/// What the last shift did, and what of it is still reversible.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Overnight {
    pub night: String,
    pub summary: String,
    pub finished_at: Option<i64>,
    pub stopped_by: Option<String>,
    pub running: bool,
    /// One `Undo` per batch the run wrote that nothing has reversed.
    pub undo: Vec<UndoGroup>,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UndoGroup {
    pub text: String,
    pub audit_ids: Vec<i64>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TodayNotice {
    pub id: i64,
    pub class_id: i64,
    pub class_name: String,
    pub class_color: String,
    pub title: String,
    pub posted_at: String,
    pub actions: Vec<crate::announcements::ActionInfo>,
}

/// One decision waiting somewhere in the app.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    /// None for a line about the app itself — the Canvas sign-in.
    pub class_id: Option<i64>,
    pub class_name: Option<String>,
    pub class_color: Option<String>,
    /// sort | deadlines | recordings | transcript | signin
    pub kind: String,
    pub text: String,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TodaySummary {
    pub meetings: Vec<TodayMeeting>,
    pub overnight: Option<Overnight>,
    /// Unix seconds of the open before this one; the notices are since it.
    pub since: Option<i64>,
    /// The newest since the previous open, capped; `notices_more` counts
    /// the rest, which the workspaces hold.
    pub notices: Vec<TodayNotice>,
    pub notices_more: usize,
    pub waiting: Vec<Waiting>,
}

struct ClassRow {
    id: i64,
    name: String,
    color: String,
}

fn classes(conn: &Connection) -> Result<Vec<ClassRow>> {
    let mut stmt = conn.prepare("SELECT id, display_name, color FROM classes ORDER BY id")?;
    let rows = stmt
        .query_map([], |r| Ok(ClassRow { id: r.get(0)?, name: r.get(1)?, color: r.get(2)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Everything the block reads, for `today` (YYYY-MM-DD).
pub fn summary(conn: &Connection, today: &str) -> Result<TodaySummary> {
    summary_with(conn, today, crate::canvas::remembered_session_kept())
}

/// `summary` with the Canvas session's presence handed in — the one read
/// that touches the Keychain, kept out of the tables' own assembly.
pub(crate) fn summary_with(conn: &Connection, today: &str, canvas_session: bool) -> Result<TodaySummary> {
    let date = NaiveDate::parse_from_str(today, "%Y-%m-%d").context("today is not a date")?;
    let weekday = i64::from(chrono::Datelike::weekday(&date).number_from_monday());
    let classes = classes(conn)?;
    let mut meetings = Vec::new();
    let mut waiting = Vec::new();
    for class in &classes {
        let position = crate::units::current_position(conn, class.id, today)?;
        let mut stmt = conn.prepare(
            "SELECT start_time, end_time FROM meetings WHERE class_id = ?1 AND weekday = ?2
             ORDER BY start_time",
        )?;
        let times = stmt
            .query_map(params![class.id, weekday], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !times.is_empty() {
            let preread = crate::preread::list(conn, class.id, today)?
                .into_iter()
                .find(|p| p.rel_path.is_some() && p.meets_on.as_deref() == Some(today))
                .map(|p| p.scope);
            for (start, end) in times {
                meetings.push(TodayMeeting {
                    class_id: class.id,
                    class_name: class.name.clone(),
                    class_color: class.color.clone(),
                    start_time: start,
                    end_time: end,
                    unit_name: position.as_ref().map(|p| p.unit.name.clone()),
                    week: position.as_ref().and_then(|p| p.week),
                    preread_scope: preread.clone(),
                });
            }
        }
        waiting.extend(class_waiting(conn, class, date)?);
    }
    if !canvas_session {
        waiting.push(Waiting {
            class_id: None,
            class_name: None,
            class_color: None,
            kind: "signin".into(),
            text: "Canvas has no stored sign-in — the shift cannot sync until you sign in from Settings".into(),
        });
    }
    let since = previous_open(conn)?;
    let (notices, notices_more) = match since {
        Some(since) => notices_since(conn, &classes, since)?,
        None => (Vec::new(), 0),
    };
    Ok(TodaySummary {
        meetings,
        overnight: overnight(conn)?,
        notices,
        notices_more,
        since,
        waiting,
    })
}

/// The decisions waiting in one class: files to sort, proposed deadlines
/// with their series, recordings found and not captured, and a meeting of
/// the past week with no transcript filed and no recording waiting.
fn class_waiting(conn: &Connection, class: &ClassRow, today: NaiveDate) -> Result<Vec<Waiting>> {
    let mut out = Vec::new();
    let line = |kind: &str, text: String| Waiting {
        class_id: Some(class.id),
        class_name: Some(class.name.clone()),
        class_color: Some(class.color.clone()),
        kind: kind.into(),
        text,
    };
    let to_sort = crate::sorter::pending_count(conn, class.id)?;
    if to_sort > 0 {
        out.push(line("sort", format!("{} to sort", plural(to_sort, "file", "files"))));
    }
    let queue = crate::deadlines::queue(conn, class.id)?;
    if !queue.proposals.is_empty() {
        let in_series: std::collections::BTreeSet<i64> =
            queue.series.iter().flat_map(|s| s.ids.iter().copied()).collect();
        let singles = queue.proposals.iter().filter(|p| !in_series.contains(&p.id)).count() as i64;
        let mut parts = Vec::new();
        for series in &queue.series {
            parts.push(format!("{} as one series, {} dates", series.stem, series.ids.len()));
        }
        if singles > 0 {
            parts.push(plural(singles, "proposed deadline", "proposed deadlines"));
        }
        out.push(line("deadlines", parts.join(" · ")));
    }
    let recordings = crate::recordings::list_waiting(conn, class.id)?;
    let failed = recordings.iter().filter(|r| r.status == "failed").count() as i64;
    let new = recordings.len() as i64 - failed;
    if new > 0 || failed > 0 {
        let mut parts = Vec::new();
        if new > 0 {
            parts.push(format!("{} found on Zoom, not captured yet", plural(new, "recording", "recordings")));
        }
        if failed > 0 {
            parts.push(format!("{} could not be captured", plural(failed, "recording", "recordings")));
        }
        out.push(line("recordings", parts.join(" · ")));
    }
    if let Some(day) = meeting_without_transcript(conn, class.id, today, &recordings)? {
        out.push(line(
            "transcript",
            format!("{}'s lecture has no transcript yet", day.format("%A")),
        ));
    }
    Ok(out)
}

/// The latest meeting in the past week whose week folder holds no
/// transcript and whose date no waiting recording covers — for a course
/// that dates its weeks, since an undated course's week is only known once
/// a lecture is filed.
fn meeting_without_transcript(
    conn: &Connection,
    class_id: i64,
    today: NaiveDate,
    recordings: &[crate::recordings::RecordingInfo],
) -> Result<Option<NaiveDate>> {
    let earliest = today.checked_sub_days(Days::new(TRANSCRIPT_LOOKBACK)).unwrap_or(today);
    let slots = crate::units::week_slots(conn, class_id)?;
    let latest = slots
        .iter()
        .filter_map(|slot| {
            let met = NaiveDate::parse_from_str(slot.meets_on.as_deref()?, "%Y-%m-%d").ok()?;
            (met >= earliest && met < today).then_some((met, slot))
        })
        .max_by_key(|(met, _)| *met);
    let Some((met, slot)) = latest else {
        return Ok(None);
    };
    if recordings.iter().any(|r| r.recorded_at.starts_with(&met.to_string())) {
        return Ok(None);
    }
    let prefix = format!(
        "{}/{}/%",
        crate::db::WEEKS_DIR,
        slot.folder.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let transcripts: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE class_id = ?1 AND rel_path LIKE ?2 ESCAPE '\\'
           AND lower(rel_path) LIKE '%.md'",
        params![class_id, prefix],
        |r| r.get(0),
    )?;
    Ok((transcripts == 0).then_some(met))
}

/// The announcements posted since `since`, newest first, with their lines —
/// the newest `MAX_NOTICES` and how many more. A stamp no date can be made
/// of lists nothing rather than everything.
fn notices_since(conn: &Connection, classes: &[ClassRow], since: i64) -> Result<(Vec<TodayNotice>, usize)> {
    let Some(since_local) = chrono::DateTime::from_timestamp(since, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%dT%H:%M").to_string())
    else {
        return Ok((Vec::new(), 0));
    };
    let mut out = Vec::new();
    for class in classes {
        for a in crate::canvas_sync::list_announcements(conn, class.id)? {
            if a.posted_at > since_local {
                out.push(TodayNotice {
                    id: a.id,
                    class_id: class.id,
                    class_name: class.name.clone(),
                    class_color: class.color.clone(),
                    title: a.title,
                    posted_at: a.posted_at,
                    actions: a.actions,
                });
            }
        }
    }
    out.sort_by(|a, b| b.posted_at.cmp(&a.posted_at).then(b.id.cmp(&a.id)));
    let more = out.len().saturating_sub(MAX_NOTICES);
    out.truncate(MAX_NOTICES);
    Ok((out, more))
}

/// The last run's line and what of it is still reversible: the files the
/// sync filed, a note the review appended to, each an `Undo` over its audit
/// rows unless one has reversed them already. Only a finished run offers
/// one: a run under way has no end to bound its window, and a move or a
/// note the reader makes while it works would fall inside it.
fn overnight(conn: &Connection) -> Result<Option<Overnight>> {
    let Some(run) = crate::shift::latest_run(conn)? else {
        return Ok(None);
    };
    let undo = match run.finished_at {
        Some(until) => reversible_between(conn, run.started_at, until)?,
        None => Vec::new(),
    };
    Ok(Some(Overnight {
        night: run.night,
        summary: run.summary.unwrap_or_else(|| "running".to_string()),
        running: run.finished_at.is_none(),
        finished_at: run.finished_at,
        stopped_by: run.stopped_by,
        undo,
    }))
}

/// The reversible rows written between two moments, grouped by what they
/// did, less the ones an `undo.*` row already names.
pub(crate) fn reversible_between(conn: &Connection, from: i64, until: i64) -> Result<Vec<UndoGroup>> {
    let mut stmt = conn.prepare(
        "SELECT id, action FROM audit_log
         WHERE created_at BETWEEN ?1 AND ?2
           AND action IN ('canvas.filed', 'sort.move', 'review.write_note')
           AND id NOT IN (SELECT json_extract(payload, '$.auditId') FROM audit_log
                          WHERE action LIKE 'undo.%' AND json_valid(payload)
                            AND json_extract(payload, '$.auditId') IS NOT NULL)
         ORDER BY id",
    )?;
    let rows = stmt
        .query_map(params![from, until], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut filed = Vec::new();
    let mut moved = Vec::new();
    let mut notes = Vec::new();
    for (id, action) in rows {
        match action.as_str() {
            "canvas.filed" => filed.push(id),
            "sort.move" => moved.push(id),
            _ => notes.push(id),
        }
    }
    let mut out = Vec::new();
    if !filed.is_empty() {
        out.push(UndoGroup {
            text: format!("{} where Canvas keeps them", plural(filed.len() as i64, "file filed", "files filed")),
            audit_ids: filed,
        });
    }
    if !moved.is_empty() {
        out.push(UndoGroup {
            text: plural(moved.len() as i64, "file moved", "files moved"),
            audit_ids: moved,
        });
    }
    if !notes.is_empty() {
        out.push(UndoGroup {
            text: plural(notes.len() as i64, "note read against the room", "notes read against the room"),
            audit_ids: notes,
        });
    }
    Ok(out)
}

fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{memory_db, set_setting};
    use serde_json::json;

    /// The open stamps (SPEC §12): a launch rotates the last stamp into the
    /// previous open; a focus within the gap does not; one after it does.
    #[test]
    fn a_launch_or_a_focus_after_the_gap_rotates_the_previous_open() {
        let conn = memory_db();
        assert_eq!(previous_open(&conn).unwrap(), None);
        note_opened(&conn, 1_000, true).unwrap();
        assert_eq!(previous_open(&conn).unwrap(), None, "the first open has no previous");
        note_opened(&conn, 1_100, false).unwrap();
        assert_eq!(previous_open(&conn).unwrap(), None, "a focus within the gap is the same open");
        note_opened(&conn, 1_100 + OPEN_GAP, false).unwrap();
        assert_eq!(previous_open(&conn).unwrap(), Some(1_100));
        note_opened(&conn, 9_000, true).unwrap();
        assert_eq!(previous_open(&conn).unwrap(), Some(1_100 + OPEN_GAP), "a launch always rotates");
    }

    /// What of a run is still reversible: the sync's filings as one batch,
    /// the review's note as another, an undone row left out, a row outside
    /// the run's window left out.
    #[test]
    fn a_runs_reversible_rows_are_grouped_and_the_undone_left_out() {
        let conn = memory_db();
        let audit = |action: &str, at: i64, payload: serde_json::Value| -> i64 {
            conn.execute(
                "INSERT INTO audit_log (action, payload, created_at) VALUES (?1, ?2, ?3)",
                params![action, payload.to_string(), at],
            )
            .unwrap();
            conn.last_insert_rowid()
        };
        let before = audit("canvas.filed", 90, json!({}));
        let a = audit("canvas.filed", 100, json!({}));
        let b = audit("canvas.filed", 110, json!({}));
        let note = audit("review.write_note", 120, json!({}));
        let undone = audit("review.write_note", 130, json!({}));
        audit("undo.review.write_note", 140, json!({ "auditId": undone }));
        audit("ui.set_deadline_status", 150, json!({}));
        let groups = reversible_between(&conn, 100, 200).unwrap();
        assert_eq!(
            groups,
            vec![
                UndoGroup { text: "2 files filed where Canvas keeps them".into(), audit_ids: vec![a, b] },
                UndoGroup { text: "1 note read against the room".into(), audit_ids: vec![note] },
            ]
        );
        assert!(!groups.iter().any(|g| g.audit_ids.contains(&before)));
        assert!(reversible_between(&conn, 300, 400).unwrap().is_empty());
    }

    /// The block assembled (SPEC §12): the day's meeting with its division,
    /// the last run's line with an `Undo` only once it has finished, the
    /// notices since the previous open and no older ones, capped with a
    /// count of the rest, and the waiting lines — a file to sort, a
    /// proposed deadline, a recording, the sign-in.
    #[test]
    fn the_block_lists_the_meeting_the_run_the_notices_and_what_waits() {
        let conn = memory_db();
        set_setting(&conn, "aibhs_root", "/nonexistent/classhub-today").unwrap();
        // Design Studio meets Wednesdays; Sept 9, 2026 is one, in Week 3.
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (2, 3, 'week', 'Week 3 — HiPerGator', 3, '2026-09-09', 'syllabus')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO deadline_proposals (class_id, title, kind, due_at, status, source, created_at)
             VALUES (1, 'Homework 2', 'assignment', '2026-09-21', 'pending', 'syllabus', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (class_id, meeting_id, recorded_at, duration_minutes, title, status, seen_at)
             VALUES (3, 'm1', '2026-09-03T11:40', 120, 'Lecture', 'new', 1)",
            [],
        )
        .unwrap();
        for (class_id, canvas_id, title, posted_at) in [
            (3, "a1", "Quiz 1 opened", "2026-09-05T16:13"),
            (2, "a2", "Office hours moved", "2026-09-04T15:03"),
            (3, "a3", "Welcome", "2026-08-20T00:00"),
        ] {
            conn.execute(
                "INSERT INTO announcements (class_id, canvas_id, title, body, posted_at)
                 VALUES (?1, ?2, ?3, '', ?4)",
                params![class_id, canvas_id, title, posted_at],
            )
            .unwrap();
        }
        // The previous open: Sept 1 at noon, local.
        let since = chrono::NaiveDate::from_ymd_opt(2026, 9, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(chrono::Local)
            .single()
            .unwrap()
            .timestamp();
        set_setting(&conn, "previous_opened_at", &since.to_string()).unwrap();
        let run = crate::shift::insert_run(&conn, "2026-09-08", "idle").unwrap().unwrap();
        let (started_at, _): (i64, i64) = conn
            .query_row("SELECT started_at, id FROM shift_runs WHERE id = ?1", [run], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        conn.execute(
            "INSERT INTO audit_log (action, payload, created_at) VALUES ('canvas.filed', '{}', ?1)",
            [started_at],
        )
        .unwrap();

        let summary = summary_with(&conn, "2026-09-09", false).unwrap();
        assert_eq!(summary.meetings.len(), 1);
        let meeting = &summary.meetings[0];
        assert_eq!((meeting.class_id, meeting.start_time.as_str()), (2, "17:10"));
        assert_eq!(meeting.unit_name.as_deref(), Some("Week 3 — HiPerGator"));
        assert_eq!(meeting.week, None);
        // The run is under way: its line, and no Undo yet.
        let overnight = summary.overnight.as_ref().expect("a run");
        assert!(overnight.running);
        assert!(overnight.undo.is_empty(), "a running run offers no Undo");
        assert_eq!(summary.since, Some(since));
        let titles: Vec<&str> = summary.notices.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, vec!["Quiz 1 opened", "Office hours moved"], "newest first, the old one out");
        assert_eq!(summary.notices_more, 0);
        let kinds: Vec<(Option<i64>, &str)> =
            summary.waiting.iter().map(|w| (w.class_id, w.kind.as_str())).collect();
        assert_eq!(
            kinds,
            vec![(Some(1), "deadlines"), (Some(3), "recordings"), (None, "signin")]
        );
        assert_eq!(summary.waiting[0].text, "1 proposed deadline");
        assert_eq!(summary.waiting[1].text, "1 recording found on Zoom, not captured yet");

        // Finished: the filing it wrote is one Undo; a stored session drops
        // the sign-in line; the notices past the cap are counted.
        conn.execute(
            "UPDATE shift_runs SET finished_at = ?1, summary = '1 file filed', stopped_by = 'done' WHERE id = ?2",
            params![started_at + 30, run],
        )
        .unwrap();
        for i in 0..MAX_NOTICES + 2 {
            conn.execute(
                "INSERT INTO announcements (class_id, canvas_id, title, body, posted_at)
                 VALUES (1, ?1, ?2, '', '2026-09-06T10:00')",
                params![format!("b{i}"), format!("Notice {i}")],
            )
            .unwrap();
        }
        let summary = summary_with(&conn, "2026-09-09", true).unwrap();
        let overnight = summary.overnight.as_ref().expect("a run");
        assert!(!overnight.running);
        assert_eq!(overnight.summary, "1 file filed");
        assert_eq!(overnight.undo.len(), 1);
        assert_eq!(overnight.undo[0].text, "1 file filed where Canvas keeps them");
        assert!(summary.waiting.iter().all(|w| w.kind != "signin"));
        assert_eq!(summary.notices.len(), MAX_NOTICES);
        assert_eq!(summary.notices_more, 4);
        assert!(summary_with(&conn, "today", true).is_err(), "not a date");
    }

    /// The transcript line: the past week's meeting with nothing filed under
    /// its week folder and no recording waiting for it.
    #[test]
    fn a_meeting_of_the_past_week_with_no_transcript_is_named() {
        let conn = memory_db();
        for (n, name, on) in [
            (2, "Week 2 — Ethics", "2026-09-01"),
            (3, "Week 3 — Data", "2026-09-08"),
            (4, "Week 4 — Models", "2026-09-15"),
        ] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
                 VALUES (1, ?1, 'week', ?2, ?1, ?3, 'syllabus')",
                params![n, name, on],
            )
            .unwrap();
        }
        let today = NaiveDate::from_ymd_opt(2026, 9, 9).unwrap();
        assert_eq!(
            meeting_without_transcript(&conn, 1, today, &[]).unwrap(),
            Some(NaiveDate::from_ymd_opt(2026, 9, 8).unwrap())
        );
        // A recording waiting for that day is its own line.
        let waiting = crate::recordings::RecordingInfo {
            id: 1,
            class_id: 1,
            title: "Lecture".into(),
            recorded_at: "2026-09-08T16:05".into(),
            duration_minutes: 180,
            status: "new".into(),
            note: None,
        };
        assert_eq!(meeting_without_transcript(&conn, 1, today, &[waiting]).unwrap(), None);
        // A transcript filed under the week folder settles it.
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (1, 'Weeks/Week 03 — Data/2026-09-08 — Lecture.md', 'x', 1, 1, 'md')",
            [],
        )
        .unwrap();
        assert_eq!(meeting_without_transcript(&conn, 1, today, &[]).unwrap(), None);
        // Past the lookback, nothing.
        let later = NaiveDate::from_ymd_opt(2026, 9, 25).unwrap();
        assert_eq!(meeting_without_transcript(&conn, 1, later, &[]).unwrap(), None);
        // Week 4 met Sept 15 with nothing filed: named on the 16th.
        let sixteenth = NaiveDate::from_ymd_opt(2026, 9, 16).unwrap();
        assert_eq!(
            meeting_without_transcript(&conn, 1, sixteenth, &[]).unwrap(),
            Some(NaiveDate::from_ymd_opt(2026, 9, 15).unwrap())
        );
    }
}
