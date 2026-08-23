//! App-level settings in the SPEC §5 settings table: the AIBHS root and the
//! job runner's model / effort / concurrency. Chat's own settings (model,
//! effort, key state) live in chat.rs — the chat sidebar is their home.

use anyhow::{bail, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

use crate::db::{audit, set_setting, setting, with_conn};

/// Owner decision (M3 notes): jobs run Opus at extra-high effort unless
/// Settings says otherwise.
pub const DEFAULT_JOB_MODEL: &str = "opus";
pub const DEFAULT_JOB_EFFORT: &str = "xhigh";
pub const DEFAULT_JOB_CONCURRENCY: usize = 2;

const MODEL_SETTING: &str = "job_model";
const EFFORT_SETTING: &str = "job_effort";
const CONCURRENCY_SETTING: &str = "job_concurrency";

/// CLI aliases — the claude CLI resolves each to its current release, so a
/// model bump never needs a setting change.
const JOB_MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
/// The CLI's `--effort` ladder (M3 notes).
const JOB_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
pub const MAX_JOB_CONCURRENCY: usize = 4;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub aibhs_root: String,
    /// Whether the configured root exists on disk right now.
    pub aibhs_root_present: bool,
    pub job_model: String,
    pub job_effort: String,
    pub job_concurrency: usize,
}

pub fn get(app: &AppHandle) -> Result<AppSettings> {
    with_conn(app, |conn| {
        let root = crate::db::aibhs_root(conn)?;
        Ok(AppSettings {
            aibhs_root_present: root.is_dir(),
            aibhs_root: root.to_string_lossy().into_owned(),
            job_model: job_model(conn),
            job_effort: job_effort(conn),
            job_concurrency: concurrency(conn),
        })
    })
}

fn job_model(conn: &Connection) -> String {
    setting(conn, MODEL_SETTING)
        .ok()
        .flatten()
        .filter(|m| JOB_MODELS.contains(&m.as_str()))
        .unwrap_or_else(|| DEFAULT_JOB_MODEL.to_string())
}

fn job_effort(conn: &Connection) -> String {
    setting(conn, EFFORT_SETTING)
        .ok()
        .flatten()
        .filter(|e| JOB_EFFORTS.contains(&e.as_str()))
        .unwrap_or_else(|| DEFAULT_JOB_EFFORT.to_string())
}

fn concurrency(conn: &Connection) -> usize {
    setting(conn, CONCURRENCY_SETTING)
        .ok()
        .flatten()
        .and_then(|n| n.parse::<usize>().ok())
        .filter(|n| (1..=MAX_JOB_CONCURRENCY).contains(n))
        .unwrap_or(DEFAULT_JOB_CONCURRENCY)
}

/// What the job runner spawns with. Unreadable settings fall back to the
/// defaults — a job must never fail because a setting row is odd.
pub fn job_spawn_options(app: &AppHandle) -> (String, String) {
    with_conn(app, |conn| Ok((job_model(conn), job_effort(conn))))
        .unwrap_or_else(|_| (DEFAULT_JOB_MODEL.into(), DEFAULT_JOB_EFFORT.into()))
}

/// How many jobs the queue runs at once (master exclusivity is unaffected).
pub fn job_concurrency(app: &AppHandle) -> usize {
    with_conn(app, |conn| Ok(concurrency(conn))).unwrap_or(DEFAULT_JOB_CONCURRENCY)
}

pub fn set_job_model(app: &AppHandle, model: &str) -> Result<()> {
    if !JOB_MODELS.contains(&model) {
        bail!("model must be one of: {}", JOB_MODELS.join(", "));
    }
    with_conn(app, |conn| set_setting(conn, MODEL_SETTING, model))
}

pub fn set_job_effort(app: &AppHandle, effort: &str) -> Result<()> {
    if !JOB_EFFORTS.contains(&effort) {
        bail!("effort must be one of: {}", JOB_EFFORTS.join(", "));
    }
    with_conn(app, |conn| set_setting(conn, EFFORT_SETTING, effort))
}

pub fn set_job_concurrency(app: &AppHandle, count: usize) -> Result<()> {
    if !(1..=MAX_JOB_CONCURRENCY).contains(&count) {
        bail!("concurrency is between 1 and {MAX_JOB_CONCURRENCY}");
    }
    with_conn(app, |conn| {
        set_setting(conn, CONCURRENCY_SETTING, &count.to_string())
    })
}

/// Points the app at a different AIBHS tree. `~/` expands; the folder must
/// already exist — this setting selects a library, it never creates one. The
/// previous root rides the audit entry.
pub fn set_aibhs_root(app: &AppHandle, path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        bail!("enter the folder's path");
    }
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| path.into())
    } else {
        path.into()
    };
    if !expanded.is_absolute() {
        bail!("the path must be absolute (or start with ~/)");
    }
    if !expanded.is_dir() {
        bail!("no folder at {}", expanded.display());
    }
    with_conn(app, |conn| {
        let before = crate::db::aibhs_root(conn)?;
        set_setting(conn, "aibhs_root", &expanded.to_string_lossy())?;
        audit(
            conn,
            "ui.set_aibhs_root",
            json!({ "before": before.to_string_lossy(),
                    "after": expanded.to_string_lossy() }),
        )?;
        Ok(())
    })
}
