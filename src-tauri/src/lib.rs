/// SPEC §13: everything credential-shaped lives in the macOS Keychain under
/// this one service, each feature adding its own account beneath it — the
/// Anthropic API key, and the Canvas session cookies (§7.2). One literal, so
/// two accounts cannot drift onto different services.
pub(crate) const KEYCHAIN_SERVICE: &str = "classhub";

mod canvas;
mod canvas_sync;
mod chat;
mod db;
mod deadlines;
mod extract;
mod grades;
mod guides;
mod jobs;
mod lectures;
mod notebook;
mod notifications;
mod announcements;
mod recordings;
mod notes;
mod scanner;
mod settings;
mod shift;
mod sorter;
mod tools;
mod transcribe;
mod transcripts;
mod tray;
mod undo;
mod units;
mod zoom;

use std::sync::Mutex;

use anyhow::Context;
use rusqlite::Connection;
use tauri::Manager;

pub(crate) struct Db(pub(crate) Mutex<Connection>);

/// Tauri's own app data directory: `~/Library/Application Support/com.danny.classhub`.
///
/// Every build resolves it the same way, so the installed app and a dev build
/// open one database. A hand-picked folder name is how they once stopped doing
/// so: the installed build kept resolving the default name and started a second
/// database in the empty folder it found there.
pub(crate) fn data_dir(app: &tauri::AppHandle) -> anyhow::Result<std::path::PathBuf> {
    app.path().app_data_dir().context("resolving app data dir")
}

