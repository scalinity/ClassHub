//! App-level settings in the SPEC §5 settings table: the AIBHS root, the job
//! runner's model / effort / concurrency — one global pair and a pair per job
//! kind (SPEC §6) — the two notifications and the login item (§12). Chat's
//! own settings (model, effort, key state) live in chat.rs — the chat sidebar
//! is their home — and the shift's in shift.rs.

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

/// Every kind a model and effort can be set for, with what it is called on
/// screen (SPEC §6). The self-check is not one: it spends nothing worth
/// tuning, and its verdict is about auth, not a model.
pub const JOB_KINDS: [(&str, &str); 7] = [
    ("extract", "Extract"),
    ("module_guide", "Study guide"),
    ("master_guide", "Semester master"),
    ("practice", "Practice exam"),
    ("lecture_digest", "Lecture digest"),
    ("sort_proposal", "Sort"),
    ("syllabus_scan", "Syllabus scan"),
];

/// The two notifications (SPEC §12), each a setting that defaults to on.
pub const NOTIFY_SHIFT_FINISHED: &str = "notify_shift_finished";
pub const NOTIFY_JOB_FAILED: &str = "notify_job_failed";
const NOTIFY_KEYS: [&str; 2] = [NOTIFY_SHIFT_FINISHED, NOTIFY_JOB_FAILED];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobKindSettings {
    pub kind: String,
    pub label: String,
    /// `None` is the default: the global pair.
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub aibhs_root: String,
    /// Whether the configured root exists on disk right now.
    pub aibhs_root_present: bool,
    pub job_model: String,
    pub job_effort: String,
    pub job_concurrency: usize,
    /// One row per kind: the pair it overrides the global one with, or none.
    pub job_kinds: Vec<JobKindSettings>,
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
    pub shift: crate::shift::ShiftSettings,
    pub notify_shift_finished: bool,
    pub notify_job_failed: bool,
    /// Whether the app is registered as a login item (SPEC §12).
    pub login_item: bool,
    /// A dev build: the shift's "run in this build" toggle only means
    /// something here (SPEC §6).
    pub dev_build: bool,
}

