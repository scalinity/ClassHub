//! SPEC §8.1/§8.2 — study-guide synthesis (module and semester master): prompt
//! assembly, the guides-table upsert on job success, and on-demand staleness.
//!
//! Synthesis flows through the job runner (SPEC §6, the single gateway to the
//! subscription) and is only ever triggered manually (SPEC §7). The guides-row
//! upsert data rides `QueuedJob.payload`, and `finalize_job` runs before the
//! job row leaves `running` — the same ordering the extract pipeline uses — so
//! the UI can never observe a succeeded job without its guide row.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::{GUIDES_DIR, MASTER_SCOPE, PRACTICE_DIR, UNIT_SCOPE_PREFIX, lock, now};
use crate::extract::{current_manifest, manifest_is_stale, ManifestEntry};

/// SPEC §8.3: practice exams are dated files, several per scope — no staleness,
/// no `guides` row, the directory listing is the record.
/// SPEC §5/§8.2: the guides/jobs scope value for the semester master.
const MASTER_OUTPUT: &str = "Study Guides/Semester Master.html";
const PROMPT_TEMPLATE: &str = include_str!("../prompts/module_guide.md");
const MASTER_TEMPLATE: &str = include_str!("../prompts/master_guide.md");
const PRACTICE_TEMPLATE: &str = include_str!("../prompts/practice.md");

/// Continuation prompt for `--resume <session_id>` (SPEC §6): the session
/// already holds the full original instructions and everything read so far.
const RESUME_PROMPT: &str = "This session was interrupted before the task completed. \
Resume exactly where you left off and finish the original task: check what has \
already been written to the contracted output file, complete anything missing, and \
honor every requirement of the original instructions (content anatomy, design \
contract, hard constraints, output path). Write the file incrementally — NEVER in \
one large Write call, which hits the per-response output limit and is discarded. \
First Write the head plus the first section ending with the literal line \
`<!-- CONTINUE -->` before `</body></html>`, then extend via Edit calls that each \
replace `<!-- CONTINUE -->` with the next chunk (roughly 20-30 KB) plus the marker \
again; the final Edit removes the marker. When finished, reply with exactly one \
line: `DONE: <output path>` or `FAILED: <output path> — <reason>`.";

/// The class accent as concrete values for the guide's design contract,
/// mirroring the `--class-*` tokens in src/index.css (light, dark).
pub(crate) fn accent_values(color: &str) -> (&'static str, &'static str) {
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
    /// Session digests share this table to inherit the viewer and staleness,
    /// but they are per-lecture rather than per-module, so the Study Guides tab
    /// lists them separately instead of interleaving them with the guides.
    pub session: bool,
}

// ---------------------------------------------------------------------------
// Synthesis trigger (manual only, SPEC §7)

/// The steps all three synthesis triggers share: the class name and its accent
/// pair, the manifest captured at enqueue time, and the prompt blocks derived
/// from it. Each caller then owns only its own output path, template and
/// payload shape — which is the part that actually differs between them.
struct SynthesisContext {
    class_name: String,
    accent_light: &'static str,
    accent_dark: &'static str,
    manifest: Vec<crate::extract::ManifestEntry>,
    manifest_block: String,
    files_block: String,
    class_dir: PathBuf,
}

