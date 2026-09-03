//! SPEC §7.2 — what a Canvas sync actually does with the session `canvas.rs`
//! establishes.
//!
//! Reads only, and never on a timer. Four things come across:
//!
//! 1. **The course's own divisions** → `units` (SPEC §5), from its published
//!    modules. Today none of the four courses publishes any, so this reads an
//!    empty list and says so — the syllabus source in §7.2's precedence chain
//!    is what supplies them instead. The reader is written anyway because it
//!    costs one request and starts working the day a professor adds a module.
//! 2. **Assignments** → the existing `deadline_proposals` confirm queue, with
//!    Canvas's true due dates rather than a syllabus PDF's prose. A deadline
//!    already on the list is tracked by the assignment's id: its due date
//!    follows Canvas, and a submission closes it.
//! 3. **Grades** (SPEC §11) → assignment groups become `grade_categories` and
//!    graded, posted submissions become `grade_items`, written directly with
//!    audit rows because a grade is reversible in the Grades section.
//! 4. **Announcements** → `announcements` (SPEC §5), what the professor said
//!    between lectures: a quiz moved, slides posted. Stripped to text, never
//!    rendered as Canvas's HTML; a record, not a queue.
//! 5. **Pages and the syllabus page** → markdown under
//!    `.classhub/extracts/Canvas/`, where `search_material` already looks and
//!    the syllabus scan's picker can offer them. Not `files` rows: nothing on
//!    disk is their source, so they take no part in a guide's manifest.
//! 6. **Course files** → downloaded into `_Inbox/` and proposed through the
//!    §10 move queue, because approval is what places a file, here as
//!    everywhere.
//!
//! Nothing is deleted. A unit that disappears from Canvas is kept (a
//! mid-semester reshuffle must not orphan a guide), and a re-sync updates in
//! place rather than duplicating — every row Canvas wrote carries the Canvas
//! id it came from, which is what makes the second sync a no-op.
//!
//! Never on a timer, but once on launch (SPEC §7.2): when a session is stored
//! and the last sync is a day old, the launch reads Canvas through a window
//! that stays hidden and never asks for a sign-in — Canvas refusing the stored
//! session is a line in the report, and the next manual sync asks.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::canvas::{Session, SignInNeeded};
use crate::db::{audit, emit_hub_change, lock, now, with_conn, write_atomic, EXTRACTS_DIR, INBOX_DIR};
use crate::extract::strip_html;
use crate::deadlines::{CanvasAssignment, Recorded};
use crate::grades::{CanvasGroup, CanvasScore, CanvasWrite};
use crate::units::{self, NewUnit};

pub const PROGRESS_EVENT: &str = "canvas://progress";

/// Only classes with a real course code are matched, and only exactly. A wrong
/// mapping files another class's material into this one, so an ambiguous or
/// missing match is reported rather than guessed at.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ClassOutcome {
    pub class_id: i64,
    pub class_name: String,
    /// The Canvas course this class matched, for confirmation rather than trust.
    pub canvas_course: Option<String>,
    pub units_added: usize,
    pub deadlines_proposed: usize,
    /// Open deadlines closed because Canvas holds a submission for them.
    pub deadlines_completed: usize,
    pub files_staged: usize,
    /// Grade items written or updated from graded, posted submissions.
    pub grades_recorded: usize,
    /// Announcements recorded or updated — what the workspace's NOTICES gained.
    pub announcements_recorded: usize,
    /// Canvas Pages and the syllabus page written or rewritten into the
    /// extract cache.
    pub pages_written: usize,
    /// Plain lines about what Canvas did and did not have. A course that
    /// publishes no modules is the normal case right now, and silence about it
    /// would read as "synced, nothing to do".
    pub notes: Vec<String>,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress {
    stage: String,
    done: bool,
    /// Started by the launch rather than by a press; the report says so.
    launch: bool,
    /// Ended because Canvas wants a sign-in the sync was not allowed to ask
    /// for. The one failure the report renders as a note rather than a
    /// stopped sync — its own flag, so a launch sync that failed for any
    /// other reason still reads as one.
    sign_in_needed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<ClassOutcome>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One sync at a time. The window is a single shared resource, and two syncs
/// would fight over it and over the inbox destination names.
static SYNCING: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// How stale the last sync has to be for a launch to run one.
const LAUNCH_SYNC_AFTER: i64 = 24 * 60 * 60;

/// Whether a launch should sync (SPEC §7.2): nothing has ever synced, or the
/// last sync is a day old. Pure, since the wrong answer is silent either way
/// — a sync on every launch, or never one.
fn launch_sync_due(last_synced_at: Option<i64>, now: i64) -> bool {
    match last_synced_at {
        None => true,
        Some(then) => now - then >= LAUNCH_SYNC_AFTER,
    }
}

/// The sync a launch runs on its own, when it should: a stored session and a
/// day-old last sync. Quiet throughout — the window stays hidden, nothing
/// asks for a sign-in, and a refusal is a line in the Settings report. Not a
/// timer: this runs once, here, and nothing schedules a second one.
pub fn sync_on_launch(app: &AppHandle) {
    let last_synced_at = match with_conn(app, |conn| status(conn)) {
        Ok(status) => status.last_synced_at,
        Err(e) => {
            eprintln!("canvas: launch sync skipped — {e:#}");
            return;
        }
    };
    if !launch_sync_due(last_synced_at, now()) {
        return;
    }
    // The Keychain read comes second, so a launch inside the day never
    // touches the Keychain at all.
    if !crate::canvas::has_remembered_session() {
        return;
    }
    if let Err(e) = spawn_with(app, Vec::new(), true) {
        eprintln!("canvas: launch sync not started — {e:#}");
    }
}

/// Runs a sync on its own thread, reporting over `PROGRESS_EVENT`.
///
/// A plain thread rather than an async command, for the same reason
/// `lectures::spawn_add` uses one: this waits on a human completing SSO and
/// then on blocking HTTP, neither of which belongs on the async runtime.
pub fn spawn(app: &AppHandle, class_ids: Vec<i64>) -> Result<()> {
    spawn_with(app, class_ids, false)
}

/// `launch` is the sync nobody pressed: the window stays hidden and Canvas
/// wanting a sign-in ends it rather than asking.
fn spawn_with(app: &AppHandle, class_ids: Vec<i64>, launch: bool) -> Result<()> {
    // Refused here rather than reported over the progress channel. That channel
    // carries one snapshot, so a "already running" terminal event would
    // overwrite the running sync's own progress and render as SYNC STOPPED for
    // a sync that is still going.
    {
        let mut busy = lock(&SYNCING);
        if *busy {
            bail!("a Canvas sync is already running");
        }
        *busy = true;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let emit = |stage: &str, done: bool, results: Option<Vec<ClassOutcome>>, error: Option<String>| {
            let _ = app.emit(
                PROGRESS_EVENT,
                Progress {
                    stage: stage.to_string(),
                    done,
                    launch,
                    sign_in_needed: false,
                    results,
                    error,
                },
            );
        };
        // Claimed above, released here however this thread ends.
        struct Claim;
        impl Drop for Claim {
            fn drop(&mut self) {
                *lock(&SYNCING) = false;
            }
        }
        let _claim = Claim;

        let on_stage = |stage: &str| emit(stage, false, None, None);
        let finished = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run(&app, &class_ids, &on_stage, launch)
        }));
        match finished {
            Ok(Ok(results)) => {
                emit("Synced", true, Some(results), None);
                emit_hub_change(&app, "units");
            }
            // The one outcome a launch sync expects: the stored session was
            // turned down. Flagged as such, so the report can say so quietly.
            Ok(Err(e)) if e.chain().any(|cause| cause.is::<SignInNeeded>()) => {
                let _ = app.emit(
                    PROGRESS_EVENT,
                    Progress {
                        stage: "Not synced".to_string(),
                        done: true,
                        launch,
                        sign_in_needed: true,
                        results: None,
                        error: Some(format!("{SignInNeeded}")),
                    },
                );
            }
            Ok(Err(e)) => emit("Failed", true, None, Some(format!("{e:#}"))),
            // The claim releases during the unwind, so the backend recovers —
            // but without a terminal event `done` stays false forever, and both
            // screens derive their button state from it. The backend would be
            // fine and the UI would have no way back short of a relaunch.
            Err(_) => emit(
                "Failed",
                true,
                None,
                Some("the sync stopped unexpectedly — nothing was left half-written".into()),
            ),
        }
    });
    Ok(())
}

/// The sync proper. `class_ids` empty means every class; `quiet` is the
/// launch's sync, which never shows the window.
fn run(
    app: &AppHandle,
    class_ids: &[i64],
    on_stage: &dyn Fn(&str),
    quiet: bool,
) -> Result<Vec<ClassOutcome>> {
    let classes = with_conn(app, |conn| load_classes(conn, class_ids))?;
    if classes.is_empty() {
        bail!("no classes to sync");
    }

    on_stage("Opening Canvas…");
    // Whatever happens below, the window closes when `session` goes out of
    // scope: it *is* the app's reach, so leaving it open would leave the reach
    // open. `Session`'s own `Drop` is what guarantees that on every path,
    // including the ones where opening it is what failed.
    let session = if quiet {
        Session::open_quiet(app, on_stage)?
    } else {
        Session::open(app, on_stage)?
    };

    on_stage("Reading your courses…");
    // The syllabus page rides on the course listing: `include[]=syllabus_body`
    // puts each course's syllabus HTML on its own object, which costs no
    // request per course.
    let courses = session.get_all(
        "/api/v1/courses?enrollment_state=active&include[]=syllabus_body",
        on_stage,
    )?;

    let mut results = Vec::new();
    for class in classes {
        on_stage(&format!("Syncing {}…", class.display_name));
        let mut outcome = ClassOutcome {
            class_id: class.id,
            class_name: class.display_name.clone(),
            canvas_course: None,
            units_added: 0,
            deadlines_proposed: 0,
            deadlines_completed: 0,
            files_staged: 0,
            grades_recorded: 0,
            announcements_recorded: 0,
            pages_written: 0,
            notes: Vec::new(),
            error: None,
        };
        // One class's failure costs that class, never the rest of the sync —
        // except the session lapsing under a quiet sync, which is the sync's
        // own outcome: the stored copy is already gone, every later class
        // would fail the same way, and the report should say what happened
        // once rather than in red per class.
        if let Err(e) = sync_class(app, &session, &class, &courses, &mut outcome, on_stage) {
            if e.chain().any(|cause| cause.is::<SignInNeeded>()) {
                return Err(e);
            }
            outcome.error = Some(format!("{e:#}"));
        }
        results.push(outcome);
    }
    Ok(results)
}

