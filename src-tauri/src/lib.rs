mod db;

use std::sync::Mutex;

use rusqlite::Connection;
use tauri::Manager;

struct Db(Mutex<Connection>);

#[tauri::command]
fn list_classes(state: tauri::State<Db>) -> Result<Vec<db::ClassCard>, String> {
    let conn = state.0.lock().map_err(|e| e.to_string())?;
    db::list_classes(&conn).map_err(|e| format!("{e:#}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let conn = db::open(&data_dir.join("classhub.db"))?;
            app.manage(Db(Mutex::new(conn)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![list_classes])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