/// `today` is client-formatted YYYY-MM-DD, for the current division (SPEC §8.5).
#[tauri::command(async)]
fn list_classes(state: tauri::State<Db>, today: String) -> Result<Vec<db::ClassCard>, String> {
    let conn = db::lock(&state.0);
    db::list_classes(&conn, &today).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn scan_class(
    app: tauri::AppHandle,
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<scanner::TreeNode>, String> {
    let scan = scanner::scan_class(&state.0, class_id).map_err(|e| format!("{e:#}"))?;
    // Staleness, the divisions' counts and the lectures read rows the scan
    // rewrote, and the tree's own refetch does not reach them.
    if scan.changed {
        db::emit_hub_change(&app, "index");
    }
    // SPEC §7: auto-extract after every scan; a no-change scan is a no-op there.
    extract::spawn_pipeline(&app, class_id);
    Ok(scan.tree)
}

#[tauri::command(async)]
fn reveal_in_finder(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<(), String> {
    let path = {
        let conn = db::lock(&state.0);
        scanner::resolve_rel(&conn, class_id, &rel_path).map_err(|e| format!("{e:#}"))?
    };
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[tauri::command(async)]
fn open_in_default_app(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<(), String> {
    let path = {
        let conn = db::lock(&state.0);
        scanner::resolve_rel(&conn, class_id, &rel_path).map_err(|e| format!("{e:#}"))?
    };
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
}

/// In-app file viewer: raw text of a class file, rendered by the frontend
/// (markdown, HTML, code). resolve_rel guards traversal; the cap keeps huge
/// artifacts in their default apps.
#[tauri::command(async)]
fn read_class_file(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<String, String> {
    const MAX_VIEW_BYTES: u64 = 8 * 1024 * 1024;
    let path = {
        let conn = db::lock(&state.0);
        scanner::resolve_rel(&conn, class_id, &rel_path).map_err(|e| format!("{e:#}"))?
    };
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if size > MAX_VIEW_BYTES {
        return Err("too large to view in-app — use its default app".into());
    }
    std::fs::read_to_string(&path).map_err(|e| format!("reading {rel_path}: {e}"))
}

/// SPEC §12: what the viewer frames for a PDF or a slide deck — the absolute
/// path its `asset:` URL is built from, inside the scope `allow_asset_root`
/// granted. A deck answers with its converted twin, or `None` while it has no
/// current one, which the tree turns into an open in the default app; an
/// error is a failure the tree shows.
#[tauri::command(async)]
fn pdf_view_path(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<Option<String>, String> {
    let conn = db::lock(&state.0);
    extract::pdf_view_path(&conn, class_id, &rel_path)
        .map(|p| p.map(|p| p.to_string_lossy().into_owned()))
        .map_err(|e| format!("{e:#}"))
}

/// SPEC §13: the asset protocol reaches the AIBHS root and nothing else. The
/// scope starts empty in `tauri.conf.json`, because the root is a setting the
/// config cannot follow, and is widened here to whatever the setting says —
/// at launch, and again when the setting changes.
pub(crate) fn allow_asset_root(app: &tauri::AppHandle, root: &std::path::Path) {
    if let Err(e) = app.asset_protocol_scope().allow_directory(root, true) {
        eprintln!("asset scope: could not allow {}: {e}", root.display());
    }
}

/// SPEC §8.1: manual synthesis trigger. The label is the guide footer's
/// display-only generated-at stamp, formatted client-side.
#[tauri::command]
fn synthesize_module(
    app: tauri::AppHandle,
    class_id: i64,
    module_rel_path: String,
    generated_at_label: String,
) -> Result<i64, String> {
    guides::synthesize_module(&app, class_id, &module_rel_path, &generated_at_label)
        .map_err(|e| format!("{e:#}"))
}

/// SPEC §8.1: synthesis for one of the course's own divisions — the guide a
/// filed lecture actually reaches, through its corpus note (SPEC §8.5).
#[tauri::command]
fn synthesize_unit(
    app: tauri::AppHandle,
    class_id: i64,
    unit_id: i64,
    generated_at_label: String,
) -> Result<i64, String> {
    guides::synthesize_unit(&app, class_id, unit_id, &generated_at_label)
        .map_err(|e| format!("{e:#}"))
}

/// SPEC §8.2: manual semester-master trigger (exclusive, long-running job).
#[tauri::command]
fn synthesize_master(
    app: tauri::AppHandle,
    class_id: i64,
    generated_at_label: String,
) -> Result<i64, String> {
    guides::synthesize_master(&app, class_id, &generated_at_label).map_err(|e| format!("{e:#}"))
}

/// SPEC §8.3: the workspace's PRACTICE EXAM action. `scope` is a folder rel
/// path, `unit:<name>` for one of the course's divisions, or `master`; the
/// two labels are formatted client-side — the footer stamp and the file
/// name's YYYY-MM-DD.
#[tauri::command]
fn generate_practice(
    app: tauri::AppHandle,
    class_id: i64,
    scope: String,
    generated_at_label: String,
    date_label: String,
    focus: Option<String>,
) -> Result<i64, String> {
    guides::generate_practice(&app, class_id, &scope, focus.as_deref(), &generated_at_label, &date_label)
        .map(|(job_id, _)| job_id)
        .map_err(|e| format!("{e:#}"))
}

/// SPEC §6: resume a failed master run with `--resume <session_id>`.
#[tauri::command]
fn resume_master_guide(app: tauri::AppHandle, job_id: i64) -> Result<i64, String> {
    guides::resume_master(&app, job_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn list_guides(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<guides::GuideInfo>, String> {
    let conn = db::lock(&state.0);
    guides::list_guides(&conn, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn read_guide(
    state: tauri::State<Db>,
    class_id: i64,
    scope: String,
) -> Result<String, String> {
    let conn = db::lock(&state.0);
    guides::read_guide(&conn, class_id, &scope).map_err(|e| format!("{e:#}"))
}

/// Read-only note listing for the workspace (M8; the editor lands in M11).
#[tauri::command(async)]
fn list_notes(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<notes::NoteFile>, String> {
    let conn = db::lock(&state.0);
    notes::list_notes(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// Practice exams on disk (SPEC §8.3) for the workspace listing.
#[tauri::command(async)]
fn list_practice(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<guides::PracticeInfo>, String> {
    let conn = db::lock(&state.0);
    guides::list_practice(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// The notes editor's save (SPEC §11) — content lands in `<Class>/Notes/`.
/// `rel_path` targets an exact existing file; without it the title names one.
#[tauri::command(async)]
fn save_note(
    app: tauri::AppHandle,
    class_id: i64,
    title: String,
    content: String,
    rel_path: Option<String>,
) -> Result<notes::SavedNote, String> {
    notes::save_from_ui(&app, class_id, &title, &content, rel_path.as_deref())
        .map_err(|e| format!("{e:#}"))
}

// --- Grades (SPEC §11) --------------------------------------------------------

#[tauri::command(async)]
fn list_grades(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<grades::GradesInfo, String> {
    let conn = db::lock(&state.0);
    grades::list_grades(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// Create (id None) or amend a weighted category — same rules as the chat tool.
#[tauri::command]
fn save_grade_category(
    app: tauri::AppHandle,
    class_id: i64,
    id: Option<i64>,
    name: String,
    weight: f64,
) -> Result<(), String> {
    grades::save_category(&app, class_id, id, &name, weight).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn delete_grade_category(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    grades::delete_category(&app, id).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn save_grade_item(
    app: tauri::AppHandle,
    category_id: i64,
    id: Option<i64>,
    name: String,
    score: f64,
    max_score: f64,
) -> Result<(), String> {
    grades::save_item(&app, category_id, id, &name, score, max_score)
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn delete_grade_item(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    grades::delete_item(&app, id).map_err(|e| format!("{e:#}"))
}

// --- App settings (SPEC §11 M11) ----------------------------------------------

#[tauri::command(async)]
fn get_app_settings(app: tauri::AppHandle) -> Result<settings::AppSettings, String> {
    settings::get(&app).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn set_aibhs_root(app: tauri::AppHandle, path: String) -> Result<(), String> {
    settings::set_aibhs_root(&app, &path).map_err(|e| format!("{e:#}"))
}

/// Starts an ingestion and returns immediately; progress and the result arrive
/// on `lectures::PROGRESS_EVENT`.
#[tauri::command]
fn add_lecture(app: tauri::AppHandle, request: lectures::AddRequest) {
    lectures::spawn_add(&app, request);
}

#[tauri::command(async)]
fn digest_lecture(
    app: tauri::AppHandle,
    class_id: i64,
    rel_path: String,
    date: String,
) -> Result<i64, String> {
    lectures::enqueue_digest(&app, class_id, &rel_path, &date).map_err(|e| format!("{e:#}"))
}

/// The weeks a lecture can be filed into, and which one this date lands on.
///
/// Both come from the course's own schedule, never from arithmetic (SPEC §8.5)
/// — which is why the default is resolved here rather than in the form.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct LectureWeeks {
    slots: Vec<units::WeekSlot>,
    /// The week nearest `date`, or `None` where the course published no dates
    /// to measure against and the form has to ask outright.
    default_week: Option<i64>,
}

#[tauri::command(async)]
fn lecture_weeks(
    state: tauri::State<Db>,
    class_id: i64,
    date: String,
) -> Result<LectureWeeks, String> {
    let conn = db::lock(&state.0);
    let slots = units::week_slots(&conn, class_id).map_err(|e| format!("{e:#}"))?;
    Ok(LectureWeeks {
        default_week: units::nearest_week(&slots, &date),
        slots,
    })
}

/// Which division each filed lecture feeds (SPEC §8.5), for the Lectures list.
/// SPEC §7.1: the recordings found behind the course's Zoom tool that still
/// wait for a capture, for the Lectures section.
#[tauri::command(async)]
fn list_recordings(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<recordings::RecordingInfo>, String> {
    let conn = db::lock(&state.0);
    recordings::list_waiting(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// SPEC §7.1: `Find recordings` — lists the course's recordings through a
/// Canvas session and captures what waits, reporting on
/// `recordings::PROGRESS_EVENT`.
#[tauri::command]
fn find_recordings(app: tauri::AppHandle, class_id: i64) -> Result<(), String> {
    recordings::spawn_find(&app, class_id).map_err(|e| format!("{e:#}"))
}

/// The player link of a waiting recording, for the Add lecture form to open
/// pre-filled when its week is the form's to pick.
#[tauri::command(async)]
fn recording_play_url(state: tauri::State<Db>, id: i64) -> Result<Option<String>, String> {
    let conn = db::lock(&state.0);
    recordings::play_url_of(&conn, id).map_err(|e| format!("{e:#}"))
}

/// The form filed a waiting recording: its row leaves the listing.
#[tauri::command]
fn mark_recording_filed(app: tauri::AppHandle, id: i64, rel_path: String) -> Result<(), String> {
    db::with_conn(&app, |conn| recordings::mark_filed(conn, id, &rel_path)).map_err(|e| format!("{e:#}"))?;
    db::emit_hub_change(&app, "recordings");
    Ok(())
}

/// SPEC §7.2: the checkbox on a to-do under a notice.
#[tauri::command]
fn set_announcement_action_done(app: tauri::AppHandle, id: i64, done: bool) -> Result<(), String> {
    announcements::set_action_done(&app, id, done).map_err(|e| format!("{e:#}"))
}

/// SPEC §8.4: what the professor flagged, for the workspace's Flagged section.
#[tauri::command(async)]
fn list_hints(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<lectures::HintInfo>, String> {
    let conn = db::lock(&state.0);
    lectures::list_hints(&conn, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn list_lecture_contributions(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<lectures::Contribution>, String> {
    let conn = db::lock(&state.0);
    lectures::list_contributions(&conn, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn set_parakeet_python(app: tauri::AppHandle, path: String) -> Result<(), String> {
    settings::set_parakeet_python(&app, &path).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_job_model(app: tauri::AppHandle, model: String) -> Result<(), String> {
    settings::set_job_model(&app, &model).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_job_effort(app: tauri::AppHandle, effort: String) -> Result<(), String> {
    settings::set_job_effort(&app, &effort).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_job_concurrency(app: tauri::AppHandle, count: usize) -> Result<(), String> {
    settings::set_job_concurrency(&app, count).map_err(|e| format!("{e:#}"))?;
    // A raised limit should start queued jobs now, not at the next enqueue.
    // The setter's Db guard is released by here, so pump's Db-before-manager
    // lock ordering holds.
    jobs::poke(&app);
    Ok(())
}

// --- Deadlines + syllabus scan (SPEC §11) -------------------------------------

/// Every deadline across every class, due-soonest first; the dashboard strip
/// and the per-class list both filter this client-side.
#[tauri::command(async)]
fn list_deadlines(state: tauri::State<Db>) -> Result<Vec<deadlines::DeadlineInfo>, String> {
    let conn = db::lock(&state.0);
    deadlines::list_deadlines(&conn).map_err(|e| format!("{e:#}"))
}

/// Create (id None) or amend a deadline — same validation as the chat tool.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn save_deadline(
    app: tauri::AppHandle,
    class_id: i64,
    id: Option<i64>,
    title: String,
    kind: String,
    due_at: String,
    notes: Option<String>,
) -> Result<(), String> {
    deadlines::save_deadline(&app, class_id, id, &title, &kind, &due_at, notes.as_deref())
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_deadline_status(app: tauri::AppHandle, id: i64, done: bool) -> Result<(), String> {
    deadlines::set_deadline_status(&app, id, done).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn delete_deadline(app: tauri::AppHandle, id: i64) -> Result<(), String> {
    deadlines::delete_deadline(&app, id).map_err(|e| format!("{e:#}"))
}

/// Pending proposed deadlines for the class's confirm cards, and the series
/// among them (SPEC §11).
#[tauri::command(async)]
fn get_deadline_proposals(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<deadlines::DeadlineQueue, String> {
    let conn = db::lock(&state.0);
    deadlines::queue(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// A series card's Skip: every member card dismissed at once, one that
/// cannot be named while the rest go.
#[tauri::command]
fn dismiss_deadline_proposals(
    app: tauri::AppHandle,
    proposal_ids: Vec<i64>,
) -> Result<deadlines::BatchOutcome, String> {
    deadlines::dismiss_proposals(&app, &proposal_ids).map_err(|e| format!("{e:#}"))
}

/// SPEC §6: reverses the audit rows a notice carries, newest first.
#[tauri::command]
fn undo_audit(app: tauri::AppHandle, audit_ids: Vec<i64>) -> Result<undo::UndoOutcome, String> {
    undo::undo(&app, &audit_ids).map_err(|e| format!("{e:#}"))
}

/// Scan a chosen file (rel_path) or the whole class folder (None) for dated
/// items. `today` is client-formatted (std Rust cannot format a local date).
#[tauri::command]
fn run_syllabus_scan(
    app: tauri::AppHandle,
    class_id: i64,
    rel_path: Option<String>,
    today: String,
) -> Result<i64, String> {
    deadlines::run_scan(&app, class_id, rel_path.as_deref(), &today)
        .map_err(|e| format!("{e:#}"))
}

/// Approve or skip a proposed deadline. Approval stamps the deadline with the
/// proposal's own source, so it stays traceable to the reader that found it.
#[tauri::command]
fn resolve_deadline_proposal(
    app: tauri::AppHandle,
    proposal_id: i64,
    approve: bool,
) -> Result<String, String> {
    deadlines::resolve_proposal(&app, proposal_id, approve).map_err(|e| format!("{e:#}"))
}

/// ADD ALL: approve a batch — each card independently, one hub push at the end.
#[tauri::command]
fn approve_deadline_proposals(
    app: tauri::AppHandle,
    proposal_ids: Vec<i64>,
) -> Result<deadlines::BatchOutcome, String> {
    deadlines::approve_proposals(&app, &proposal_ids).map_err(|e| format!("{e:#}"))
}

// --- Canvas sync (SPEC §7.2) --------------------------------------------------

/// The course's own divisions for the workspace listing (SPEC §5).
#[tauri::command(async)]
fn list_units(state: tauri::State<Db>, class_id: i64) -> Result<Vec<units::UnitInfo>, String> {
    let conn = db::lock(&state.0);
    units::list_units(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// When Canvas data last came across, and how many classes are linked.
///
/// Deliberately not a "connected" state: holding a session cookie is not the
/// same as Canvas still honouring it, so there is nothing whose health could be
/// reported. The only way to find out is to sync.
#[tauri::command(async)]
fn canvas_status(state: tauri::State<Db>) -> Result<canvas_sync::CanvasStatus, String> {
    let conn = db::lock(&state.0);
    canvas_sync::status(&conn).map_err(|e| format!("{e:#}"))
}

/// Starts a sync and returns immediately; progress and results arrive on
/// `canvas_sync::PROGRESS_EVENT`. An empty `class_ids` syncs every class.
#[tauri::command]
fn sync_canvas(app: tauri::AppHandle, class_ids: Vec<i64>) -> Result<(), String> {
    canvas_sync::spawn(&app, class_ids).map_err(|e| format!("{e:#}"))
}

/// The class's Canvas announcements, newest first — the workspace's NOTICES
/// section (SPEC §7.2).
#[tauri::command(async)]
fn list_announcements(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<canvas_sync::AnnouncementInfo>, String> {
    let conn = db::lock(&state.0);
    canvas_sync::list_announcements(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// The mirrored Canvas syllabus page's class-relative path, once a sync has
/// written one — what the syllabus scan's picker offers (SPEC §11).
#[tauri::command(async)]
fn canvas_syllabus(state: tauri::State<Db>, class_id: i64) -> Result<Option<String>, String> {
    let conn = db::lock(&state.0);
    canvas_sync::canvas_syllabus_path(&conn, class_id).map_err(|e| format!("{e:#}"))
}

// --- Drop-to-sort (SPEC §10) --------------------------------------------------

/// Step 1: files dropped onto a class workspace are COPIED into
/// `<Class>/_Inbox/` (originals untouched); a sort job is auto-enqueued.
#[tauri::command(async)]
fn stage_inbox_files(
    app: tauri::AppHandle,
    class_id: i64,
    paths: Vec<String>,
) -> Result<sorter::StageResult, String> {
    sorter::stage_files(&app, class_id, &paths).map_err(|e| format!("{e:#}"))
}

/// The workspace queue: inbox files + pending proposals (chat and sort_job).
#[tauri::command(async)]
fn get_sort_state(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<sorter::SortState, String> {
    let conn = db::lock(&state.0);
    sorter::sort_state(&conn, class_id).map_err(|e| format!("{e:#}"))
}

/// Manual sort trigger: retry after a failure, or files left in the inbox.
#[tauri::command]
fn run_sort_job(app: tauri::AppHandle, class_id: i64) -> Result<i64, String> {
    sorter::run_sort_job(&app, class_id).map_err(|e| format!("{e:#}"))
}

/// SORT BY CONTENT on a Canvas card: a sort job over that one file, whose
/// destination replaces the Canvas placement (SPEC §7.2).
#[tauri::command]
fn sort_by_content(app: tauri::AppHandle, proposal_id: i64) -> Result<i64, String> {
    sorter::sort_by_content(&app, proposal_id).map_err(|e| format!("{e:#}"))
}

/// Steps 3–4: approve (move + index update + audit log, optionally to a
/// picker-chosen destination) or leave the file where it is.
#[tauri::command]
fn resolve_move_proposal(
    app: tauri::AppHandle,
    proposal_id: i64,
    approve: bool,
    dest_override: Option<String>,
) -> Result<String, String> {
    sorter::resolve_proposal(&app, proposal_id, approve, dest_override)
        .map_err(|e| format!("{e:#}"))
}

/// A file or folder named for a week, filed into that week's folder from its
/// row in Materials (SPEC §10); what it did comes back — the destination, the
/// count and the audit rows the notice's `Undo` reverses.
#[tauri::command]
fn file_under_week(
    app: tauri::AppHandle,
    class_id: i64,
    rel_path: String,
) -> Result<sorter::WeekFiling, String> {
    sorter::file_under_week(&app, class_id, &rel_path).map_err(|e| format!("{e:#}"))
}

/// Approve all over a class's pending cards (SPEC §10): one batch, one `Undo`.
#[tauri::command]
fn approve_move_proposals(
    app: tauri::AppHandle,
    class_id: i64,
) -> Result<deadlines::BatchOutcome, String> {
    sorter::approve_all(&app, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn list_jobs(state: tauri::State<Db>) -> Result<Vec<jobs::JobInfo>, String> {
    let conn = db::lock(&state.0);
    jobs::list_jobs(&conn).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn cancel_job(app: tauri::AppHandle, job_id: i64) -> Result<(), String> {
    jobs::cancel_job(&app, job_id).map_err(|e| format!("{e:#}"))
}

/// Snapshot of condensed progress so late subscribers can backfill (dedupe by seq).
#[tauri::command]
fn get_job_events(state: tauri::State<jobs::JobManager>, job_id: i64) -> Vec<jobs::ProgressEvent> {
    state.events_for(job_id)
}

/// Rolling tail of the source text a synthesis job is currently writing.
#[tauri::command]
fn get_job_tail(state: tauri::State<jobs::JobManager>, job_id: i64) -> String {
    state.tail_for(job_id)
}

#[tauri::command]
fn get_auth_check(state: tauri::State<jobs::JobManager>) -> jobs::AuthCheck {
    state.auth_check()
}

#[tauri::command]
fn run_auth_check(app: tauri::AppHandle) -> Result<i64, String> {
    jobs::enqueue_self_check(&app).map_err(|e| format!("{e:#}"))
}

// --- The idle shift (SPEC §6) and what sits beside it (§12) --------------------

#[tauri::command(async)]
fn get_shift_status(app: tauri::AppHandle) -> Result<shift::ShiftStatus, String> {
    shift::status(&app).map_err(|e| format!("{e:#}"))
}

/// `Run the shift now`: trigger `manual`, once a night like the rest.
#[tauri::command]
fn run_shift_now(app: tauri::AppHandle) -> Result<i64, String> {
    shift::run_now(&app).map_err(|e| format!("{e:#}"))
}

/// `Pause tonight`, or its undo; a run under way stops between jobs.
#[tauri::command]
fn pause_shift_tonight(app: tauri::AppHandle, paused: bool) -> Result<(), String> {
    shift::pause_tonight(&app, paused).map_err(|e| format!("{e:#}"))
}

/// One of the shift's settings by key — the window, the idle threshold, the
/// caps, the switches — validated backend-side.
#[tauri::command]
fn set_shift_setting(app: tauri::AppHandle, key: String, value: String) -> Result<(), String> {
    shift::set_shift_setting(&app, &key, &value).map_err(|e| format!("{e:#}"))
}

/// A job kind's own model (SPEC §6); `None` returns it to the global pair.
#[tauri::command]
fn set_job_kind_model(
    app: tauri::AppHandle,
    kind: String,
    model: Option<String>,
) -> Result<(), String> {
    settings::set_job_kind_model(&app, &kind, model.as_deref()).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_job_kind_effort(
    app: tauri::AppHandle,
    kind: String,
    effort: Option<String>,
) -> Result<(), String> {
    settings::set_job_kind_effort(&app, &kind, effort.as_deref()).map_err(|e| format!("{e:#}"))
}

/// One of the two notifications, on or off (SPEC §12).
#[tauri::command]
fn set_notify_setting(app: tauri::AppHandle, key: String, on: bool) -> Result<(), String> {
    settings::set_notify(&app, &key, on).map_err(|e| format!("{e:#}"))
}

/// The login item (SPEC §12): a LaunchAgent naming this build's executable,
/// registered and removed from Settings. Audited like a setting, since it
/// decides whether the shift is there to run at all.
#[tauri::command]
fn set_login_item(app: tauri::AppHandle, on: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    // The plist names this process's own executable, and a dev build's is
    // `target/debug/classhub`: registering it would open the wrong binary
    // at login, so only the installed app registers itself. Removal stays
    // open to every build, which is how a stale plist gets cleared.
    let exe = std::env::current_exe().map_err(|e| format!("{e}"))?;
    if on && (cfg!(debug_assertions) || !exe.starts_with("/Applications/")) {
        return Err(
            "only the installed app can be the login item — switch this on there".into(),
        );
    }
    let launcher = app.autolaunch();
    let result = if on { launcher.enable() } else { launcher.disable() };
    result.map_err(|e| format!("{e}"))?;
    db::with_conn(&app, |conn| {
        db::audit(
            conn,
            "ui.set_login_item",
            serde_json::json!({ "after": on, "executable": exe }),
        )?;
        Ok(())
    })
    .map_err(|e| format!("{e:#}"))
}

/// Whether the app is registered as a login item, for the Settings toggle.
pub(crate) fn login_item_enabled(app: &tauri::AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

// --- Agent chat (SPEC §9) ---------------------------------------------------

#[tauri::command(async)]
fn chat_settings(app: tauri::AppHandle) -> Result<chat::ChatSettings, String> {
    chat::settings(&app).map_err(|e| format!("{e:#}"))
}

/// The key goes to the macOS Keychain only — never the DB, never a file.
#[tauri::command(async)]
fn save_chat_key(app: tauri::AppHandle, key: String) -> Result<(), String> {
    chat::save_key(&app, &key).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn delete_chat_key(app: tauri::AppHandle) -> Result<(), String> {
    chat::delete_key(&app).map_err(|e| format!("{e:#}"))
}

/// Live `GET /v1/models` plus the model chat will use (SPEC §9).
///
/// `command(async)` rather than `async fn`: the body is blocking HTTP, and the
/// async runtime is the one place `reqwest::blocking` must never run. This
/// form puts it on the blocking pool, which is where it belongs.
#[tauri::command(async)]
fn list_chat_models(app: tauri::AppHandle) -> Result<chat::ModelList, String> {
    chat::list_models(&app).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn set_chat_model(app: tauri::AppHandle, model: String) -> Result<(), String> {
    chat::set_model(&app, &model).map_err(|e| format!("{e:#}"))
}

/// `output_config.effort`; an empty string leaves it to the model.
#[tauri::command]
fn set_chat_effort(app: tauri::AppHandle, effort: String) -> Result<(), String> {
    chat::set_effort(&app, &effort).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn list_chat_sessions(state: tauri::State<Db>) -> Result<Vec<chat::SessionInfo>, String> {
    let conn = db::lock(&state.0);
    chat::list_sessions(&conn).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn chat_history(
    state: tauri::State<Db>,
    session_id: i64,
) -> Result<Vec<chat::StoredMessage>, String> {
    let conn = db::lock(&state.0);
    chat::history(&conn, session_id).map_err(|e| format!("{e:#}"))
}

/// Records the question and answers it on a background thread; progress
/// arrives as `chat-event`. `today` (display) and `today_iso` (YYYY-MM-DD)
/// are formatted client-side (std Rust cannot format a local date); the
/// write tools stamp job prompts and practice file names with them.
/// `class_id` is the open workspace, if one is — context for this turn only.
#[tauri::command]
fn send_chat(
    app: tauri::AppHandle,
    session_id: Option<i64>,
    text: String,
    today: String,
    today_iso: String,
    class_id: Option<i64>,
) -> Result<i64, String> {
    chat::send(&app, session_id, &text, &today, &today_iso, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn stop_chat(app: tauri::AppHandle, session_id: i64) {
    chat::stop(&app, session_id);
}

/// SPEC §7 step 1: scan on launch, then auto-extract whatever the scans found
/// stale. Folders may legitimately be absent; skip those.
fn scan_and_extract_all(app: &tauri::AppHandle) {
    let db = app.state::<Db>();
    let class_ids: Vec<i64> = {
        let conn = match db.0.lock() {
            Ok(conn) => conn,
            Err(_) => return,
        };
        conn.prepare("SELECT id FROM classes")
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap_or_default()
    };
    for class_id in class_ids {
        match scanner::scan_class(&db.0, class_id) {
            Ok(scan) => {
                // `files` rather than `index`: this scan's tree reaches no
                // one, so an open workspace has to fetch it as well.
                if scan.changed {
                    db::emit_hub_change(app, "files");
                }
                extract::spawn_pipeline(app, class_id)
            }
            Err(e) => eprintln!("launch scan skipped class {class_id}: {e:#}"),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let data_dir = crate::data_dir(app.handle())?;
            std::fs::create_dir_all(&data_dir)?;
            let conn = match db::open(&data_dir.join("classhub.db")) {
                Ok(conn) => conn,
                Err(e) => report_startup_failure(&data_dir, &e),
            };
            jobs::startup_recovery(&conn)?;
            shift::startup_recovery(&conn)?;
            jobs::prune_logs(&data_dir);
            match db::aibhs_root(&conn) {
                Ok(root) => allow_asset_root(app.handle(), &root),
                Err(e) => eprintln!("asset scope: no AIBHS root to allow: {e:#}"),
            }
            app.manage(Db(Mutex::new(conn)));
            app.manage(jobs::JobManager::default());
            app.manage(chat::ChatState::default());
            app.manage(shift::ShiftState::default());
            tray::install(app.handle())?;
            if let Err(e) = jobs::startup_self_check(app.handle()) {
                eprintln!("startup self-check failed to enqueue: {e:#}");
            }
            // Off the main thread: this walks every class folder and hashes
            // whatever changed, and the window should not wait behind it. The
            // `hub-changed` pushes each class emits bring the UI up to date.
            // The launch's Canvas sync (SPEC §7.2) follows the scan on the
            // same thread, so its duplicate check reads a fresh index.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                // What an earlier sync left beside Canvas's rows folds first
                // (SPEC §7.2), so the lists open without a duplicate.
                deadlines::fold_at_launch(&handle);
                scan_and_extract_all(&handle);
                canvas_sync::sync_on_launch(&handle);
            });
            // The shift's minute check, and its catch-up of a missed window
            // (SPEC §6), on a thread of its own.
            shift::start_scheduler(app.handle());
            Ok(())
        })
        // Closing the window hides it (SPEC §12): the shift's thread survives
        // the window, and the tray, the dock and ⌘Q are the ways back and out.
        // Only the main window: the Canvas session and the Zoom capture
        // windows close when their reads end (`Session`'s drop, the
        // capture's `close`), and a hide in their place leaves the label
        // taken, so the next sync or capture in the same process is refused
        // as "still open".
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_classes,
            scan_class,
            reveal_in_finder,
            open_in_default_app,
            synthesize_module,
            synthesize_unit,
            synthesize_master,
            generate_practice,
            resume_master_guide,
            add_lecture,
            digest_lecture,
            lecture_weeks,
            list_lecture_contributions,
            list_hints,
            list_guides,
            read_guide,
            read_class_file,
            pdf_view_path,
            list_notes,
            list_practice,
            save_note,
            list_grades,
            save_grade_category,
            delete_grade_category,
            save_grade_item,
            delete_grade_item,
            get_app_settings,
            set_aibhs_root,
            set_parakeet_python,
            set_job_model,
            set_job_effort,
            set_job_concurrency,
            list_deadlines,
            save_deadline,
            set_deadline_status,
            delete_deadline,
            get_deadline_proposals,
            run_syllabus_scan,
            resolve_deadline_proposal,
            approve_deadline_proposals,
            list_units,
            canvas_status,
            sync_canvas,
            list_announcements,
            list_recordings,
            find_recordings,
            recording_play_url,
            mark_recording_filed,
            set_announcement_action_done,
            canvas_syllabus,
            stage_inbox_files,
            get_sort_state,
            run_sort_job,
            sort_by_content,
            resolve_move_proposal,
            file_under_week,
            approve_move_proposals,
            dismiss_deadline_proposals,
            undo_audit,
            list_jobs,
            cancel_job,
            get_job_events,
            get_job_tail,
            get_auth_check,
            run_auth_check,
            get_shift_status,
            run_shift_now,
            pause_shift_tonight,
            set_shift_setting,
            set_job_kind_model,
            set_job_kind_effort,
            set_notify_setting,
            set_login_item,
            chat_settings,
            save_chat_key,
            delete_chat_key,
            list_chat_models,
            set_chat_model,
            set_chat_effort,
            list_chat_sessions,
            chat_history,
            send_chat,
            stop_chat
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| match event {
            // Children spawned by the job runner are not reaped by dropping
            // their handles, so quitting mid-job would otherwise hand a live
            // `claude` to launchd. ExitRequested covers the ordinary quit;
            // Exit is the backstop for the paths that skip it. The shift's
            // run row and its `caffeinate` settle the same way.
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
                shift::shutdown(app);
                app.state::<jobs::JobManager>().shutdown();
            }
            // The dock icon, clicked with the window hidden (SPEC §12).
            tauri::RunEvent::Reopen { .. } => tray::show_main(app),
            _ => {}
        });
}

/// The database is the app: without it every screen is empty and every write
/// fails, so there is nothing useful to open a window onto. Launched from
/// Finder there is no stderr to read either, which is why the reason is left
/// in a file next to the database the migration could not open.
fn report_startup_failure(data_dir: &std::path::Path, e: &anyhow::Error) -> ! {
    let message = format!(
        "ClassHub could not open its database.\n\n{e:#}\n\n\
         The database is at {}. Nothing has been changed.",
        data_dir.join("classhub.db").display()
    );
    let _ = std::fs::write(data_dir.join("startup-error.txt"), &message);
    eprintln!("{message}");
    panic!("{message}");
}
