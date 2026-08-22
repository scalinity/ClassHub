//! SPEC §8.1 — module study-guide synthesis: prompt assembly, the guides-table
//! upsert on job success, and on-demand staleness for the M5 badges.
//!
//! Synthesis flows through the job runner (SPEC §6, the single gateway to the
//! subscription) and is only ever triggered manually (SPEC §7). The guides-row
//! upsert data rides `QueuedJob.payload`, and `finalize_job` runs before the
//! job row leaves `running` — the same ordering the extract pipeline uses — so
//! the UI can never observe a succeeded job without its guide row.

use std::fs;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::extract::{current_manifest, manifest_is_stale};

const GUIDES_DIR: &str = "Study Guides";
const PROMPT_TEMPLATE: &str = include_str!("../prompts/module_guide.md");

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The class accent as concrete values for the guide's design contract,
/// mirroring the `--class-*` tokens in src/index.css (light, dark).
fn accent_values(color: &str) -> (&'static str, &'static str) {
    match color {
        "orange" => ("oklch(0.646 0.175 45)", "oklch(0.748 0.145 50)"),
        "green" => ("oklch(0.578 0.135 158)", "oklch(0.732 0.13 158)"),
        "amber" => ("oklch(0.672 0.145 78)", "oklch(0.792 0.125 82)"),
        _ => ("oklch(0.548 0.185 262)", "oklch(0.702 0.145 262)"), // blue
    }
}

/// Rides `QueuedJob.payload`: everything `finalize_job` needs for the upsert.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GuidePayload {
    scope: String,
    rel_path: String,
    /// JSON `[{relPath, sha256}]` — stored verbatim as guides.source_manifest.
    source_manifest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideInfo {
    pub scope: String,
    pub rel_path: String,
    pub generated_at: i64,
    pub stale: bool,
}

// ---------------------------------------------------------------------------
// Synthesis trigger (manual only, SPEC §7)

