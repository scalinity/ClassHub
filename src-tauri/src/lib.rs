mod db;
mod extract;
mod guides;
mod jobs;
mod scanner;

use std::sync::Mutex;

use rusqlite::Connection;
use tauri::Manager;

pub(crate) struct Db(pub(crate) Mutex<Connection>);

#[tauri::command]
fn list_classes(state: tauri::State<Db>) -> Result<Vec<db::ClassCard>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_classes(&conn).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn scan_class(
    app: tauri::AppHandle,
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<scanner::TreeNode>, String> {
    let tree = {
        let mut conn = state.0.lock().map_err(|e| e.to_string())?;
        scanner::scan_class(&mut conn, class_id).map_err(|e| format!("{e:#}"))?
    };
    // SPEC §7: auto-extract after every scan; a no-change scan is a no-op there.
    extract::spawn_pipeline(&app, class_id);
    Ok(tree)
}

#[tauri::command]
fn reveal_in_finder(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<(), String> {
    let path = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        scanner::resolve_rel(&conn, class_id, &rel_path).map_err(|e| format!("{e:#}"))?
    };
    tauri_plugin_opener::reveal_item_in_dir(path).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_in_default_app(
    state: tauri::State<Db>,
    class_id: i64,
    rel_path: String,
) -> Result<(), String> {
    let path = {
        let conn = state.0.lock().map_err(|e| e.to_string())?;
        scanner::resolve_rel(&conn, class_id, &rel_path).map_err(|e| format!("{e:#}"))?
    };
    tauri_plugin_opener::open_path(path, None::<&str>).map_err(|e| e.to_string())
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

#[tauri::command]
fn list_guides(
    state: tauri::State<Db>,
    class_id: i64,
) -> Result<Vec<guides::GuideInfo>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    guides::list_guides(&conn, class_id).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn read_guide(
    state: tauri::State<Db>,
    class_id: i64,
    scope: String,
) -> Result<String, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    guides::read_guide(&conn, class_id, &scope).map_err(|e| format!("{e:#}"))
}

#[tauri::command]
fn list_jobs(state: tauri::State<Db>) -> Result<Vec<jobs::JobInfo>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
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

#[tauri::command]
fn get_auth_check(state: tauri::State<jobs::JobManager>) -> jobs::AuthCheck {
    state.auth_check()
}

#[tauri::command]
fn run_auth_check(app: tauri::AppHandle) -> Result<i64, String> {
    jobs::enqueue_self_check(&app).map_err(|e| format!("{e:#}"))
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
        let scanned = match db.0.lock() {
            Ok(mut conn) => scanner::scan_class(&mut conn, class_id),
            Err(e) => Err(anyhow::anyhow!("db lock poisoned: {e}")),
        };
        match scanned {
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
            let conn = db::open(&data_dir.join("classhub.db"))?;
            jobs::startup_recovery(&conn)?;
            app.manage(Db(Mutex::new(conn)));
            app.manage(jobs::JobManager::default());
            if let Err(e) = jobs::enqueue_self_check(app.handle()) {
                eprintln!("startup self-check failed to enqueue: {e:#}");
            }
            scan_and_extract_all(app.handle());
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
            list_jobs,
            cancel_job,
            get_job_events,
            get_auth_check,
            run_auth_check
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
