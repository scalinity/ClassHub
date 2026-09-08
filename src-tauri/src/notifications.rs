//! SPEC §12 — the two notifications: `Shift finished` with the run's
//! summary, and `A job failed` naming the job. Each is a setting that
//! defaults to on, read at the moment of showing.

use tauri::AppHandle;

use crate::db::with_conn;
use crate::settings::flag;

/// Shows a notification when its setting (`key`) is on. The plugin hands the
/// show to the system and drops its result, so a refusal is not observable
/// here; a dev build's notification is attributed to Terminal by the plugin.
pub fn notify(app: &AppHandle, key: &str, title: &str, body: &str) {
    let on = with_conn(app, |conn| Ok(flag(conn, key, true))).unwrap_or(true);
    if !on {
        return;
    }
    use tauri_plugin_notification::NotificationExt;
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        eprintln!("notification not built ({title}): {e}");
    }
}