/// Enqueues a `module_guide` job for one module. `generated_at_label` is the
/// display-only footer stamp (formatted by the frontend at enqueue time; the
/// guides row's `generated_at` is set at completion and is the DB truth).
pub fn synthesize_module(
    app: &AppHandle,
    class_id: i64,
    module_rel: &str,
    generated_at_label: &str,
) -> Result<i64> {
    let module_name = Path::new(module_rel)
        .file_name()
        .context("invalid module path")?
        .to_string_lossy()
        .into_owned();
    let output_rel = format!("{GUIDES_DIR}/{module_name}.html");

    let (class_dir, prompt, payload) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);

        if has_active_guide_job(&conn, class_id, module_rel)? {
            bail!("a synthesis for this module is already queued or running");
        }
        let module_dir = crate::scanner::resolve_rel(&conn, class_id, module_rel)?;
        if !module_dir.is_dir() {
            bail!("not a module folder: {module_rel}");
        }

        let (class_name, color): (String, String) = conn.query_row(
            "SELECT display_name, color FROM classes WHERE id = ?1",
            [class_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        // SPEC §8.1: the manifest is captured at enqueue time (extract-pattern
        // semantics: a source edited mid-job leaves the guide stale afterwards).
        let manifest = current_manifest(&conn, class_id, module_rel)?;
        if manifest.is_empty() {
            bail!("no indexed files in {module_rel} — rescan the class first");
        }
        let manifest_block = manifest
            .iter()
            .map(|e| format!("- {}", e.rel_path))
            .collect::<Vec<_>>()
            .join("\n");
        let (accent_light, accent_dark) = accent_values(&color);

        let prompt = PROMPT_TEMPLATE
            .replace("{class}", &class_name)
            .replace("{module}", &module_name)
            .replace("{output}", &output_rel)
            .replace("{accent_light}", accent_light)
            .replace("{accent_dark}", accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{files}", &files_block(&conn, class_id, module_rel)?)
            .replace("{manifest}", &manifest_block);
        let payload = serde_json::to_string(&GuidePayload {
            scope: module_rel.to_string(),
            rel_path: output_rel.clone(),
            source_manifest: serde_json::to_string(&manifest)?,
        })?;
        (crate::scanner::class_dir(&conn, class_id)?, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    crate::jobs::enqueue_module_guide(app, class_id, module_rel, &prompt, payload)
}

/// SPEC §8.1 inputs listing: extracts are primary; PDFs (and converted PPTX
/// PDFs) are offered for figure re-inspection; Daniel's classwork is marked as
/// learner work. The only layout signal for learner work is the `Edited Files`
/// folder convention from SPEC §4's example tree.
fn files_block(conn: &Connection, class_id: i64, scope: &str) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, kind, extract_rel_path FROM files
         WHERE class_id = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let prefix = format!("{scope}/");
    let mut lines = Vec::new();
    for (rel_path, kind, extract) in rows {
        if rel_path != scope && !rel_path.starts_with(&prefix) {
            continue;
        }
        let learner = Path::new(&rel_path)
            .components()
            .any(|c| c.as_os_str() == "Edited Files");
        let mut entry = format!("- source: {rel_path}");
        if learner {
            entry.push_str(" (learner work)");
        }
        match extract {
            Some(extract_rel) => {
                entry.push_str(&format!("\n  extract: {extract_rel}"));
                match kind.as_str() {
                    "pdf" => entry
                        .push_str(&format!("\n  original for figure re-inspection: {rel_path}")),
                    "pptx" => entry.push_str(&format!(
                        "\n  original for figure re-inspection: .classhub/extracts/{rel_path}.pdf"
                    )),
                    _ => {}
                }
            }
            None => entry.push_str(
                "\n  (no extract available — read the source directly if it is readable)",
            ),
        }
        lines.push(entry);
    }
    Ok(lines.join("\n"))
}

fn has_active_guide_job(conn: &Connection, class_id: i64, scope: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = 'module_guide' AND class_id = ?1 AND scope = ?2
           AND status IN ('queued', 'running')",
        params![class_id, scope],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

// ---------------------------------------------------------------------------
// Completion (called by the job runner before the row leaves 'running')

pub fn finalize_job(app: &AppHandle, class_id: i64, payload: &str) -> Result<()> {
    let payload: GuidePayload =
        serde_json::from_str(payload).context("parsing guide payload")?;
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let abs = crate::scanner::class_dir(&conn, class_id)?.join(&payload.rel_path);
    let written = fs::metadata(&abs)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if !written {
        bail!("no guide file written at {}", payload.rel_path);
    }
    conn.execute(
        "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(class_id, scope) DO UPDATE SET
           rel_path = excluded.rel_path,
           generated_at = excluded.generated_at,
           source_manifest = excluded.source_manifest",
        params![class_id, payload.scope, payload.rel_path, now(), payload.source_manifest],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads (staleness computed on demand, SPEC §7 step 5)

pub fn list_guides(conn: &Connection, class_id: i64) -> Result<Vec<GuideInfo>> {
    let mut stmt = conn.prepare(
        "SELECT scope, rel_path, generated_at, source_manifest FROM guides
         WHERE class_id = ?1 ORDER BY scope",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut guides = Vec::with_capacity(rows.len());
    for (scope, rel_path, generated_at, manifest_json) in rows {
        let current = current_manifest(conn, class_id, &scope)?;
        guides.push(GuideInfo {
            stale: manifest_is_stale(&manifest_json, &current),
            scope,
            rel_path,
            generated_at,
        });
    }
    Ok(guides)
}

/// Dashboard card badge (SPEC §12): stale guide count for a class.
pub fn stale_guide_count(conn: &Connection, class_id: i64) -> Result<i64> {
    Ok(list_guides(conn, class_id)?.iter().filter(|g| g.stale).count() as i64)
}

/// Guide HTML for the in-app sandboxed viewer.
pub fn read_guide(conn: &Connection, class_id: i64, scope: &str) -> Result<String> {
    let rel_path: Option<String> = conn
        .query_row(
            "SELECT rel_path FROM guides WHERE class_id = ?1 AND scope = ?2",
            params![class_id, scope],
            |row| row.get(0),
        )
        .optional()?;
    let rel_path = rel_path.context("no guide recorded for this scope")?;
    let abs = crate::scanner::resolve_rel(conn, class_id, &rel_path)?;
    fs::read_to_string(&abs).with_context(|| format!("reading {rel_path}"))
}