pub fn get(app: &AppHandle) -> Result<AppSettings> {
    let login_item = crate::login_item_enabled(app);
    with_conn(app, |conn| {
        let root = crate::db::aibhs_root(conn)?;
        let parakeet = parakeet_python(conn);
        Ok(AppSettings {
            aibhs_root_present: root.is_dir(),
            aibhs_root: root.to_string_lossy().into_owned(),
            job_model: job_model(conn),
            job_effort: job_effort(conn),
            job_concurrency: concurrency(conn),
            job_kinds: JOB_KINDS
                .iter()
                .map(|(kind, label)| JobKindSettings {
                    kind: kind.to_string(),
                    label: label.to_string(),
                    model: kind_override(conn, MODEL_SETTING, kind, &JOB_MODELS),
                    effort: kind_override(conn, EFFORT_SETTING, kind, &JOB_EFFORTS),
                })
                .collect(),
            parakeet_present: std::path::Path::new(&parakeet).is_file(),
            parakeet_python: parakeet,
            job_models: JOB_MODELS.iter().map(|m| m.to_string()).collect(),
            job_efforts: JOB_EFFORTS.iter().map(|e| e.to_string()).collect(),
            max_concurrency: MAX_JOB_CONCURRENCY,
            shift: crate::shift::settings(conn),
            notify_shift_finished: flag(conn, NOTIFY_SHIFT_FINISHED, true),
            notify_job_failed: flag(conn, NOTIFY_JOB_FAILED, true),
            login_item,
            dev_build: cfg!(debug_assertions),
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

/// A kind's own value for `job_model` or `job_effort`, when one is set and
/// accepted; an odd row is named on stderr and reads as unset, so the kind
/// falls back to the global pair rather than failing to spawn.
fn kind_override(conn: &Connection, base: &str, kind: &str, allowed: &[&str]) -> Option<String> {
    let key = kind_key(base, kind);
    match setting(conn, &key) {
        Ok(Some(v)) if allowed.contains(&v.as_str()) => Some(v),
        Ok(Some(v)) => {
            eprintln!("settings: {key} '{v}' is not an accepted value — using the default");
            None
        }
        Ok(None) => None,
        Err(e) => {
            eprintln!("settings: {key} unreadable ({e:#}) — using the default");
            None
        }
    }
}

/// `job_model.sort_proposal`, `job_effort.lecture_digest`.
fn kind_key(base: &str, kind: &str) -> String {
    format!("{base}.{kind}")
}

fn job_model(conn: &Connection) -> String {
    validated(conn, MODEL_SETTING, &JOB_MODELS, DEFAULT_JOB_MODEL)
}

fn job_effort(conn: &Connection) -> String {
    validated(conn, EFFORT_SETTING, &JOB_EFFORTS, DEFAULT_JOB_EFFORT)
}

/// A yes/no setting: `1` or `0`, the default where the row is absent or odd.
pub(crate) fn flag(conn: &Connection, key: &str, default: bool) -> bool {
    match setting(conn, key) {
        Ok(Some(v)) => match v.as_str() {
            "1" | "true" => true,
            "0" | "false" => false,
            _ => {
                eprintln!("settings: {key} '{v}' is not 1 or 0 — using {default}");
                default
            }
        },
        Ok(None) => default,
        Err(e) => {
            eprintln!("settings: {key} unreadable ({e:#}) — using {default}");
            default
        }
    }
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

/// What a job of `kind` spawns with (SPEC §6): the kind's own model and effort
/// where set, else the global pair — each half on its own, so a kind with a
/// model of its own and no effort takes the global effort.
pub(crate) fn spawn_options_in(conn: &Connection, kind: &str) -> (String, String) {
    (
        kind_override(conn, MODEL_SETTING, kind, &JOB_MODELS).unwrap_or_else(|| job_model(conn)),
        kind_override(conn, EFFORT_SETTING, kind, &JOB_EFFORTS).unwrap_or_else(|| job_effort(conn)),
    )
}

/// What the job runner spawns with; falls back (loudly) rather than failing.
pub fn job_spawn_options(app: &AppHandle, kind: &str) -> (String, String) {
    with_conn(app, |conn| Ok(spawn_options_in(conn, kind))).unwrap_or_else(|e| {
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
pub(crate) fn set_audited(app: &AppHandle, key: &str, value: &str) -> Result<()> {
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

/// Removes a setting row, its before value on the audit row; a no-op without
/// a row, since there is nothing to record.
fn clear_audited(app: &AppHandle, key: &str) -> Result<()> {
    with_conn(app, |conn| {
        let Some(before) = setting(conn, key)? else {
            return Ok(());
        };
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        audit(
            &tx,
            &format!("ui.set_{key}"),
            json!({ "before": before, "after": null }),
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

fn known_kind(kind: &str) -> Result<()> {
    if !JOB_KINDS.iter().any(|(k, _)| *k == kind) {
        bail!("no job kind called {kind}");
    }
    Ok(())
}

/// A kind's own model (SPEC §6); `None` returns it to the global pair.
pub fn set_job_kind_model(app: &AppHandle, kind: &str, model: Option<&str>) -> Result<()> {
    known_kind(kind)?;
    match model {
        Some(model) if !JOB_MODELS.contains(&model) => {
            bail!("model must be one of: {}", JOB_MODELS.join(", "))
        }
        Some(model) => set_audited(app, &kind_key(MODEL_SETTING, kind), model),
        None => clear_audited(app, &kind_key(MODEL_SETTING, kind)),
    }
}

/// A kind's own effort (SPEC §6); `None` returns it to the global pair.
pub fn set_job_kind_effort(app: &AppHandle, kind: &str, effort: Option<&str>) -> Result<()> {
    known_kind(kind)?;
    match effort {
        Some(effort) if !JOB_EFFORTS.contains(&effort) => {
            bail!("effort must be one of: {}", JOB_EFFORTS.join(", "))
        }
        Some(effort) => set_audited(app, &kind_key(EFFORT_SETTING, kind), effort),
        None => clear_audited(app, &kind_key(EFFORT_SETTING, kind)),
    }
}

/// One of the two notifications, on or off (SPEC §12).
pub fn set_notify(app: &AppHandle, key: &str, on: bool) -> Result<()> {
    if !NOTIFY_KEYS.contains(&key) {
        bail!("no notification setting called {key}");
    }
    set_audited(app, key, if on { "1" } else { "0" })
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

#[cfg(test)]
mod tests {
    use super::{flag, spawn_options_in};
    use crate::db::{memory_db, set_setting};

    /// A kind's own pair wins; an unset kind, or an unset half, takes the
    /// global pair; an odd row reads as unset. A sort quietly spawning as
    /// opus/xhigh, or a guide as the sort's sonnet, would differ on the
    /// subscription window with nothing to notice it by (SPEC §6).
    #[test]
    fn a_kinds_pair_falls_back_to_the_global_pair_half_by_half() {
        let conn = memory_db();
        assert_eq!(
            spawn_options_in(&conn, "sort_proposal"),
            ("opus".to_string(), "xhigh".to_string()),
            "nothing set: the shipped defaults"
        );
        set_setting(&conn, "job_model", "sonnet").unwrap();
        set_setting(&conn, "job_effort", "high").unwrap();
        set_setting(&conn, "job_model.sort_proposal", "haiku").unwrap();
        assert_eq!(
            spawn_options_in(&conn, "sort_proposal"),
            ("haiku".to_string(), "high".to_string()),
            "its own model, the global effort"
        );
        set_setting(&conn, "job_effort.sort_proposal", "low").unwrap();
        assert_eq!(
            spawn_options_in(&conn, "sort_proposal"),
            ("haiku".to_string(), "low".to_string())
        );
        assert_eq!(
            spawn_options_in(&conn, "module_guide"),
            ("sonnet".to_string(), "high".to_string()),
            "a kind with nothing of its own takes the global pair"
        );
        set_setting(&conn, "job_model.module_guide", "gpt").unwrap();
        assert_eq!(
            spawn_options_in(&conn, "module_guide").0,
            "sonnet",
            "an odd row reads as unset"
        );
    }

    #[test]
    fn a_flag_reads_one_and_zero_and_keeps_its_default_otherwise() {
        let conn = memory_db();
        assert!(flag(&conn, "notify_job_failed", true));
        set_setting(&conn, "notify_job_failed", "0").unwrap();
        assert!(!flag(&conn, "notify_job_failed", true));
        set_setting(&conn, "notify_job_failed", "maybe").unwrap();
        assert!(flag(&conn, "notify_job_failed", true));
    }
}