struct ClassRow {
    id: i64,
    display_name: String,
    code: String,
}

fn load_classes(conn: &Connection, class_ids: &[i64]) -> Result<Vec<ClassRow>> {
    let mut stmt = conn.prepare("SELECT id, display_name, code FROM classes ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ClassRow {
                id: row.get(0)?,
                display_name: row.get(1)?,
                code: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|c| class_ids.is_empty() || class_ids.contains(&c.id))
        .collect())
}

fn sync_class(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    courses: &[Value],
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let course = match_course(class, courses)?;
    let course_id = course["id"].as_i64().context("the Canvas course has no id")?;
    let course_name = course["name"].as_str().unwrap_or(&class.code).to_string();
    outcome.canvas_course = Some(course_name.clone());

    // The mapping is established the moment `match_course` succeeds, and being
    // able to show which course a class resolved to is the whole point of
    // storing it.
    with_conn(app, |conn| {
        conn.execute(
            "UPDATE classes SET canvas_course_id = ?1 WHERE id = ?2",
            params![course_id.to_string(), class.id],
        )?;
        Ok(())
    })?;

    if let Err(e) = sync_units(app, session, class, course_id, outcome, on_stage) {
        note_or_fail(outcome, e, "modules")?;
    }
    // One read of the assignments, submissions included, feeds both the
    // deadline pass and the grade pass; the groups the grades sit under are
    // one more request inside the second.
    match fetch_assignments(session, course_id, on_stage) {
        Ok(assignments) => {
            if let Err(e) = sync_assignments(app, class, &assignments, outcome) {
                note_or_fail(outcome, e, "assignments")?;
            }
            // One extra request's worth of trouble, and the file sync behind
            // it does not depend on it — so a failure here is a line in the
            // report, never the class's failure and never a skipped file sync.
            if let Err(e) =
                sync_grades(app, session, class, course, &assignments, outcome, on_stage)
            {
                note_or_carry_on(outcome, e, "grades")?;
            }
        }
        Err(e) => note_or_fail(outcome, e, "assignments")?,
    }
    // Before the files, which are the slow read and the one that downloads:
    // a deck that will not come across must not cost the notices. And like
    // the grades, neither read is worth the class — a failure here is a line
    // in the report, and the file sync and the sync stamp still follow.
    if let Err(e) = sync_announcements(app, session, class, course_id, outcome, on_stage) {
        note_or_carry_on(outcome, e, "announcements")?;
    }
    if let Err(e) = sync_pages(app, session, class, course, outcome, on_stage) {
        note_or_carry_on(outcome, e, "pages")?;
    }
    if let Err(e) = sync_files(app, session, class, course_id, outcome, on_stage) {
        note_or_fail(outcome, e, "files")?;
    }

    // Stamped only now. Written alongside the mapping it claimed a sync that
    // had not happened yet: all three reads can fail, the caller records that
    // per class and carries on, and Settings would still have shown a fresh
    // LAST SYNC for a class that read nothing.
    with_conn(app, |conn| {
        conn.execute(
            "UPDATE classes SET canvas_synced_at = ?1 WHERE id = ?2",
            params![now(), class.id],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// A course refusing one of its tabs is a setting, not a failed sync.
///
/// A professor can disable Files or Modules for a course; Canvas then answers
/// 401 for that collection while the session stays demonstrably live. That is
/// worth a line, and the other two reads still count — failing the class over
/// it would throw away the assignments that did come across.
fn note_or_fail(outcome: &mut ClassOutcome, error: anyhow::Error, what: &str) -> Result<()> {
    if error.chain().any(|cause| cause.is::<crate::canvas::Refused>()) {
        outcome
            .notes
            .push(format!("Canvas will not share this course's {what} — {error}"));
        return Ok(());
    }
    Err(error)
}

/// A pass that is not worth the class: a refusal is a line, and any other
/// failure is a line saying the read did not happen this time, so the reads
/// after it and the sync stamp still follow. The one exception is a sign-in
/// the session needs — that is the sync's to answer, not one class's line.
fn note_or_carry_on(outcome: &mut ClassOutcome, error: anyhow::Error, what: &str) -> Result<()> {
    if error.chain().any(|cause| cause.is::<SignInNeeded>()) {
        return Err(error);
    }
    if let Err(e) = note_or_fail(outcome, error, what) {
        outcome
            .notes
            .push(format!("Canvas {what} were not read this time — {e:#}"));
    }
    Ok(())
}

/// Maps a `classes` row to a Canvas course by course code, exactly.
///
/// The account is enrolled in twelve "active" courses — orientation shells and
/// non-credit org sites from years back sit alongside this semester's four —
/// so the match has to be narrow. A code is unique and machine-assigned, which
/// makes it the one field worth trusting; a name match would have to survive
/// "CAI5724- AI in Health Design Studio I" against "AI in Health Design
/// Studio I", and near-misses are precisely what must not be guessed at.
fn match_course<'a>(class: &ClassRow, courses: &'a [Value]) -> Result<&'a Value> {
    let wanted = class.code.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        bail!("{} has no course code to match on", class.display_name);
    }
    let matches: Vec<&Value> = courses
        .iter()
        .filter(|c| {
            c["course_code"]
                .as_str()
                .map(|code| code.trim().to_ascii_lowercase() == wanted)
                .unwrap_or(false)
        })
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => bail!(
            "no active Canvas course has the code {} — check the enrollment is current",
            class.code
        ),
        n => bail!("{n} active Canvas courses share the code {} — not guessing", class.code),
    }
}

// ---------------------------------------------------------------------------
// Units (SPEC §5) — the course's published modules

fn sync_units(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course_id: i64,
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let path = format!("/api/v1/courses/{course_id}/modules?include[]=items");
    let modules = session.get_all(&path, on_stage)?;
    if modules.is_empty() {
        // Said plainly rather than passed over. Canvas holding no structure is
        // the difference between "ground truth" and "the syllabus is all there
        // is", and the reader has no other way to tell those apart.
        outcome
            .notes
            .push("Canvas publishes no modules for this course — units come from the syllabus".into());
        return Ok(());
    }

    let (added, skipped) = with_conn(app, |conn| {
        let mut added = 0usize;
        let mut skipped: Vec<String> = Vec::new();
        for (index, module) in modules.iter().enumerate() {
            let Some(name) = module["name"].as_str().map(str::trim).filter(|n| !n.is_empty())
            else {
                continue;
            };
            let unit = NewUnit {
                // Canvas's own `position` when it has one, else the order it
                // listed them in.
                ordinal: module["position"].as_i64().unwrap_or(index as i64 + 1),
                kind: units::kind_for_name(name).to_string(),
                name: name.to_string(),
                canvas_id: module["id"].as_i64().map(|id| id.to_string()),
                rel_path: None,
                starts_on: iso_date(module["unlock_at"].as_str()),
                ends_on: None,
                source: "canvas",
            };
            match units::upsert(conn, class.id, &unit) {
                Ok(true) => added += 1,
                Ok(false) => {}
                // Counted rather than only printed: a module that did not land
                // shows up as a lower total, which is indistinguishable from
                // Canvas having published less.
                Err(e) => {
                    eprintln!("canvas: skipping module '{name}': {e:#}");
                    skipped.push(format!("{name} ({e})"));
                }
            }
        }
        Ok((added, skipped))
    })?;
    outcome.units_added = added;
    if !skipped.is_empty() {
        outcome.notes.push(format!(
            "{} module(s) could not be recorded: {}",
            skipped.len(),
            skipped.join("; ")
        ));
    }
    Ok(())
}

/// The date half of a Canvas timestamp, in the local zone.
fn iso_date(utc: Option<&str>) -> Option<String> {
    local_iso(utc?).map(|s| s[..10].to_string())
}

