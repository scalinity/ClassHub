//! SPEC §7.2 — what a Canvas sync actually does with the session `canvas.rs`
//! establishes.
//!
//! Reads only, and never on a timer. Three things come across:
//!
//! 1. **The course's own divisions** → `units` (SPEC §5), from its published
//!    modules. Today none of the four courses publishes any, so this reads an
//!    empty list and says so — the syllabus source in §7.2's precedence chain
//!    is what supplies them instead. The reader is written anyway because it
//!    costs one request and starts working the day a professor adds a module.
//! 2. **Assignments** → the existing `deadline_proposals` confirm queue, with
//!    Canvas's true due dates rather than a syllabus PDF's prose.
//! 3. **Course files** → downloaded into `_Inbox/` and proposed through the
//!    §10 move queue, because approval is what places a file, here as
//!    everywhere.
//!
//! Nothing is deleted. A unit that disappears from Canvas is kept (a
//! mid-semester reshuffle must not orphan a guide), and a re-sync updates in
//! place rather than duplicating.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter};

use crate::canvas::Session;
use crate::db::{audit, emit_hub_change, lock, now, with_conn, INBOX_DIR};
use crate::deadlines::Recorded;
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
    pub files_staged: usize,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<ClassOutcome>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// One sync at a time. The window is a single shared resource, and two syncs
/// would fight over it and over the inbox destination names.
static SYNCING: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Runs a sync on its own thread, reporting over `PROGRESS_EVENT`.
///
/// A plain thread rather than an async command, for the same reason
/// `lectures::spawn_add` uses one: this waits on a human completing SSO and
/// then on blocking HTTP, neither of which belongs on the async runtime.
pub fn spawn(app: &AppHandle, class_ids: Vec<i64>) -> Result<()> {
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
                Progress { stage: stage.to_string(), done, results, error },
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
            run(&app, &class_ids, &on_stage)
        }));
        match finished {
            Ok(Ok(results)) => {
                emit("Synced", true, Some(results), None);
                emit_hub_change(&app, "units");
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

/// The sync proper. `class_ids` empty means every class.
fn run(
    app: &AppHandle,
    class_ids: &[i64],
    on_stage: &dyn Fn(&str),
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
    let session = Session::open(app, on_stage)?;

    on_stage("Reading your courses…");
    let courses = session.get_all("/api/v1/courses?enrollment_state=active", on_stage)?;

    let mut results = Vec::new();
    for class in classes {
        on_stage(&format!("Syncing {}…", class.display_name));
        let mut outcome = ClassOutcome {
            class_id: class.id,
            class_name: class.display_name.clone(),
            canvas_course: None,
            units_added: 0,
            deadlines_proposed: 0,
            files_staged: 0,
            notes: Vec::new(),
            error: None,
        };
        // One class's failure costs that class, never the rest of the sync.
        if let Err(e) = sync_class(app, &session, &class, &courses, &mut outcome, on_stage) {
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
    if let Err(e) = sync_assignments(app, session, class, course_id, outcome, on_stage) {
        note_or_fail(outcome, e, "assignments")?;
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
// Assignments → the deadline confirm queue (SPEC §11)

fn sync_assignments(
    app: &AppHandle,
    session: &Session,
    class: &ClassRow,
    course_id: i64,
    outcome: &mut ClassOutcome,
    on_stage: &dyn Fn(&str),
) -> Result<()> {
    let path = format!("/api/v1/courses/{course_id}/assignments");
    let assignments = session.get_all(&path, on_stage)?;

    let (proposed, undated, known) = with_conn(app, |conn| {
        let mut proposed = 0usize;
        let mut undated = 0usize;
        let mut known = 0usize;
        for assignment in &assignments {
            let Some(title) = assignment["name"].as_str().map(str::trim).filter(|n| !n.is_empty())
            else {
                continue;
            };
            // An assignment with no due date is real but not a deadline. It
            // would have to be invented to store one, which is exactly what
            // reading Canvas was supposed to stop.
            let Some(due_at) = assignment["due_at"].as_str().and_then(local_iso) else {
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
                conn, class.id, title, kind, &due_at, Some(&notes), "canvas",
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
        Ok((proposed, undated, known))
    })?;

    outcome.deadlines_proposed = proposed;
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
    if proposed > 0 {
        emit_hub_change(app, "deadlineProposals");
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
    crate::sorter::upsert_proposal(conn, class_id, "canvas", source_rel, dest_rel, reasoning, None)
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
}
