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
/// Seeded by db.rs on first launch; this module only repoints it.
const ROOT_SETTING: &str = "aibhs_root";

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
    /// Interpreter the on-device transcriber runs through, and whether it is
    /// there right now — it lives inside another app's bundle, so "LocalFlow
    /// was updated or removed" needs to read as a settings problem rather than
    /// as a mysteriously broken feature.
    pub parakeet_python: String,
    pub parakeet_present: bool,
    /// What the setters accept — served so the UI renders exactly the values
    /// the backend will take, instead of keeping a second copy of the lists.
    pub job_models: Vec<String>,
    pub job_efforts: Vec<String>,
    pub max_concurrency: usize,
}

pub fn get(app: &AppHandle) -> Result<AppSettings> {
    with_conn(app, |conn| {
        let root = crate::db::aibhs_root(conn)?;
        let parakeet = parakeet_python(conn);
        Ok(AppSettings {
            aibhs_root_present: root.is_dir(),
            aibhs_root: root.to_string_lossy().into_owned(),
            job_model: job_model(conn),
            job_effort: job_effort(conn),
            job_concurrency: concurrency(conn),
            parakeet_present: std::path::Path::new(&parakeet).is_file(),
            parakeet_python: parakeet,
            job_models: JOB_MODELS.iter().map(|m| m.to_string()).collect(),
            job_efforts: JOB_EFFORTS.iter().map(|e| e.to_string()).collect(),
            max_concurrency: MAX_JOB_CONCURRENCY,
        })
    })
}

/// Reads a whitelisted setting. Anything unreadable or unknown falls back to
/// the default — a job must never fail on an odd settings row — but never
/// silently: the substitution lands on stderr, since a configured
/// sonnet/low quietly spawning as opus/xhigh would differ on the
/// subscription window with nothing to notice it by.
fn validated(conn: &Connection, key: &str, allowed: &[&str], default: &'static str) -> String {
    match setting(conn, key) {
        Ok(None) => default.to_string(),
        Ok(Some(v)) if allowed.contains(&v.as_str()) => v,
        Ok(Some(v)) => {
            eprintln!("settings: {key} '{v}' is not an accepted value — using {default}");
            default.to_string()
        }
        Err(e) => {
            eprintln!("settings: {key} unreadable ({e:#}) — using {default}");
            default.to_string()
        }
    }
}

fn job_model(conn: &Connection) -> String {
    validated(conn, MODEL_SETTING, &JOB_MODELS, DEFAULT_JOB_MODEL)
}

fn job_effort(conn: &Connection) -> String {
    validated(conn, EFFORT_SETTING, &JOB_EFFORTS, DEFAULT_JOB_EFFORT)
}

fn parakeet_python(conn: &Connection) -> String {
    crate::transcribe::interpreter_of(conn)
        .to_string_lossy()
        .into_owned()
}

fn concurrency(conn: &Connection) -> usize {
    match setting(conn, CONCURRENCY_SETTING) {
        Ok(None) => DEFAULT_JOB_CONCURRENCY,
        Ok(Some(v)) => match v
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=MAX_JOB_CONCURRENCY).contains(n))
        {
            Some(n) => n,
            None => {
                eprintln!(
                    "settings: job_concurrency '{v}' is not 1–{MAX_JOB_CONCURRENCY} — \
                     using {DEFAULT_JOB_CONCURRENCY}"
                );
                DEFAULT_JOB_CONCURRENCY
            }
        },
        Err(e) => {
            eprintln!(
                "settings: job_concurrency unreadable ({e:#}) — using {DEFAULT_JOB_CONCURRENCY}"
            );
            DEFAULT_JOB_CONCURRENCY
        }
    }
}

/// What the job runner spawns with; falls back (loudly) rather than failing.
pub fn job_spawn_options(app: &AppHandle) -> (String, String) {
    with_conn(app, |conn| Ok((job_model(conn), job_effort(conn)))).unwrap_or_else(|e| {
        eprintln!("settings: job spawn options unreadable ({e:#}) — using the defaults");
        (DEFAULT_JOB_MODEL.into(), DEFAULT_JOB_EFFORT.into())
    })
}