/// A Canvas UTC timestamp as local wall-clock ISO, `YYYY-MM-DDTHH:MM`.
///
/// The conversion is load-bearing, not cosmetic. Canvas returns
/// `2026-08-26T03:59:59Z` for an assignment the syllabus calls "due 11:59 PM
/// on the 25th": stored as its UTC date it would show up a day late. And a
/// fixed offset would not do either — one of this semester's two assignments
/// falls in EDT and the other in EST, so the offset that fixes one breaks the
/// other by an hour, which for a 23:59 deadline is a whole day.
fn local_iso(utc: &str) -> Option<String> {
    let parsed = chrono::DateTime::parse_from_rfc3339(utc).ok()?;
    Some(
        parsed
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%dT%H:%M")
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// Assignments → the deadline confirm queue (SPEC §11), and the deadlines
// already on the list that Canvas tracks

/// The course's assignments, each with the reader's own submission — the one
/// read the deadline pass and the grade pass both consume.
fn fetch_assignments(
    session: &Session,
    course_id: i64,
    on_stage: &dyn Fn(&str),
) -> Result<Vec<Value>> {
    let path = format!("/api/v1/courses/{course_id}/assignments?include[]=submission");
    session.get_all(&path, on_stage)
}

/// Brings the deadline list up to date with the course's assignments.
///
/// An assignment that already has a deadline — approved from an earlier card,
/// or a syllabus row on the same title and day, which takes the assignment's id
/// on first contact — is brought up to date in place: Canvas's due date, and
/// done once a submission exists. Everything else is proposed through the
/// confirm queue as before.
fn sync_assignments(
    app: &AppHandle,
    class: &ClassRow,
    assignments: &[Value],
    outcome: &mut ClassOutcome,
) -> Result<()> {
    let (proposed, undated, known, completed, moved) = with_conn(app, |conn| {
        let mut proposed = 0usize;
        let mut undated = 0usize;
        let mut known = 0usize;
        let mut completed = 0usize;
        let mut moved = 0usize;
        for assignment in assignments {
            let Some(title) = assignment["name"].as_str().map(str::trim).filter(|n| !n.is_empty())
            else {
                continue;
            };
            let due_at = assignment["due_at"].as_str().and_then(local_iso);
            let canvas_id = assignment["id"].as_i64().map(|id| id.to_string());
            // Settled before the due-date guard below: a deadline the list
            // already holds is tracked by its id whether or not Canvas dates
            // the assignment, and its submission closes it either way.
            if let Some(canvas_id) = canvas_id.as_deref() {
                let submitted_at = assignment["submission"]["submitted_at"]
                    .as_str()
                    .and_then(local_iso);
                let tracked = CanvasAssignment {
                    id: canvas_id,
                    title,
                    due_at: due_at.as_deref(),
                    submitted_at: submitted_at.as_deref(),
                };
                match crate::deadlines::settle_canvas_deadline(conn, class.id, &tracked) {
                    Ok(Some(settled)) => {
                        known += 1;
                        if settled.completed {
                            completed += 1;
                        }
                        if settled.due_moved {
                            moved += 1;
                        }
                        continue;
                    }
                    Ok(None) => {}
                    Err(e) => eprintln!("canvas: could not settle '{title}': {e:#}"),
                }
            }
            // An assignment with no due date is real but not a deadline. It
            // would have to be invented to store one, which is exactly what
            // reading Canvas was supposed to stop.
            let Some(due_at) = due_at else {
                undated += 1;
                continue;
            };
            let kind = if assignment["is_quiz_assignment"].as_bool().unwrap_or(false)
                || assignment["submission_types"]
                    .as_array()
                    .map(|t| t.iter().any(|v| v.as_str() == Some("online_quiz")))
                    .unwrap_or(false)
            {
                "quiz"
            } else {
                "assignment"
            };
            let notes = assignment["points_possible"]
                .as_f64()
                .filter(|p| *p > 0.0)
                .map(|p| format!("From Canvas · {} points", trim_number(p)))
                .unwrap_or_else(|| "From Canvas".to_string());

            match crate::deadlines::record_proposal(
                conn,
                class.id,
                title,
                kind,
                &due_at,
                Some(&notes),
                "canvas",
                canvas_id.as_deref(),
            ) {
                Ok(Recorded::Proposed) => proposed += 1,
                // Already a deadline, already waiting, or declined earlier.
                // A re-sync of an unchanged course is entirely these, and
                // reporting them as new would make "nothing happened" read
                // like work.
                Ok(_) => known += 1,
                Err(e) => eprintln!("canvas: skipping assignment '{title}': {e:#}"),
            }
        }
        Ok((proposed, undated, known, completed, moved))
    })?;

    outcome.deadlines_proposed = proposed;
    outcome.deadlines_completed = completed;
    if undated > 0 {
        outcome.notes.push(format!(
            "{undated} Canvas assignment(s) have no due date set — not proposed"
        ));
    }
    if known > 0 {
        outcome
            .notes
            .push(format!("{known} Canvas assignment(s) already accounted for"));
    }
    if moved > 0 {
        outcome.notes.push(format!(
            "{moved} deadline(s) moved to the due date Canvas states"
        ));
    }
    if proposed > 0 {
        emit_hub_change(app, "deadlineProposals");
    }
    if completed > 0 || moved > 0 {
        emit_hub_change(app, "deadlines");
    }
    Ok(())
}

/// `100` rather than `100.0`, since points are almost always whole.
fn trim_number(value: f64) -> String {
    if (value.fract()).abs() < f64::EPSILON {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

// ---------------------------------------------------------------------------
// Assignment groups → grade categories, graded submissions → grade items
// (SPEC §11)

/// Whether the course applies its assignment-group weights — the only case in
/// which Canvas knows what a category is worth. Otherwise the syllabus does,
/// and the weight stays the reader's to type.
fn applies_group_weights(course: &Value) -> bool {
    course["apply_assignment_group_weights"].as_bool().unwrap_or(false)
}

/// A graded, posted submission, read off an assignment fetched with
/// `include[]=submission`.
#[derive(Debug, PartialEq)]
struct Graded {
    assignment_id: String,
    group_id: String,
    name: String,
    score: f64,
    max_score: f64,
    /// The local calendar day the grade was given, when Canvas says.
    graded_at: Option<String>,
}

/// Where a submission stands, for the grade pass and its report.
#[derive(Debug, PartialEq)]
enum GradeState {
    /// Graded, posted, not excused, with points to score against: a grade.
    Posted(Graded),
    /// Graded, but the professor has not released it. Reported and never
    /// recorded — silence here would read as "nothing to do", when the reader
    /// is waiting on exactly this.
    Unposted,
    /// Nothing to record: ungraded, excused, no points possible, or read
    /// without the submission.
    Nothing,
}

/// The predicate that turns a submission into a grade, or refuses to.
///
/// Three things have to be true: Canvas holds a score, the professor has
/// posted it — a muted grade is one the reader is not meant to see yet, and
/// recording it early is the wrong kind of early — and the submission is not
/// excused. Points possible has to be positive as well: Canvas reports 0 or
/// null for an ungraded assignment, and the CHECK on `max_score` would refuse
/// it a step later.
fn grade_state(assignment: &Value) -> GradeState {
    let graded = || -> Option<Graded> {
        let submission = assignment.get("submission")?;
        let score = submission["score"].as_f64()?;
        let max_score = assignment["points_possible"].as_f64().filter(|p| *p > 0.0)?;
        Some(Graded {
            assignment_id: assignment["id"].as_i64()?.to_string(),
            group_id: assignment["assignment_group_id"].as_i64()?.to_string(),
            name: assignment["name"]
                .as_str()
                .map(str::trim)
                .filter(|n| !n.is_empty())?
                .to_string(),
            score,
            max_score,
            graded_at: iso_date(submission["graded_at"].as_str()),
        })
    };
    let submission = &assignment["submission"];
    if submission["excused"].as_bool().unwrap_or(false) {
        return GradeState::Nothing;
    }
    let Some(graded) = graded() else {
        return GradeState::Nothing;
    };
    if submission["posted_at"].as_str().filter(|p| !p.is_empty()).is_none() {
        return GradeState::Unposted;
    }
    GradeState::Posted(graded)
}

/// `grade_state` for a test that wants the grade or nothing.
#[cfg(test)]
fn graded_and_posted(assignment: &Value) -> Option<Graded> {
    match grade_state(assignment) {
        GradeState::Posted(graded) => Some(graded),
        GradeState::Unposted | GradeState::Nothing => None,
    }
}

/// Categories from the course's assignment groups, items from every graded and
/// posted submission among `assignments`, each keyed on its Canvas id.
///
/// Written directly rather than proposed: a grade is reversible through the
/// Grades section, which is the app's rule for skipping a confirm step, and
/// every write leaves an audit row. A second sync of an unchanged course
/// writes nothing and leaves no row.
fn sync_grades(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course: &Value,
    assignments: &[Value],
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let course_id = course["id"].as_i64().context("the Canvas course has no id")?;
    let groups = session.get_all(
        &format!("/api/v1/courses/{course_id}/assignment_groups"),
        on_stage,
    )?;
    let weighted = applies_group_weights(course);

    let (categories_landed, category_notes, recorded, orphans, unposted, changed) = with_conn(app, |conn| {
        let mut by_group: HashMap<String, i64> = HashMap::new();
        let mut categories_landed = 0usize;
        let mut category_notes: Vec<String> = Vec::new();
        let mut changed = false;
        for group in &groups {
            let Some(id) = group["id"].as_i64().map(|id| id.to_string()) else {
                continue;
            };
            let Some(name) = group["name"].as_str().map(str::trim).filter(|n| !n.is_empty())
            else {
                continue;
            };
            let weight = if weighted { group["group_weight"].as_f64() } else { None };
            let group = CanvasGroup { id: &id, name, weight };
            match crate::grades::upsert_canvas_category(conn, class.id, &group) {
                Ok(category) => {
                    match category.write {
                        CanvasWrite::Created | CanvasWrite::Claimed => {
                            categories_landed += 1;
                            changed = true;
                        }
                        CanvasWrite::Updated => changed = true,
                        CanvasWrite::Unchanged => {}
                    }
                    category_notes.extend(category.note);
                    by_group.insert(id, category.id);
                }
                Err(e) => eprintln!("canvas: skipping assignment group '{name}': {e:#}"),
            }
        }

        let mut recorded = 0usize;
        let mut orphans = 0usize;
        let mut unposted = 0usize;
        for assignment in assignments {
            let graded = match grade_state(assignment) {
                GradeState::Posted(graded) => graded,
                GradeState::Unposted => {
                    unposted += 1;
                    continue;
                }
                GradeState::Nothing => continue,
            };
            // A group the listing did not carry — deleted between the two
            // reads, or refused. Counted rather than filed under a guess.
            let Some(&category_id) = by_group.get(&graded.group_id) else {
                orphans += 1;
                continue;
            };
            let score = CanvasScore {
                assignment_id: &graded.assignment_id,
                name: &graded.name,
                score: graded.score,
                max_score: graded.max_score,
                graded_at: graded.graded_at.as_deref(),
            };
            match crate::grades::upsert_canvas_item(conn, class.id, category_id, &score) {
                Ok(CanvasWrite::Unchanged) => {}
                Ok(_) => {
                    recorded += 1;
                    changed = true;
                }
                Err(e) => eprintln!("canvas: skipping grade '{}': {e:#}", graded.name),
            }
        }
        Ok((categories_landed, category_notes, recorded, orphans, unposted, changed))
    })?;

    outcome.grades_recorded = recorded;
    outcome.notes.extend(category_notes);
    if unposted > 0 {
        outcome.notes.push(format!(
            "{unposted} graded assignment(s) not posted yet — Canvas is holding the grade"
        ));
    }
    if categories_landed > 0 {
        outcome.notes.push(format!(
            "{categories_landed} grade categor{} from Canvas{}",
            if categories_landed == 1 { "y" } else { "ies" },
            if weighted {
                ", weighted as the course weights them"
            } else {
                " — the course does not weight them, so the weights are yours to set"
            }
        ));
    }
    if orphans > 0 {
        outcome.notes.push(format!(
            "{orphans} graded assignment(s) belong to no assignment group Canvas listed — not recorded"
        ));
    }
    if changed {
        emit_hub_change(app, "grades");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Course files → the inbox and the move confirm queue (SPEC §10)

fn sync_files(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course_id: i64,
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let files = session.get_all(&format!("/api/v1/courses/{course_id}/files"), on_stage)?;
    if files.is_empty() {
        return Ok(());
    }
    // Canvas's folder paths are the only placement signal it offers, since
    // none of these courses uses modules. One request buys every file's home.
    let folders = match session.get_all(&format!("/api/v1/courses/{course_id}/folders"), on_stage) {
        Ok(folders) => folders,
        // Said rather than absorbed. With no folder list every file reports as
        // "Canvas gave no folder for it", which is a different claim from
        // "Canvas was never successfully asked" and calls for something else.
        Err(e) => {
            outcome.notes.push(format!(
                "Canvas's folder list could not be read ({e:#}) — files land in the inbox unplaced"
            ));
            Vec::new()
        }
    };

    let (class_dir, vocabulary) = with_conn(app, |conn| {
        Ok((
            crate::scanner::class_dir(conn, class.id)?,
            crate::scanner::folder_vocabulary(conn)?,
        ))
    })?;
    let inbox = class_dir.join(INBOX_DIR);
    std::fs::create_dir_all(&inbox).with_context(|| format!("creating {}", inbox.display()))?;

    // Everything the class already holds, by (name, size). Canvas has no
    // stable identity the tree preserves — a downloaded file keeps only its
    // name — so this is what stops a re-sync duplicating, and it is also what
    // stops it re-downloading material that arrived any other way.
    let mut seen = with_conn(app, |conn| indexed_files(conn, class.id))?;
    for entry in std::fs::read_dir(&inbox).into_iter().flatten().flatten() {
        if let Ok(meta) = entry.metadata() {
            seen.insert((entry.file_name().to_string_lossy().into_owned(), meta.len() as i64));
        }
    }

    let mut staged = 0usize;
    let mut skipped = 0usize;
    let mut loose = 0usize;
    let mut unproposed = 0usize;
    for file in &files {
        let Some(name) = canvas_file_name(file) else {
            continue;
        };
        // Without a size neither the ceiling nor the duplicate check can do its
        // job, so the file is named and passed over rather than let through on
        // a sentinel that reads as "small" to one and "new" to the other.
        let Some(size) = file["size"].as_i64().filter(|s| *s >= 0) else {
            outcome
                .notes
                .push(format!("{name} has no size in Canvas — not downloaded"));
            continue;
        };
        // Also catches Canvas's own duplicates: two of these courses file the
        // same PDF under two folders, and both come back in one listing.
        if !seen.insert((name.clone(), size)) {
            skipped += 1;
            continue;
        }
        let Some(url) = file["url"].as_str().filter(|u| !u.is_empty()) else {
            outcome.notes.push(format!("{name} has no download link in Canvas"));
            continue;
        };
        // Checked before fetching, not after: the ceiling exists because the
        // bytes travel through the page in memory, and discovering the size by
        // loading it would defeat the point. The page enforces it again on what
        // actually arrives, since this number is Canvas's claim.
        if size > crate::canvas::MAX_DOWNLOAD_BYTES {
            outcome.notes.push(format!(
                "{name} is {} — too large to pull through Canvas; download it there",
                crate::tools::format_size(size)
            ));
            continue;
        }

        on_stage(&format!("Downloading {name}…"));
        // Never onto a name the inbox already holds. The (name, size) check
        // above lets a same-named file of a different size through, and what it
        // would land on may be the only copy of something dropped by hand and
        // still waiting for approval.
        let dest = crate::sorter::free_slot(&inbox, &name);
        let landed = dest
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if let Err(e) = download_once_retried(session, url, &dest, Some(size), on_stage) {
            outcome.notes.push(format!("{name} did not download: {e:#}"));
            // Released, so a second copy of the same file elsewhere in Canvas
            // still gets a turn — two of these courses keep the same PDF in
            // two folders, and the copy that failed must not stand in for the
            // one that might not.
            seen.remove(&(name.clone(), size));
            continue;
        }
        staged += 1;
        seen.insert((landed.clone(), size));
        let source_rel = format!("{INBOX_DIR}/{landed}");

        match record_landed(
            app,
            class.id,
            &class_dir,
            file,
            &folders,
            &vocabulary,
            &source_rel,
            &landed,
        ) {
            Err(e) => {
                outcome.notes.push(format!("{landed}: {e:#}"));
                unproposed += 1;
            }
            Ok(false) => loose += 1,
            Ok(true) => {}
        }
    }

    outcome.files_staged = staged;
    if skipped > 0 {
        outcome.notes.push(format!("{skipped} file(s) already in the class"));
    }
    if loose > 0 {
        outcome.notes.push(format!(
            "{loose} file(s) waiting in the inbox — Canvas keeps them loose, so the sorter reads \
             them by content"
        ));
    }
    // A different situation from the one above, with a different remedy: these
    // downloaded but could not be proposed.
    if unproposed > 0 {
        outcome.notes.push(format!(
            "{unproposed} file(s) downloaded but could not be proposed — see the lines above"
        ));
    }
    if staged > 0 {
        emit_hub_change(app, "proposals");
        // The loose ones have no placement to inherit, so they wait for the
        // content-aware sorter — which SPEC §7.2 promises and which nothing was
        // actually asking for.
        if loose > 0 {
            crate::sorter::enqueue_followup(app, class.id);
        }
    }
    Ok(())
}

/// Logs a downloaded file and proposes where it goes, returning whether Canvas
/// had a destination for it.
///
/// Where Canvas filed something is a proposal, never a placement — approval is
/// what moves a file (SPEC §10). Canvas keeping it loose in the course root is
/// no signal at all, so those stay in the inbox for the content-aware sorter to
/// read rather than being given an invented home.
#[allow(clippy::too_many_arguments)]
fn record_landed(
    app: &AppHandle,
    class_id: i64,
    class_dir: &std::path::Path,
    file: &Value,
    folders: &[Value],
    vocabulary: &[(String, usize)],
    source_rel: &str,
    landed: &str,
) -> Result<bool> {
    let folder = canvas_folder_path(file, folders, vocabulary);
    let dest_rel = folder.as_ref().map(|folder| format!("{folder}/{landed}"));
    with_conn(app, |conn| {
        // Logged where the bytes land, not where they are proposed to go: the
        // question this answers is "what did the sync put on my disk", and a
        // loose file is on disk just the same.
        audit(
            conn,
            "canvas.staged_file",
            json!({ "classId": class_id, "source": source_rel, "dest": dest_rel }),
        )?;
        let (Some(folder), Some(dest_rel)) = (&folder, &dest_rel) else {
            return Ok(false);
        };
        // Says both names when they differ, so a retargeted destination is
        // legible rather than looking like a misread of Canvas.
        let original = raw_canvas_folder(file, folders);
        let reasoning = match original {
            Some(ref original) if !original.eq_ignore_ascii_case(folder) => format!(
                "Canvas files it under \"{original}\"; this library calls that \"{folder}\""
            ),
            _ => format!("Canvas files it under \"{folder}\""),
        };
        propose_move(conn, class_id, class_dir, source_rel, dest_rel, &reasoning)?;
        Ok(true)
    })
}

/// A Canvas file's name, as a single path segment safe to write.
///
/// A name that sanitizes away entirely would resolve to the inbox folder
/// itself, and the download would write over a directory path.
fn canvas_file_name(file: &Value) -> Option<String> {
    let raw = file["display_name"]
        .as_str()
        .or_else(|| file["filename"].as_str())
        .map(str::trim)
        .filter(|n| !n.is_empty())?;
    sanitize_name(raw).filter(|n| !n.is_empty())
}

/// One retry, for the one failure mode that is worth retrying.
///
/// An in-page fetch occasionally comes back with WebKit's bare "Load failed" —
/// no status, no body — which is what it reports for a request that never
/// completed: a cancelled connection, or the page navigating underneath it.
/// That is transient by nature, and one more attempt settles it. A refusal
/// (Canvas answering 401/403/404) is not retried: the answer would be the same,
/// and a retry loop is the one thing that could make a sync approach the rate
/// limit.
fn download_once_retried(
    session: &Session,
    url: &str,
    dest: &std::path::Path,
    expected: Option<i64>,
    on_stage: &dyn Fn(&str),
) -> Result<u64> {
    match session.download(url, dest, expected, on_stage) {
        Ok(bytes) => Ok(bytes),
        Err(first) if is_transient(&first) => {
            std::thread::sleep(std::time::Duration::from_secs(2));
            session.download(url, dest, expected, on_stage).map_err(|second| {
                // Both attempts, because "it failed twice the same way" and
                // "it failed two different ways" call for different things.
                anyhow::anyhow!("{first:#}; on retry: {second:#}")
            })
        }
        Err(e) => Err(e),
    }
}

/// Whether the failure was the page's rather than Canvas's answer.
///
/// A fetch that never completed reaches `catch` and comes out as a
/// `PageFailure`; a refusal resolves with a status and is not one. So the retry
/// decision is a type test rather than a reading of the error's prose — which
/// by the time it arrived here also carried the file URL and up to 200
/// characters of Canvas's own response body, and would have gone quiet the day
/// WebKit reworded "Load failed".
fn is_transient(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<crate::canvas::PageFailure>())
}

/// (name, size) for everything the scanner has indexed for this class.
fn indexed_files(conn: &Connection, class_id: i64) -> Result<HashSet<(String, i64)>> {
    let mut stmt = conn.prepare("SELECT rel_path, size FROM files WHERE class_id = ?1")?;
    let rows = stmt.query_map([class_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut out = HashSet::new();
    for row in rows {
        let (rel_path, size) = row?;
        let name = rel_path.rsplit('/').next().unwrap_or(&rel_path).to_string();
        out.insert((name, size));
    }
    Ok(out)
}

/// The Canvas folder a file sits in, as a class-relative folder path.
///
/// Canvas roots every course's files at "course files", which carries no
/// meaning here and is stripped. What is left is the professor's own
/// organization ("Lecture Slides", "Coding Material/Week 2 Coding Material"),
/// which is a far better first guess than anything derivable from a filename.
/// The root itself, and Canvas's "unfiled" bucket, mean *no* organization —
/// those return None rather than a folder to invent.
///
/// Each segment is then mapped onto the vocabulary the tree already uses, so a
/// sync does not undo the consistency the sorter is asked to keep: a course
/// calling its deck folder "Lecture Slides" should not earn this class a second
/// folder beside the "Slides" every other class has.
fn canvas_folder_path(
    file: &Value,
    folders: &[Value],
    vocabulary: &[(String, usize)],
) -> Option<String> {
    let trimmed = folder_full_name(file, folders)?;
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unfiled") {
        return None;
    }
    // Canvas allows characters a folder name here should not carry, and a
    // hidden segment would make the moved file invisible to the scanner. A
    // segment that sanitizes to nothing drops the whole path rather than
    // silently reparenting the file one level up.
    let cleaned: Option<Vec<String>> = trimmed.split('/').map(|s| sanitize_name(s.trim())).collect();
    let mut path: Vec<String> = Vec::new();
    for segment in cleaned? {
        let mapped = canonical_segment(&segment, vocabulary);
        // Mapping a child onto a name already above it would nest a folder
        // inside itself — "Coding Material/Coding Material" — which is not
        // where the professor filed anything. Canvas's own name stands there.
        if path.iter().any(|ancestor| ancestor.eq_ignore_ascii_case(&mapped)) {
            path.push(segment);
        } else {
            path.push(mapped);
        }
    }
    Some(path.join("/"))
}

/// Canvas's own name for the folder, before the vocabulary is applied — only
/// used to say so in the reasoning when the two differ.
fn raw_canvas_folder(file: &Value, folders: &[Value]) -> Option<String> {
    let trimmed = folder_full_name(file, folders)?;
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// The professor's own path for a file's folder, with Canvas's "course files"
/// root stripped.
///
/// One reader, so the two callers cannot come to disagree about what the root
/// is — the copy that lacked the `unfiled` check was reachable only because of
/// the order the caller happened to use them in.
fn folder_full_name<'a>(file: &Value, folders: &'a [Value]) -> Option<&'a str> {
    let folder_id = file["folder_id"].as_i64()?;
    let full_name = folders
        .iter()
        .find(|f| f["id"].as_i64() == Some(folder_id))?["full_name"]
        .as_str()?;
    Some(
        full_name
            .trim()
            .trim_start_matches("course files")
            .trim_matches('/'),
    )
}

/// A Canvas folder name in the tree's own words, where the tree has a word.
///
/// Two matches count, and nothing else: the same name in different casing, and
/// a vocabulary name the Canvas one ends with on a word boundary ("Lecture
/// Slides" → "Slides"). Anything looser starts renaming folders on a
/// resemblance, which is worse than a second folder.
///
/// The exact rule is exhausted before the suffix rule is tried at all. Testing
/// both per entry meant the first *entry* won rather than the better *match*,
/// so a suffix hit on a more widely shared name beat an exact hit on a less
/// shared one — "Reading Material" became "Material" whenever "Material"
/// happened to rank higher. Within each rule the vocabulary is sorted
/// most-shared first, which is the order convergence should follow.
fn canonical_segment(segment: &str, vocabulary: &[(String, usize)]) -> String {
    for (name, _) in vocabulary {
        if name.eq_ignore_ascii_case(segment) {
            return name.clone();
        }
    }
    let lower_segment = segment.to_lowercase();
    for (name, _) in vocabulary {
        let Some(prefix) = lower_segment.strip_suffix(&name.to_lowercase()) else {
            continue;
        };
        if !prefix.ends_with(' ') {
            continue;
        }
        // A qualifier carrying a number is the thing that tells one division's
        // folder from another's: "Week 2 Slides" and "Week 3 Slides" are two
        // folders, and dropping the prefix would merge them into one and file
        // three weeks of material together.
        if prefix.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }
        return name.clone();
    }
    segment.to_string()
}

/// A path segment safe to write into the class tree.
///
/// Canvas names are the professor's, so they carry whatever they carry. Two
/// properties have to hold on the way in: no separator (or the name would
/// silently become a path), and no leading dot (the scanner hides dot-entries
/// at every depth, so the file would land and then be invisible). Between them
/// those also defuse a traversal attempt — `..` cannot survive a rule that
/// strips leading dots and keeps the result to one segment.
///
/// `None` when nothing usable is left, which the caller skips rather than
/// resolving to the containing folder.
fn sanitize_name(name: &str) -> Option<String> {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | '\0') { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Records a pending move for a downloaded file, through the same validation
/// and the same table the sort job and chat both use.
fn propose_move(
    conn: &Connection,
    class_id: i64,
    class_dir: &std::path::Path,
    source_rel: &str,
    dest_rel: &str,
    reasoning: &str,
) -> Result<()> {
    crate::sorter::validate_dest(class_dir, source_rel, dest_rel)?;
    // No confidence: the queue's confidence ladder rates how sure a model is of
    // a guess, and this is not one. The card says VIA CANVAS instead, and
    // storing `high` here would make a later read of the table look as though
    // something had rated it.
    crate::sorter::upsert_proposal(conn, class_id, "canvas", source_rel, dest_rel, reasoning, None)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Announcements → `announcements` (SPEC §5): what the professor said

/// One announcement as the sync records it, the body already stripped to text.
pub(crate) struct CanvasAnnouncement<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    /// Local wall-clock ISO, the shape `deadlines.due_at` uses.
    pub posted_at: &'a str,
}

fn sync_announcements(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course_id: i64,
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    // `/announcements?context_codes[]=course_N` answers only a fortnight back
    // unless told otherwise, and the semester is longer than that; the
    // course's discussion topics filtered to announcements are the same
    // objects with no window on them.
    let path = format!("/api/v1/courses/{course_id}/discussion_topics?only_announcements=true");
    let topics = session.get_all(&path, on_stage)?;
    // The count lands on the outcome as each row does, so a failure partway
    // reports what reached the table rather than zero.
    let (recorded, refused) = with_conn(app, |conn| {
        let mut recorded = 0usize;
        // Announcements the table would not take, with the first reason: a
        // row another class holds for the same Canvas id, or a write that
        // failed. Said in the report, since NOTICES staying empty says nothing.
        let mut refused: Vec<String> = Vec::new();
        for topic in &topics {
            let Some(id) = topic["id"].as_i64().map(|id| id.to_string()) else {
                continue;
            };
            let Some(title) = topic["title"].as_str().map(str::trim).filter(|t| !t.is_empty())
            else {
                continue;
            };
            // A delayed announcement has no `posted_at` yet, and a student
            // does not see it either.
            let Some(posted_at) = topic["posted_at"].as_str().and_then(local_iso) else {
                continue;
            };
            let body = strip_html(topic["message"].as_str().unwrap_or_default());
            let announcement = CanvasAnnouncement {
                id: &id,
                title,
                body: body.trim(),
                posted_at: &posted_at,
            };
            match record_announcement(conn, class.id, &announcement) {
                Ok(true) => {
                    recorded += 1;
                    outcome.announcements_recorded = recorded;
                }
                Ok(false) => {}
                Err(e) => {
                    eprintln!("canvas: skipping announcement '{title}': {e:#}");
                    refused.push(format!("{e:#}"));
                }
            }
        }
        Ok((recorded, refused))
    })?;
    if let Some(first) = refused.first() {
        outcome.notes.push(format!(
            "{} could not be recorded — {first}",
            plural(refused.len(), "announcement")
        ));
    }
    if recorded > 0 {
        emit_hub_change(app, "announcements");
    }
    Ok(())
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Inserts an announcement, or updates the row its Canvas id already names
/// when the title, body or posting time moved — an edited announcement is the
/// same notice. `true` when a row was written; a re-sync of an unchanged
/// course writes nothing. A row another class holds is refused rather than
/// moved: the id is global, and two classes matched to one course would
/// otherwise pass the row back and forth each sync.
pub(crate) fn record_announcement(
    conn: &Connection,
    class_id: i64,
    announcement: &CanvasAnnouncement<'_>,
) -> Result<bool> {
    let existing: Option<(i64, i64, String, String, String)> = conn
        .query_row(
            "SELECT id, class_id, title, body, posted_at FROM announcements WHERE canvas_id = ?1",
            [announcement.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    match existing {
        Some((_, other, ..)) if other != class_id => {
            bail!("announcement {} is recorded under another class", announcement.id)
        }
        Some((_, _, title, body, posted_at))
            if title == announcement.title
                && body == announcement.body
                && posted_at == announcement.posted_at =>
        {
            Ok(false)
        }
        Some((id, ..)) => {
            conn.execute(
                "UPDATE announcements SET title = ?1, body = ?2, posted_at = ?3 WHERE id = ?4",
                params![announcement.title, announcement.body, announcement.posted_at, id],
            )?;
            Ok(true)
        }
        None => {
            conn.execute(
                "INSERT INTO announcements (class_id, canvas_id, title, body, posted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    class_id,
                    announcement.id,
                    announcement.title,
                    announcement.body,
                    announcement.posted_at
                ],
            )?;
            Ok(true)
        }
    }
}

/// One announcement for the workspace's NOTICES section and the chat overview.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncementInfo {
    pub id: i64,
    pub canvas_id: String,
    pub title: String,
    pub body: String,
    pub posted_at: String,
}

/// The class's announcements, newest first.
pub fn list_announcements(conn: &Connection, class_id: i64) -> Result<Vec<AnnouncementInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, canvas_id, title, body, posted_at FROM announcements
         WHERE class_id = ?1 ORDER BY posted_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(AnnouncementInfo {
                id: row.get(0)?,
                canvas_id: row.get(1)?,
                title: row.get(2)?,
                body: row.get(3)?,
                posted_at: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Pages and the syllabus page → the extract cache (SPEC §7.2)

/// The folder inside the extract cache that holds the course's Canvas texts.
/// Inside `.classhub/extracts/` so `search_material` covers them without a
/// new root; a folder of their own so they never share a name with a source
/// file's extract.
const CANVAS_TEXTS_DIR: &str = "Canvas";
/// The syllabus page's file stem, reserved so a Page titled "Syllabus" lands
/// beside it rather than on it.
const SYLLABUS_STEM: &str = "Syllabus";

fn canvas_texts_dir() -> String {
    format!("{EXTRACTS_DIR}/{CANVAS_TEXTS_DIR}")
}

/// The mirrored syllabus page's class-relative path.
pub fn canvas_syllabus_rel() -> String {
    format!("{}/{SYLLABUS_STEM}.md", canvas_texts_dir())
}

/// The mirrored syllabus page, when a sync has written one — what the
/// syllabus scan's picker offers (SPEC §11).
pub fn canvas_syllabus_path(conn: &Connection, class_id: i64) -> Result<Option<String>> {
    let rel = canvas_syllabus_rel();
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    Ok(class_dir.join(&rel).is_file().then_some(rel))
}

fn sync_pages(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course: &Value,
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let course_id = course["id"].as_i64().context("the Canvas course has no id")?;
    // `include[]=body` puts each page's HTML on the listing, so a course's
    // Pages cost one request per hundred rather than one per page (the rate
    // limit is 700 / 10 min, and a per-page loop is the one shape that could
    // reach it).
    let pages = session.get_all(
        &format!("/api/v1/courses/{course_id}/pages?include[]=body"),
        on_stage,
    )?;
    let class_dir = with_conn(app, |conn| crate::scanner::class_dir(conn, class.id))?;
    let dir = class_dir.join(canvas_texts_dir());

    let mut written = 0usize;
    // Titles that appear more than once in the listing, so every page sharing
    // one gets its slug in its file name and the name is the page's rather
    // than the listing order's — a re-sync must find the same files.
    let duplicated = duplicated_titles(&pages);
    let mut taken: HashSet<String> = HashSet::new();
    taken.insert(SYLLABUS_STEM.to_lowercase());
    // What this sync wrote or confirmed, so the folder can be reconciled to
    // it afterwards. Separate from `taken`, which reserves the syllabus stem
    // whether or not a syllabus page exists.
    let mut kept: HashSet<String> = HashSet::new();
    for page in &pages {
        // The API answers with what the reader may see, but both flags are
        // cheap to honour and a hidden page is not course content.
        if !page["published"].as_bool().unwrap_or(true)
            || page["hide_from_students"].as_bool().unwrap_or(false)
        {
            continue;
        }
        let Some(title) = page["title"].as_str().map(str::trim).filter(|t| !t.is_empty()) else {
            continue;
        };
        let text = strip_html(page["body"].as_str().unwrap_or_default());
        if text.trim().is_empty() {
            continue;
        }
        let Some(stem) = page_file_stem(
            title,
            page["url"].as_str().unwrap_or_default(),
            &duplicated,
            &mut taken,
        ) else {
            outcome.notes.push(format!("the Canvas page \"{title}\" could not be given a file name"));
            continue;
        };
        let content = page_markdown(
            title,
            "Canvas page",
            page["updated_at"].as_str().and_then(local_iso).as_deref(),
            page["html_url"].as_str(),
            &text,
        );
        // One page that will not write is a line, not the end of the pass —
        // and the page is still on Canvas, so whatever copy is on disk is
        // kept rather than pruned as stale.
        match write_if_changed(&dir.join(format!("{stem}.md")), &content) {
            Ok(true) => {
                written += 1;
                outcome.pages_written = written;
            }
            Ok(false) => {}
            Err(e) => outcome
                .notes
                .push(format!("the Canvas page \"{title}\" could not be written — {e:#}")),
        }
        kept.insert(stem.to_lowercase());
    }

    // The syllabus page, from the course object the listing carried it on.
    let syllabus = strip_html(course["syllabus_body"].as_str().unwrap_or_default());
    if !syllabus.trim().is_empty() {
        let url = format!(
            "https://{}/courses/{course_id}/assignments/syllabus",
            crate::canvas::CANVAS_HOST
        );
        let content =
            page_markdown(SYLLABUS_STEM, "Canvas syllabus page", None, Some(&url), &syllabus);
        match write_if_changed(&dir.join(format!("{SYLLABUS_STEM}.md")), &content) {
            Ok(true) => {
                written += 1;
                outcome.pages_written = written;
                outcome
                    .notes
                    .push("the Canvas syllabus page was mirrored — SCAN SYLLABUS can read it".into());
            }
            Ok(false) => {}
            Err(e) => outcome
                .notes
                .push(format!("the Canvas syllabus page could not be written — {e:#}")),
        }
        kept.insert(SYLLABUS_STEM.to_lowercase());
    }

    // The one thing a sync removes is a file it wrote itself. A page retitled
    // or unpublished on Canvas would otherwise leave its old text beside the
    // new, and chat reads both as the course's own words.
    let removed = prune_stale_texts(&dir, &kept)?;
    if removed > 0 {
        outcome.notes.push(format!(
            "{} no longer on Canvas — removed from the extract cache",
            plural_pages(removed)
        ));
    }
    Ok(())
}

/// Removes the `.md` files in the Canvas texts folder whose stem this sync did
/// not write or confirm, and returns how many. The folder is the sync's own —
/// nothing else writes there, and no `files` row points into it — which is
/// what makes deleting here compatible with never deleting source material.
fn prune_stale_texts(dir: &Path, kept: &HashSet<String>) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut removed = 0usize;
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if kept.contains(&stem.to_lowercase()) {
            continue;
        }
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn plural_pages(count: usize) -> String {
    plural(count, "Canvas page")
}

/// The sanitized titles that more than one page in the listing shares,
/// lowercased the way file names are compared.
fn duplicated_titles(pages: &[Value]) -> HashSet<String> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut duplicated: HashSet<String> = HashSet::new();
    for page in pages {
        let Some(name) = page["title"].as_str().and_then(sanitize_name) else {
            continue;
        };
        let key = name.to_lowercase();
        if !seen.insert(key.clone()) {
            duplicated.insert(key);
        }
    }
    duplicated
}

/// How much of a Page's title goes into its file name. Canvas allows 255
/// characters, and with the slug and `.md` behind it that is past what the
/// file system takes; a stem this long still reads as the page.
const MAX_STEM_CHARS: usize = 120;

/// A file stem for a Page, unique within the course and a function of the
/// page alone: its title as a path segment, capped, with Canvas's own URL
/// slug appended — sanitized the same way, since it becomes part of a file
/// name — when another page shares the title or the title is the syllabus
/// page's. Deciding by the listing rather than by arrival order is what keeps
/// two pages named `Home` on the same two files from one sync to the next.
/// Names are compared case-insensitively, since the disk is.
fn page_file_stem(
    title: &str,
    url: &str,
    duplicated: &HashSet<String>,
    taken: &mut HashSet<String>,
) -> Option<String> {
    let base = crate::db::truncate(&sanitize_name(title)?, MAX_STEM_CHARS);
    let key = base.to_lowercase();
    let stem = if duplicated.contains(&key) || key == SYLLABUS_STEM.to_lowercase() {
        format!("{base} ({})", sanitize_name(url).unwrap_or_default())
    } else {
        base
    };
    taken.insert(stem.to_lowercase()).then_some(stem)
}

/// The markdown a Canvas text is mirrored as: its title, one line saying
/// what it is and where it lives on Canvas, then the text.
fn page_markdown(
    title: &str,
    kind: &str,
    updated_at: Option<&str>,
    url: Option<&str>,
    text: &str,
) -> String {
    let mut line = kind.to_string();
    if let Some(updated) = updated_at {
        line.push_str(&format!(", last edited {}", &updated[..updated.len().min(10)]));
    }
    if let Some(url) = url {
        line.push_str(&format!(" · {url}"));
    }
    // The title is the professor's text; on one line it stays a heading and
    // cannot open a section of its own in a file a job reads.
    let title = title.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("# {title}\n\n{line}\n\n{}\n", text.trim())
}

/// Writes `content` unless the file already holds exactly that, so a re-sync
/// of an unchanged course touches nothing on disk.
fn write_if_changed(path: &Path, content: &str) -> Result<bool> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_atomic(path, content)?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// What the UI reads between syncs

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasStatus {
    /// The most recent per-class sync, or None if nothing has ever synced.
    /// There is no "connected" state to report: holding a session cookie says
    /// nothing about whether Canvas still honours it, so the only honest thing
    /// to say is when data last came across.
    pub last_synced_at: Option<i64>,
    pub classes_linked: i64,
    pub host: String,
}

pub fn status(conn: &Connection) -> Result<CanvasStatus> {
    let last_synced_at: Option<i64> = conn
        .query_row("SELECT MAX(canvas_synced_at) FROM classes", [], |row| row.get(0))
        .optional()?
        .flatten();
    let classes_linked: i64 = conn.query_row(
        "SELECT COUNT(*) FROM classes WHERE canvas_course_id IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    Ok(CanvasStatus {
        last_synced_at,
        classes_linked,
        host: crate::canvas::CANVAS_HOST.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(code: &str) -> ClassRow {
        ClassRow {
            id: 1,
            display_name: "Biostatistics for AI".into(),
            code: code.into(),
        }
    }

    fn courses() -> Vec<Value> {
        vec![
            json!({"id": 576057, "name": "CAI5731 - Biostatistics for AI", "course_code": "CAI5731"}),
            json!({"id": 576174, "name": "CAI5720 - Fund AI in Medicine I", "course_code": "CAI5720"}),
            json!({"id": 369819, "name": "Civic Literacy 2019 ", "course_code": "Civic Literacy 2019"}),
        ]
    }

    /// The account is enrolled in a dozen "active" courses, most of them
    /// years-old org shells. Matching has to pick one and refuse the rest.
    #[test]
    fn matches_a_class_to_its_canvas_course_by_code() {
        let all = courses();
        let matched = match_course(&class("CAI5731"), &all).expect("matched");
        assert_eq!(matched["id"], 576057);
        // Case and stray whitespace are Canvas's, not a reason to miss.
        assert_eq!(match_course(&class(" cai5720 "), &all).expect("matched")["id"], 576174);
    }

    #[test]
    fn refuses_to_guess_when_there_is_no_exact_match() {
        let all = courses();
        // A course that is simply not enrolled this term.
        assert!(match_course(&class("CAI9999"), &all).is_err());
        assert!(match_course(&class(""), &all).is_err());
        // Two courses sharing a code is not a coin flip.
        let mut duplicated = all.clone();
        duplicated.push(json!({"id": 999, "name": "Other", "course_code": "CAI5731"}));
        let err = match_course(&class("CAI5731"), &duplicated).unwrap_err().to_string();
        assert!(err.contains("not guessing"), "{err}");
    }

    /// The conversion this milestone would be silently wrong without. Both
    /// real assignments are 23:59 local deadlines expressed in UTC, and they
    /// straddle the DST change — so a fixed offset gets one of them a day off.
    #[test]
    fn converts_canvas_timestamps_to_local_wall_clock() {
        // Only meaningful in the zone the machine actually runs in.
        let Some(august) = local_iso("2026-08-26T03:59:59Z") else {
            panic!("failed to parse a plain RFC 3339 timestamp");
        };
        let december = local_iso("2026-12-03T04:59:59Z").expect("parses");
        assert_eq!(august.len(), 16, "{august}");
        assert!(crate::deadlines::valid_due_at(&august), "{august}");
        assert!(crate::deadlines::valid_due_at(&december), "{december}");
        if std::env::var("TZ").as_deref() == Ok("America/New_York") {
            assert_eq!(august, "2026-08-25T23:59");
            assert_eq!(december, "2026-12-02T23:59");
        }
        assert_eq!(iso_date(Some("2026-08-26T03:59:59Z")).map(|d| d.len()), Some(10));
        assert_eq!(local_iso("not a timestamp"), None);
        assert_eq!(iso_date(None), None);
    }

    #[test]
    fn reads_the_professor_s_folders_and_ignores_canvas_s_own_root() {
        let folders = vec![
            json!({"id": 1, "full_name": "course files"}),
            json!({"id": 2, "full_name": "course files/Lecture Slides"}),
            json!({"id": 3, "full_name": "course files/Coding Material/Week 2 Coding Material"}),
            json!({"id": 4, "full_name": "course files/unfiled"}),
        ];
        let at = |id: i64| json!({"folder_id": id});
        let none: Vec<(String, usize)> = Vec::new();
        assert_eq!(
            canvas_folder_path(&at(2), &folders, &none).as_deref(),
            Some("Lecture Slides")
        );
        assert_eq!(
            canvas_folder_path(&at(3), &folders, &none).as_deref(),
            Some("Coding Material/Week 2 Coding Material")
        );
        // No organization is not a folder to invent — these stay in the inbox
        // and the content-aware sorter proposes a home instead.
        assert_eq!(canvas_folder_path(&at(1), &folders, &none), None);
        assert_eq!(canvas_folder_path(&at(4), &folders, &none), None);
        assert_eq!(canvas_folder_path(&at(99), &folders, &none), None);
        assert_eq!(canvas_folder_path(&json!({}), &folders, &none), None);
    }

    /// A sync must not undo the naming consistency the sorter is asked to keep:
    /// one course calling its decks "Lecture Slides" should not earn that class
    /// a second folder beside the "Slides" every other class has.
    #[test]
    fn files_land_under_the_name_the_library_already_uses() {
        let vocabulary = vec![
            ("Slides".to_string(), 3),
            ("Reading Material".to_string(), 2),
            ("Syllabus".to_string(), 2),
        ];
        let folders = vec![
            json!({"id": 2, "full_name": "course files/Lecture Slides"}),
            json!({"id": 5, "full_name": "course files/SYLLABUS"}),
            json!({"id": 6, "full_name": "course files/Coding Material"}),
        ];
        let at = |id: i64| json!({"folder_id": id});
        assert_eq!(
            canvas_folder_path(&at(2), &folders, &vocabulary).as_deref(),
            Some("Slides")
        );
        // Casing drift is the same folder, so it takes the library's casing.
        assert_eq!(
            canvas_folder_path(&at(5), &folders, &vocabulary).as_deref(),
            Some("Syllabus")
        );
        // Nothing in the vocabulary means this — Canvas's own name stands, and
        // becomes vocabulary itself once the file is filed.
        assert_eq!(
            canvas_folder_path(&at(6), &folders, &vocabulary).as_deref(),
            Some("Coding Material")
        );
    }

    /// The match has to stop at a word boundary. Renaming on a resemblance is
    /// worse than a second folder: it files material somewhere it is not.
    #[test]
    fn a_resemblance_is_not_a_match() {
        let vocabulary = vec![("Slides".to_string(), 3), ("Notes".to_string(), 1)];
        let same = |s: &str| canonical_segment(s, &vocabulary) == s;
        assert!(same("Preslides"), "matched mid-word");
        assert!(same("Slides Archive"), "matched a prefix, not a suffix");
        assert!(same("Footnotes"), "matched mid-word");
        assert_eq!(canonical_segment("Lecture Slides", &vocabulary), "Slides");
        assert_eq!(canonical_segment("anything", &[]), "anything");
    }

    /// A numbered qualifier is what tells one division's folder from another's.
    /// Dropping it merges every week's folder into a single one, and then the
    /// files inside them collide by name.
    #[test]
    fn a_numbered_qualifier_survives_the_vocabulary() {
        let vocabulary = vec![("Slides".to_string(), 3)];
        assert_eq!(canonical_segment("Week 2 Slides", &vocabulary), "Week 2 Slides");
        assert_eq!(canonical_segment("Week 3 Slides", &vocabulary), "Week 3 Slides");
    }

    /// An exact match beats a suffix match on a more widely shared name. Tested
    /// per entry, the first *entry* won rather than the better *match*.
    #[test]
    fn an_exact_name_outranks_a_more_popular_suffix() {
        let vocabulary = vec![("Material".to_string(), 4), ("Reading Material".to_string(), 2)];
        assert_eq!(
            canonical_segment("Reading Material", &vocabulary),
            "Reading Material"
        );
        // And the suffix rule still applies where nothing matches exactly.
        assert_eq!(canonical_segment("Supplementary Material", &vocabulary), "Material");
    }

    /// Once a folder's own name is in the vocabulary, mapping its children onto
    /// it would nest it inside itself and propose a different destination than
    /// the sync before — which is what "re-syncing is a no-op" rules out.
    #[test]
    fn a_child_folder_is_not_renamed_onto_its_parent() {
        let vocabulary = vec![("Coding Material".to_string(), 2)];
        let folders = vec![json!({
            "id": 3,
            "full_name": "course files/Coding Material/Week 2 Coding Material"
        })];
        assert_eq!(
            canvas_folder_path(&json!({"folder_id": 3}), &folders, &vocabulary).as_deref(),
            Some("Coding Material/Week 2 Coding Material")
        );
    }

    /// Canvas names are the professor's, not a path builder's.
    #[test]
    fn keeps_a_downloaded_name_inside_its_folder() {
        let clean = |s: &str| sanitize_name(s).unwrap_or_default();
        assert_eq!(clean("Week 1/2 review.pdf"), "Week 1-2 review.pdf");
        // A dot-leading name would land and then be invisible to the scanner.
        assert_eq!(clean("  ..hidden.pdf "), "hidden.pdf");
        assert_eq!(clean(".DS_Store"), "DS_Store");
        assert_eq!(clean("Normal Name.pptx"), "Normal Name.pptx");

        // The two properties that actually matter: whatever comes back is a
        // single path segment, and it cannot be a relative-parent reference.
        for hostile in ["../../etc/passwd", "..", ".", "/etc/passwd", "a\\b"] {
            let out = clean(hostile);
            assert!(!out.contains('/'), "{hostile} -> {out}");
            assert!(!out.contains('\\'), "{hostile} -> {out}");
            assert!(!out.starts_with('.'), "{hostile} -> {out}");
        }
        // Nothing usable is left, and the caller skips rather than resolving
        // the download to the inbox folder itself.
        assert_eq!(sanitize_name(".."), None);
        assert_eq!(sanitize_name("   "), None);
    }

    #[test]
    fn writes_whole_point_values_without_a_decimal() {
        assert_eq!(trim_number(100.0), "100");
        assert_eq!(trim_number(2.5), "2.5");
    }

    /// The retry fires for a fetch that never completed, and for nothing else.
    /// A refusal would answer the same way twice, and a retry loop is the one
    /// thing that could make a sync approach the rate limit.
    #[test]
    fn only_a_page_level_failure_is_retried() {
        let page = anyhow::Error::from(crate::canvas::PageFailure("Load failed".into()))
            .context("the Canvas page could not request /files/9/download");
        assert!(is_transient(&page));

        let refused = anyhow::anyhow!("Canvas answered 404 for /files/9/download: not found");
        assert!(!is_transient(&refused));

        // The rule used to read the rendered chain, which by this point also
        // carries the request path and 200 characters of Canvas's own response
        // body — so a maintenance page merely containing the word earned a
        // retry, and a WebKit rewording would have silently stopped one.
        let prose = anyhow::anyhow!(
            "Canvas answered 500 for /files/9: <h1>network maintenance</h1> Load failed"
        );
        assert!(!is_transient(&prose), "matched on prose again");
    }

    /// What the whole re-sync no-op rests on: material the tree already holds
    /// has to be recognized from a Canvas listing entry, which knows only a
    /// name and a size.
    #[test]
    fn recognizes_material_the_class_already_holds() {
        let conn = crate::db::memory_db();
        conn.execute_batch(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind) VALUES
               (1, 'Slides/week1.pptx', 'a', 100, 0, 'pptx'),
               (1, 'deck.pdf', 'b', 200, 0, 'pdf'),
               (2, 'Slides/other.pptx', 'c', 300, 0, 'pptx');",
        )
        .expect("fixture");

        let seen = indexed_files(&conn, 1).expect("indexed");
        assert!(seen.contains(&("week1.pptx".to_string(), 100)), "nested file");
        assert!(seen.contains(&("deck.pdf".to_string(), 200)), "file at the class root");
        assert!(
            !seen.contains(&("other.pptx".to_string(), 300)),
            "another class's file counted as this one's"
        );
        // Same name, different bytes: a revised deck is not the one on disk.
        assert!(!seen.contains(&("week1.pptx".to_string(), 101)));
    }

    /// The predicate between a Canvas submission and a grade item. A muted
    /// grade, an ungraded or excused submission, and an assignment with no
    /// points possible are all refused; a posted zero is a grade.
    #[test]
    fn only_a_graded_posted_unexcused_submission_is_a_grade() {
        let base = json!({
            "id": 5001, "name": " Quiz 1 ", "points_possible": 10, "assignment_group_id": 901,
            "submission": {
                "score": 9, "posted_at": "2026-09-03T20:00:00Z",
                "graded_at": "2026-09-03T19:59:00Z", "excused": false, "workflow_state": "graded"
            }
        });
        let graded = graded_and_posted(&base).expect("a grade");
        assert_eq!(graded.assignment_id, "5001");
        assert_eq!(graded.group_id, "901");
        assert_eq!(graded.name, "Quiz 1");
        assert_eq!((graded.score, graded.max_score), (9.0, 10.0));
        assert_eq!(graded.graded_at.as_ref().map(String::len), Some(10), "a local calendar day");

        let with = |change: &dyn Fn(&mut Value)| {
            let mut value = base.clone();
            change(&mut value);
            graded_and_posted(&value)
        };
        assert!(with(&|v| v["submission"]["posted_at"] = Value::Null).is_none(), "muted");
        // Muted is told apart from ungraded, so the report can say the
        // professor is holding a grade.
        let mut muted = base.clone();
        muted["submission"]["posted_at"] = Value::Null;
        assert_eq!(grade_state(&muted), GradeState::Unposted);
        let mut ungraded = base.clone();
        ungraded["submission"]["score"] = Value::Null;
        assert_eq!(grade_state(&ungraded), GradeState::Nothing);
        let mut excused = muted.clone();
        excused["submission"]["excused"] = json!(true);
        assert_eq!(grade_state(&excused), GradeState::Nothing, "excused outranks muted");
        assert!(with(&|v| v["submission"]["score"] = Value::Null).is_none(), "ungraded");
        assert!(with(&|v| v["submission"]["excused"] = json!(true)).is_none(), "excused");
        assert!(with(&|v| v["points_possible"] = json!(0)).is_none(), "zero points");
        assert!(with(&|v| v["points_possible"] = Value::Null).is_none(), "no points");
        assert!(
            with(&|v| {
                v.as_object_mut().expect("object").remove("submission");
            })
            .is_none(),
            "read without include[]=submission"
        );
        assert_eq!(
            with(&|v| v["submission"]["score"] = json!(0)).map(|g| g.score),
            Some(0.0),
            "a posted zero is a grade"
        );
    }

    #[test]
    fn group_weights_count_only_when_the_course_applies_them() {
        assert!(applies_group_weights(&json!({"apply_assignment_group_weights": true})));
        assert!(!applies_group_weights(&json!({"apply_assignment_group_weights": false})));
        assert!(!applies_group_weights(&json!({})));
    }

    /// An announcement lands once, is updated in place when the professor
    /// edits it, and is left alone otherwise — the second sync writes nothing.
    /// A row another class holds is refused rather than moved.
    #[test]
    fn an_announcement_is_recorded_once_and_updated_in_place() {
        let conn = crate::db::memory_db();
        let notice = |body: &'static str, posted_at: &'static str| CanvasAnnouncement {
            id: "5352500",
            title: "Today's Office Hours Postponed",
            body,
            posted_at,
        };
        assert!(record_announcement(&conn, 2, &notice("until 6 PM", "2026-09-02T15:03")).expect("insert"));
        assert!(!record_announcement(&conn, 2, &notice("until 6 PM", "2026-09-02T15:03")).expect("same"));
        assert!(record_announcement(&conn, 2, &notice("until 7 PM", "2026-09-02T15:03")).expect("edited"));
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM announcements", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 1);
        assert_eq!(list_announcements(&conn, 2).expect("list")[0].body, "until 7 PM");
        assert!(record_announcement(&conn, 3, &notice("until 7 PM", "2026-09-02T15:03")).is_err());

        // Newest first, whatever order Canvas listed them in.
        let older = CanvasAnnouncement {
            id: "5343925",
            title: "Welcome",
            body: "Hi",
            posted_at: "2026-08-26T13:58",
        };
        record_announcement(&conn, 2, &older).expect("older");
        let titles: Vec<String> = list_announcements(&conn, 2)
            .expect("list")
            .into_iter()
            .map(|a| a.title)
            .collect();
        assert_eq!(titles, ["Today's Office Hours Postponed", "Welcome"]);
        assert!(list_announcements(&conn, 3).expect("other class").is_empty());
    }

    /// Two pages with one title get two files whatever order Canvas lists
    /// them in, a page titled like the syllabus lands beside it, a title or
    /// slug that is not a path segment is made one, and a name that cannot
    /// be made a segment is skipped.
    #[test]
    fn page_file_stems_are_unique_and_independent_of_listing_order() {
        let listing = vec![
            json!({"title": "Home", "url": "home-2"}),
            json!({"title": "home", "url": "home-3"}),
            json!({"title": "Module 2", "url": "module-2-2"}),
        ];
        let duplicated = duplicated_titles(&listing);
        let stems = |order: &[usize]| -> Vec<String> {
            let mut taken: HashSet<String> = HashSet::new();
            taken.insert(SYLLABUS_STEM.to_lowercase());
            order
                .iter()
                .map(|&i| {
                    let page = &listing[i];
                    page_file_stem(
                        page["title"].as_str().unwrap(),
                        page["url"].as_str().unwrap(),
                        &duplicated,
                        &mut taken,
                    )
                    .expect("a stem")
                })
                .collect()
        };
        assert_eq!(stems(&[0, 1, 2]), ["Home (home-2)", "home (home-3)", "Module 2"]);
        // Reversed, each page still lands on its own file.
        assert_eq!(stems(&[1, 0, 2]), ["home (home-3)", "Home (home-2)", "Module 2"]);

        let none = HashSet::new();
        let mut taken: HashSet<String> = HashSet::new();
        taken.insert(SYLLABUS_STEM.to_lowercase());
        assert_eq!(
            page_file_stem("Syllabus", "syllabus", &none, &mut taken).as_deref(),
            Some("Syllabus (syllabus)")
        );
        assert_eq!(
            page_file_stem("Module 1/2: Intro", "module-1-2", &none, &mut taken).as_deref(),
            Some("Module 1-2: Intro")
        );
        // The slug becomes part of a file name too, so it is made a segment:
        // the separator becomes a dash and the leading dots go.
        let dup: HashSet<String> = ["week 3".to_string()].into_iter().collect();
        assert_eq!(
            page_file_stem("Week 3", "../week-3", &dup, &mut taken).as_deref(),
            Some("Week 3 (-week-3)")
        );
        assert_eq!(page_file_stem("..", "dots", &none, &mut taken), None);
        // A repeat of a stem already claimed this sync is refused rather
        // than written over.
        assert_eq!(page_file_stem("Module 1/2: Intro", "again", &none, &mut taken), None);
        // A title at Canvas's limit still fits a file name.
        let long = "Week 3 ".repeat(40);
        let stem = page_file_stem(&long, "week-3", &none, &mut taken).expect("a stem");
        assert!(stem.chars().count() <= MAX_STEM_CHARS + 1, "{stem}");
        assert!(stem.starts_with("Week 3 Week 3"), "{stem}");
    }

    /// The mirrored file is text with a header, and an unchanged page is not
    /// rewritten — the re-sync no-op, measured on disk.
    #[test]
    fn a_page_is_mirrored_as_markdown_and_rewritten_only_when_it_changed() {
        let text = strip_html(
            "<h2><span>Responsible AI</span></h2><p>By the end of this <b>module</b>, PHI&nbsp;&amp; HIPAA.</p><img src=\"x.png\">",
        );
        let content = page_markdown(
            "Module\n 2",
            "Canvas page",
            Some("2026-09-01T02:41"),
            Some("https://ufl.instructure.com/courses/576174/pages/module-2-2"),
            &text,
        );
        assert!(content.starts_with("# Module 2\n\nCanvas page, last edited 2026-09-01 · https://"), "{content}");
        assert!(
            content.contains("\nResponsible AI\n\nBy the end of this module, PHI & HIPAA.\n"),
            "{content}"
        );
        assert!(!content.contains('<'), "{content}");

        let dir = std::env::temp_dir().join(format!("classhub-pages-{}", std::process::id()));
        let path = dir.join("Canvas").join("Module 2.md");
        assert!(write_if_changed(&path, &content).expect("first write"));
        assert!(!write_if_changed(&path, &content).expect("same content"));
        assert!(write_if_changed(&path, "# Module 2\n\nchanged\n").expect("changed"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A page retitled or unpublished on Canvas loses its file; what the sync
    /// wrote or confirmed stays, and so does anything that is not a page.
    #[test]
    fn a_page_canvas_no_longer_lists_is_removed_from_the_cache() {
        let dir = std::env::temp_dir().join(format!("classhub-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        for name in ["Home.md", "Module 2.md", "Syllabus.md", "notes.txt"] {
            std::fs::write(dir.join(name), "x").expect("file");
        }
        let kept: HashSet<String> = ["home", "syllabus"].into_iter().map(String::from).collect();
        assert_eq!(prune_stale_texts(&dir, &kept).expect("prune"), 1);
        assert!(dir.join("Home.md").is_file());
        assert!(dir.join("Syllabus.md").is_file());
        assert!(!dir.join("Module 2.md").exists(), "the retitled page's old file");
        assert!(dir.join("notes.txt").is_file(), "not a page, not the sync's to remove");
        // A folder no sync has written yet is nothing to reconcile.
        assert_eq!(prune_stale_texts(&dir.join("missing"), &kept).expect("absent"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A launch syncs when nothing ever has or the last sync is a day old,
    /// and never inside the day.
    #[test]
    fn a_launch_syncs_only_when_the_last_sync_is_a_day_old() {
        let now = 1_788_000_000;
        assert!(launch_sync_due(None, now));
        assert!(launch_sync_due(Some(now - LAUNCH_SYNC_AFTER), now));
        assert!(launch_sync_due(Some(now - 3 * LAUNCH_SYNC_AFTER), now));
        assert!(!launch_sync_due(Some(now - LAUNCH_SYNC_AFTER + 1), now));
        assert!(!launch_sync_due(Some(now), now));
    }
}
