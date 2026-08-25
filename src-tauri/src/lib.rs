mod chat;
mod db;
mod deadlines;
mod extract;
mod grades;
mod guides;
mod jobs;
mod notes;
mod scanner;
mod settings;
mod sorter;
mod tools;

use std::sync::Mutex;

use rusqlite::Connection;
use tauri::Manager;

pub(crate) struct Db(pub(crate) Mutex<Connection>);

#[tauri::command(async)]
fn list_classes(state: tauri::State<Db>) -> Result<Vec<db::ClassCard>, String> {
    let conn = db::lock(&state.0);
    db::list_classes(&conn).map_err(|e| format!("{e:#}"))
}

#[tauri::command(async)]
fn scan_class(
    app: tauri::AppHandle,
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<scanner::TreeNode>, String> {
    let tree = scanner::scan_class(&state.0, class_id).map_err(|e| format!("{e:#}"))?;
    // SPEC §7: auto-extract after every scan; a no-change scan is a no-op there.
    extract::spawn_pipeline(&app, class_id);
    Ok(tree)
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

/// SPEC §8.2: manual semester-master trigger (exclusive, long-running job).
#[tauri::command]
fn synthesize_master(
    app: tauri::AppHandle,
    class_id: i64,
    generated_at_label: String,
) -> Result<i64, String> {
    guides::synthesize_master(&app, class_id, &generated_at_label).map_err(|e| format!("{e:#}"))
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
) -> Result<Vec<notes::NoteFile>, String> {
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

/// Pending syllabus-scan proposals for the class's confirm cards.
#[tauri::command(async)]
fn get_syllabus_proposals(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<deadlines::DeadlineProposal>, String> {
    let conn = db::lock(&state.0);
    deadlines::syllabus_proposals(&conn, class_id).map_err(|e| format!("{e:#}"))
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

/// Approve (insert with source='syllabus') or skip a proposed deadline.
#[tauri::command]
fn resolve_syllabus_proposal(
    app: tauri::AppHandle,
    proposal_id: i64,
    approve: bool,
) -> Result<String, String> {
    deadlines::resolve_proposal(&app, proposal_id, approve).map_err(|e| format!("{e:#}"))
}

/// ADD ALL: approve a batch — each card independently, one hub push at the end.
#[tauri::command]
fn approve_syllabus_proposals(
    app: tauri::AppHandle,
    proposal_ids: Vec<i64>,
) -> Result<deadlines::BatchOutcome, String> {
    deadlines::approve_proposals(&app, &proposal_ids).map_err(|e| format!("{e:#}"))
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
#[tauri::command]
fn send_chat(
    app: tauri::AppHandle,
    session_id: Option<i64>,
    text: String,
    today: String,
    today_iso: String,
) -> Result<i64, String> {
    chat::send(&app, session_id, &text, &today, &today_iso).map_err(|e| format!("{e:#}"))
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
            Ok(_) => extract::spawn_pipeline(app, class_id),
            Err(e) => eprintln!("launch scan skipped class {class_id}: {e:#}"),
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let conn = match db::open(&data_dir.join("classhub.db")) {
                Ok(conn) => conn,
                Err(e) => report_startup_failure(&data_dir, &e),
            };
            jobs::startup_recovery(&conn)?;
            jobs::prune_logs(&data_dir);
            app.manage(Db(Mutex::new(conn)));
            app.manage(jobs::JobManager::default());
            app.manage(chat::ChatState::default());
            if let Err(e) = jobs::enqueue_self_check(app.handle()) {
                eprintln!("startup self-check failed to enqueue: {e:#}");
            }
            // Off the main thread: this walks every class folder and hashes
            // whatever changed, and the window should not wait behind it. The
            // `hub-changed` pushes each class emits bring the UI up to date.
            let handle = app.handle().clone();
            std::thread::spawn(move || scan_and_extract_all(&handle));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_classes,
            scan_class,
            reveal_in_finder,
            open_in_default_app,
            synthesize_module,
            synthesize_master,
            resume_master_guide,
            list_guides,
            read_guide,
            read_class_file,
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
            set_job_model,
            set_job_effort,
            set_job_concurrency,
            list_deadlines,
            save_deadline,
            set_deadline_status,
            delete_deadline,
            get_syllabus_proposals,
            run_syllabus_scan,
            resolve_syllabus_proposal,
            approve_syllabus_proposals,
            stage_inbox_files,
            get_sort_state,
            run_sort_job,
            resolve_move_proposal,
            list_jobs,
            cancel_job,
            get_job_events,
            get_job_tail,
            get_auth_check,
            run_auth_check,
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
        .run(|app, event| {
            // Children spawned by the job runner are not reaped by dropping
            // their handles, so quitting mid-job would otherwise hand a live
            // `claude` to launchd. ExitRequested covers the ordinary quit;
            // Exit is the backstop for the paths that skip it.
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                app.state::<jobs::JobManager>().shutdown();
            }
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