/// How many jobs the queue runs at once (master exclusivity is unaffected).
pub fn job_concurrency(app: &AppHandle) -> usize {
    with_conn(app, |conn| Ok(concurrency(conn))).unwrap_or_else(|e| {
        eprintln!(
            "settings: job_concurrency unreadable ({e:#}) — using {DEFAULT_JOB_CONCURRENCY}"
        );
        DEFAULT_JOB_CONCURRENCY
    })
}

/// One shape for every setting write: the value and its before/after audit
/// row commit together. These decide what a subscription-billed spawn runs
/// with, so "when did this change" is exactly what the history is for. The
/// action name derives from the key (`ui.set_job_model`, `ui.set_aibhs_root`).
fn set_audited(app: &AppHandle, key: &str, value: &str) -> Result<()> {
    with_conn(app, |conn| {
        let before = setting(conn, key)?;
        let tx = conn.unchecked_transaction()?;
        set_setting(&tx, key, value)?;
        audit(
            &tx,
            &format!("ui.set_{key}"),
            json!({ "before": before, "after": value }),
        )?;
        tx.commit()?;
        Ok(())
    })
}

pub fn set_job_model(app: &AppHandle, model: &str) -> Result<()> {
    if !JOB_MODELS.contains(&model) {
        bail!("model must be one of: {}", JOB_MODELS.join(", "));
    }
    set_audited(app, MODEL_SETTING, model)
}

pub fn set_job_effort(app: &AppHandle, effort: &str) -> Result<()> {
    if !JOB_EFFORTS.contains(&effort) {
        bail!("effort must be one of: {}", JOB_EFFORTS.join(", "));
    }
    set_audited(app, EFFORT_SETTING, effort)
}

pub fn set_job_concurrency(app: &AppHandle, count: usize) -> Result<()> {
    if !(1..=MAX_JOB_CONCURRENCY).contains(&count) {
        bail!("concurrency is between 1 and {MAX_JOB_CONCURRENCY}");
    }
    set_audited(app, CONCURRENCY_SETTING, &count.to_string())
}

/// Repoints the on-device transcriber at a different Python. Clearing the
/// field restores the bundled-LocalFlow default rather than leaving the
/// feature pointed at nothing.
pub fn set_parakeet_python(app: &AppHandle, path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        // Stored as the default rather than as empty. Both resolve the same
        // way, but the settings screen serves the *resolved* path, so storing
        // empty would leave the field showing the clear the user just made
        // with nothing to say it had taken effect.
        return set_audited(
            app,
            crate::transcribe::INTERPRETER_SETTING,
            crate::transcribe::DEFAULT_INTERPRETER,
        );
    }
    let expanded = expand_home(path);
    if !expanded.is_absolute() {
        bail!("the path must be absolute (or start with ~/)");
    }
    if !expanded.is_file() {
        bail!("no interpreter at {}", expanded.display());
    }
    set_audited(
        app,
        crate::transcribe::INTERPRETER_SETTING,
        &expanded.to_string_lossy(),
    )
}

fn expand_home(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| path.into()),
        None => path.into(),
    }
}

/// Points the app at a different AIBHS tree. `~/` expands; the folder must
/// already exist — this setting selects a library, it never creates one. The
/// previous root rides the audit entry.
pub fn set_aibhs_root(app: &AppHandle, path: &str) -> Result<()> {
    let path = path.trim();
    if path.is_empty() {
        bail!("enter the folder's path");
    }
    let expanded = expand_home(path);
    if !expanded.is_absolute() {
        bail!("the path must be absolute (or start with ~/)");
    }
    if !expanded.is_dir() {
        bail!("no folder at {}", expanded.display());
    }
    set_audited(app, ROOT_SETTING, &expanded.to_string_lossy())?;
    // The viewer's PDF frames read through the asset protocol, whose scope
    // followed the old root; the old one stays allowed until relaunch.
    crate::allow_asset_root(app, &expanded);
    Ok(())
}
