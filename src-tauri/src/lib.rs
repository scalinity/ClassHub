mod db;
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
fn scan_class(state: tauri::State<Db>, class_id: i64) -> Result<Vec<scanner::TreeNode>, String> {
    let mut conn = state.0.lock().map_err(|e| e.to_string())?;
    scanner::scan_class(&mut conn, class_id).map_err(|e| format!("{e:#}"))
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

#[tauri::command]
fn list_jobs(state: tauri::State<Db>) -> Result<Vec<jobs::JobInfo>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    jobs::list_jobs(&conn).map_err(|e| format!("{e:#}"))
}

/// M3 throwaway trigger; real job kinds land in M4+.
#[tauri::command]
fn run_test_job(app: tauri::AppHandle, class_id: i64) -> Result<i64, String> {
    jobs::enqueue_probe(&app, class_id).map_err(|e| format!("{e:#}"))
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

/// SPEC §7 step 1: scan on launch. Folders may legitimately be absent; skip those.
fn scan_all_classes(conn: &mut Connection) {
    let class_ids: Vec<i64> = conn
        .prepare("SELECT id FROM classes")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap_or_default();
    for class_id in class_ids {
        if let Err(e) = scanner::scan_class(conn, class_id) {
            eprintln!("launch scan skipped class {class_id}: {e:#}");
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
            let mut conn = db::open(&data_dir.join("classhub.db"))?;
            jobs::startup_recovery(&conn)?;
            scan_all_classes(&mut conn);
            app.manage(Db(Mutex::new(conn)));
            app.manage(jobs::JobManager::default());
            if let Err(e) = jobs::enqueue_self_check(app.handle()) {
                eprintln!("startup self-check failed to enqueue: {e:#}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_classes,
            scan_class,
            reveal_in_finder,
            open_in_default_app,
            list_jobs,
            run_test_job,
            cancel_job,
            get_job_events,
            get_auth_check,
            run_auth_check
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