fn synthesis_context(
    conn: &Connection,
    class_id: i64,
    scope: &str,
    // Set for a unit scope: its contributing lectures are listed with their
    // corpus notes instead, so naming them again under the file listing would
    // invite the job to read every transcript in full — the read the corpus
    // note exists to have paid for once.
    unit_id: Option<i64>,
    empty_message: &str,
) -> Result<SynthesisContext> {
    let (class_name, color): (String, String) = conn.query_row(
        "SELECT display_name, color FROM classes WHERE id = ?1",
        [class_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    // SPEC §8.1: the manifest is captured at enqueue time (extract-pattern
    // semantics: a source edited mid-job leaves the guide stale afterwards).
    let manifest = current_manifest(conn, class_id, scope)?;
    if manifest.is_empty() {
        bail!("{empty_message}");
    }
    let manifest_block = manifest
        .iter()
        .map(|e| format!("- {}", e.rel_path))
        .collect::<Vec<_>>()
        .join("\n");
    let (accent_light, accent_dark) = accent_values(&color);
    let listed_apart = match unit_id {
        Some(unit_id) => crate::lectures::contributing_paths(conn, class_id, unit_id)?,
        None => BTreeSet::new(),
    };
    Ok(SynthesisContext {
        class_name,
        accent_light,
        accent_dark,
        files_block: files_block(conn, class_id, &manifest, &listed_apart)?,
        manifest,
        manifest_block,
        class_dir: crate::scanner::class_dir(conn, class_id)?,
    })
}

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

        if has_active_job(&conn, class_id, "module_guide", module_rel)? {
            bail!("a synthesis for this module is already queued or running");
        }
        let module_dir = crate::scanner::resolve_rel(&conn, class_id, module_rel)?;
        if !module_dir.is_dir() {
            bail!("not a module folder: {module_rel}");
        }

        let ctx = synthesis_context(
            &conn,
            class_id,
            module_rel,
            None,
            &format!("no indexed files in {module_rel} — rescan the class first"),
        )?;
        let prompt = render_guide_prompt(
            &ctx,
            &module_name,
            &output_rel,
            generated_at_label,
            // A folder is not one of the course's divisions, so nothing is
            // mapped to it — a lecture reaches a guide through its unit.
            "(none — this guide is scoped to a folder rather than to one of the \
             course's divisions, so no lecture is mapped to it)",
        );
        let payload = serde_json::to_string(&GuidePayload {
            scope: module_rel.to_string(),
            rel_path: output_rel.clone(),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        (ctx.class_dir, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    crate::jobs::enqueue_module_guide(app, class_id, module_rel, &prompt, payload)
}

/// SPEC §8.1: synthesis for one of the course's own divisions — a Module, a
/// Week or a Part, whatever that course calls it (SPEC §5).
///
/// Its sources are the three of SPEC §8.5: files under the unit's folder where
/// it has one, files Canvas attributed to it, and its corpus notes — which is
/// how a lecture stored under `Weeks/` reaches a guide scoped by topic. Today
/// no unit of these four courses has a folder, so the corpus notes are usually
/// all of it, and a division with no distilled lecture has nothing to build
/// from and says so rather than producing a guide out of nothing.
pub fn synthesize_unit(
    app: &AppHandle,
    class_id: i64,
    unit_id: i64,
    generated_at_label: &str,
) -> Result<i64> {
    let (class_dir, prompt, payload, scope) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);

        let unit_name: String = conn
            .query_row(
                "SELECT name FROM units WHERE id = ?1 AND class_id = ?2",
                params![unit_id, class_id],
                |row| row.get(0),
            )
            .optional()?
            .context("that division is no longer in this course's structure")?;
        let scope = unit_scope(&unit_name);
        if has_active_job(&conn, class_id, "module_guide", &scope)? {
            bail!("a synthesis for {unit_name} is already queued or running");
        }

        let corpus = crate::lectures::corpus_block(&conn, class_id, unit_id)?;
        let ctx = synthesis_context(
            &conn,
            class_id,
            &scope,
            Some(unit_id),
            &format!(
                "nothing to build {unit_name} from yet — it has no folder of its own and no \
                 lecture mapped to it has been distilled. Add a lecture for one of its weeks, \
                 or distil one already filed."
            ),
        )?;
        let output_rel = format!("{GUIDES_DIR}/{}.html", crate::units::folder_segment(&unit_name));
        let prompt =
            render_guide_prompt(&ctx, &unit_name, &output_rel, generated_at_label, &corpus);
        let payload = serde_json::to_string(&GuidePayload {
            scope: scope.clone(),
            rel_path: output_rel,
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        (ctx.class_dir, prompt, payload, scope)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    crate::jobs::enqueue_module_guide(app, class_id, &scope, &prompt, payload)
}

/// `guides.scope` and `jobs.scope` for one of the course's own divisions.
pub(crate) fn unit_scope(unit_name: &str) -> String {
    format!("{UNIT_SCOPE_PREFIX}{unit_name}")
}

/// What a scope is called when it is shown or told to someone.
///
/// A scope is a storage key, and two of its four shapes read as machinery: the
/// app never shows the word "unit" (SPEC §5), and a session names a file path
/// rather than a session. Mirrored by `scopeLabel` in src/lib/guides.ts.
pub fn scope_label(scope: &str) -> String {
    if scope == MASTER_SCOPE {
        return "Semester Master".into();
    }
    if let Some(name) = scope.strip_prefix(UNIT_SCOPE_PREFIX) {
        return name.to_string();
    }
    match scope.strip_prefix(crate::db::SESSION_SCOPE_PREFIX) {
        Some(path) => path
            .rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(".md")
            .to_string(),
        None => scope.to_string(),
    }
}

fn render_guide_prompt(
    ctx: &SynthesisContext,
    unit_name: &str,
    output_rel: &str,
    generated_at_label: &str,
    corpus_block: &str,
) -> String {
    PROMPT_TEMPLATE
        .replace("{class}", &ctx.class_name)
        .replace("{module}", unit_name)
        .replace("{output}", output_rel)
        .replace("{accent_light}", ctx.accent_light)
        .replace("{accent_dark}", ctx.accent_dark)
        .replace("{generated_at}", generated_at_label)
        .replace("{files}", &ctx.files_block)
        .replace("{corpus}", corpus_block)
        .replace("{manifest}", &ctx.manifest_block)
}

/// SPEC §8.2: semester master synthesis — full raw re-synthesis from every
/// module's extracts, never from module guides. Runs exclusively (jobs.rs).
pub fn synthesize_master(
    app: &AppHandle,
    class_id: i64,
    generated_at_label: &str,
) -> Result<i64> {
    let (class_dir, prompt, payload) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);

        if has_active_job(&conn, class_id, "master_guide", MASTER_SCOPE)? {
            bail!("a master synthesis for this class is already queued or running");
        }
        // Scope 'master' = every indexed file in the class (SPEC §7 step 5).
        let ctx = synthesis_context(
            &conn,
            class_id,
            MASTER_SCOPE,
            None,
            "no indexed files in this class — rescan first",
        )?;
        // Module roster = distinct depth-0 folders holding indexed files.
        let modules: BTreeSet<String> = ctx
            .manifest
            .iter()
            .filter_map(|e| {
                let (first, rest) = e.rel_path.split_once('/')?;
                (!rest.is_empty()).then(|| first.to_string())
            })
            .collect();
        let modules_block = modules
            .iter()
            .map(|m| format!("- {m}"))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = MASTER_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{output}", MASTER_OUTPUT)
            .replace("{modules}", &modules_block)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{files}", &ctx.files_block)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&GuidePayload {
            scope: MASTER_SCOPE.to_string(),
            rel_path: MASTER_OUTPUT.to_string(),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        (ctx.class_dir, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    crate::jobs::enqueue_master_guide(app, class_id, &prompt, payload, None)
}

/// Rides `QueuedJob.payload` for practice jobs: finalize only has to verify
/// the contracted file actually landed.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PracticePayload {
    rel_path: String,
}

/// SPEC §8.3: practice exam synthesis, triggered from chat (M8). Scope is a
/// module rel path or `master` (the whole semester); `focus` narrows topics.
/// `date_label` names the file (`<scope> — <date>.html`), so it must be
/// filename-safe (YYYY-MM-DD).
pub fn generate_practice(
    app: &AppHandle,
    class_id: i64,
    scope: &str,
    focus: Option<&str>,
    generated_at_label: &str,
    date_label: &str,
) -> Result<(i64, String)> {
    let scope_label = if scope == MASTER_SCOPE {
        "Semester".to_string()
    } else {
        Path::new(scope)
            .file_name()
            .context("invalid module path")?
            .to_string_lossy()
            .into_owned()
    };

    let (class_dir, output_rel, prompt, payload) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);

        if has_active_job(&conn, class_id, "practice", scope)? {
            bail!("a practice exam for this scope is already queued or running");
        }
        let ctx = synthesis_context(
            &conn,
            class_id,
            scope,
            None,
            "no indexed files in that scope — rescan the class first",
        )?;

        // Same-day exams for the same scope get a numeric suffix instead of
        // silently overwriting the earlier one.
        let class_dir = ctx.class_dir.clone();
        let base = format!("{PRACTICE_DIR}/{scope_label} — {date_label}");
        let mut output_rel = format!("{base}.html");
        let mut n = 2;
        while class_dir.join(&output_rel).exists() {
            output_rel = format!("{base} ({n}).html");
            n += 1;
        }

        let prompt = PRACTICE_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{scope_label}", &scope_label)
            .replace(
                "{focus}",
                focus.filter(|f| !f.trim().is_empty()).unwrap_or(
                    "none — cover the whole scope evenly, weighted toward what an exam would test",
                ),
            )
            .replace("{output}", &output_rel)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{files}", &ctx.files_block);
        let payload = serde_json::to_string(&PracticePayload {
            rel_path: output_rel.clone(),
        })?;
        (class_dir, output_rel, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(PRACTICE_DIR))?;
    let job_id = crate::jobs::enqueue_practice(app, class_id, scope, &prompt, payload)?;
    Ok((job_id, output_rel))
}

/// Practice completion check (job runner, before the row leaves `running`):
/// a "succeeded" job with no exam on disk is a failure.
pub fn finalize_practice(app: &AppHandle, class_id: i64, payload: &str) -> Result<()> {
    let payload: PracticePayload =
        serde_json::from_str(payload).context("parsing practice payload")?;
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let abs = crate::scanner::class_dir(&conn, class_id)?.join(&payload.rel_path);
    let written = fs::metadata(&abs)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if !written {
        bail!("no exam file written at {}", payload.rel_path);
    }
    Ok(())
}

/// Practice exams for the workspace listing — the directory is the truth.
pub fn list_practice(conn: &Connection, class_id: i64) -> Result<Vec<crate::notes::NoteFile>> {
    let dir = crate::scanner::class_dir(conn, class_id)?.join(PRACTICE_DIR);
    Ok(crate::notes::list_dir_files(&dir, PRACTICE_DIR)
        .into_iter()
        .filter(|f| f.name.to_lowercase().ends_with(".html"))
        .collect())
}

/// SPEC §6 resumability: re-invoke a failed master run with
/// `--resume <session_id>`. The new job reuses the failed row's persisted
/// payload, so the enqueue-time manifest semantics survive the retry (and an
/// app restart, since jobs.payload is a DB column).
pub fn resume_master(app: &AppHandle, job_id: i64) -> Result<i64> {
    let (class_id, session_id, payload) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);
        let row = conn
            .query_row(
                "SELECT kind, status, class_id, session_id, payload
                 FROM jobs WHERE id = ?1",
                [job_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?
            .context("job not found")?;
        let (kind, status, class_id, session_id, payload) = row;
        if kind != "master_guide" {
            bail!("only master guide jobs can be resumed");
        }
        if status != "failed" {
            bail!("only failed jobs can be resumed");
        }
        let class_id = class_id.context("job has no class")?;
        let session_id = session_id.context(
            "no claude session was recorded — the run failed before it started; start over instead",
        )?;
        let payload = payload.context("job has no recorded payload — start over instead")?;
        if has_active_job(&conn, class_id, "master_guide", MASTER_SCOPE)? {
            bail!("a master synthesis for this class is already queued or running");
        }
        (class_id, session_id, payload)
    };
    crate::jobs::enqueue_master_guide(app, class_id, RESUME_PROMPT, payload, Some(session_id))
}

/// SPEC §8.1/§8.2 inputs listing: extracts are primary; PDFs (and converted
/// PPTX PDFs) are offered for figure re-inspection; Daniel's classwork is
/// marked as learner work. The only layout signal for learner work is the
/// `Edited Files` folder convention from SPEC §4's example tree.
///
/// Driven by the manifest rather than by re-deriving the scope, so what the
/// prompt lists and what staleness is measured against are the same set by
/// construction. That matters most for a unit scope, whose sources come from
/// two places sharing no path prefix (SPEC §8.5).
fn files_block(
    conn: &Connection,
    class_id: i64,
    manifest: &[ManifestEntry],
    listed_apart: &BTreeSet<String>,
) -> Result<String> {
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

    let in_scope: BTreeSet<&str> = manifest.iter().map(|e| e.rel_path.as_str()).collect();
    let mut lines = Vec::new();
    for (rel_path, kind, extract) in rows {
        if !in_scope.contains(rel_path.as_str()) || listed_apart.contains(&rel_path) {
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

pub(crate) fn has_active_job(conn: &Connection, class_id: i64, kind: &str, scope: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = ?1 AND class_id = ?2 AND scope = ?3
           AND status IN ('queued', 'running')",
        params![kind, class_id, scope],
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
            session: crate::db::is_session_scope(&scope),
            scope,
            rel_path,
            generated_at,
        });
    }
    Ok(guides)
}

/// Dashboard card badge (SPEC §12): stale guide count for a class.
///
/// Study guides only. A session digest also lives in this table and also goes
/// stale, but the badge means "guides worth regenerating" and the Lectures
/// listing carries its own affordance for a stale session.
pub fn stale_guide_count(conn: &Connection, class_id: i64) -> Result<i64> {
    Ok(list_guides(conn, class_id)?
        .iter()
        .filter(|g| g.stale && !g.session)
        .count() as i64)
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
