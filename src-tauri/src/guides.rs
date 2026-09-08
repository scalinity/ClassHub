//! SPEC §8.1/§8.2/§8.3 — study-guide synthesis (division, folder, semester
//! master and practice exam): prompt assembly, the guides-table upsert on job
//! success, and on-demand staleness with the diff behind it.
//!
//! Synthesis flows through the job runner (SPEC §6, the single gateway to the
//! subscription) and is only ever triggered manually (SPEC §7). The guides-row
//! upsert data rides `QueuedJob.payload`, and `finalize_job` runs before the
//! job row leaves `running` — the same ordering the extract pipeline uses — so
//! the UI can never observe a succeeded job without its guide row. The row's
//! manifest is what the job was told about, widened by what its own log shows
//! it read (SPEC §7 step 5).

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::{
    GUIDES_DIR, MASTER_SCOPE, PRACTICE_DIR, PRACTICE_SCOPE_PREFIX, UNIT_SCOPE_PREFIX, lock, now,
    unit_scope, unit_scope_id,
};
use crate::extract::{
    current_hash, current_manifest, manifest_diff, union_manifest, ManifestDiff, ManifestEntry,
};

/// SPEC §5/§8.2: the guides/jobs scope value for the semester master.
const MASTER_OUTPUT: &str = "Study Guides/Semester Master.html";
/// SPEC §8.1: where a guide's cards sidecar goes — `<guide file stem>.json`
/// under here, one card per self-test question and glossary term. Nothing
/// reads them until M37; the finalizer requires the file so the shape holds.
pub const CARDS_DIR: &str = ".classhub/cards";
const PROMPT_TEMPLATE: &str = include_str!("../prompts/module_guide.md");
const MASTER_TEMPLATE: &str = include_str!("../prompts/master_guide.md");
const PRACTICE_TEMPLATE: &str = include_str!("../prompts/practice.md");

/// Continuation prompt for `--resume <session_id>` (SPEC §6): the session
/// already holds the full original instructions and everything read so far.
const RESUME_PROMPT: &str = "This session was interrupted before the task completed. \
Resume exactly where you left off and finish the original task: check what has \
already been written to the contracted output file, complete anything missing, and \
honor every requirement of the original instructions (content anatomy, design \
contract, hard constraints, output path, and the cards file the instructions name \
under `.classhub/cards/`). Write the file incrementally — NEVER in \
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
    /// JSON `[{relPath, sha256}]` captured at enqueue — the listed sources,
    /// widened at finalize by what the log shows the job read.
    source_manifest: String,
    /// The cards sidecar the job was told to write; a payload from before
    /// the sidecar existed carries none and is finalized without it.
    #[serde(default)]
    cards_rel_path: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GuideInfo {
    pub scope: String,
    /// What the scope is called on screen — a division's name for `unit:<id>`,
    /// since the key itself is a row id the reader never sees.
    pub label: String,
    pub rel_path: String,
    pub generated_at: i64,
    pub stale: bool,
    /// What changed since the guide was written (SPEC §7 step 5): the names
    /// behind `stale`, for the row's `Rewrite · 2 files added, 1 changed`.
    pub diff: ManifestDiff,
    /// Session digests share this table to inherit the viewer and staleness,
    /// but they are per-lecture rather than per-module, so the Study Guides tab
    /// lists them separately instead of interleaving them with the guides.
    pub session: bool,
    /// A practice exam's row (SPEC §8.3): listed with the exams, never among
    /// the guides, and never counted as one worth regenerating.
    pub practice: bool,
}

// ---------------------------------------------------------------------------
// Synthesis trigger (manual only, SPEC §7)

/// The steps every synthesis trigger shares: the class name and its accent
/// pair, the manifest captured at enqueue time, the prompt blocks derived
/// from it, and what changed since the guide on record. Each caller then owns
/// only its own output path, template and payload shape — which is the part
/// that actually differs between them.
struct SynthesisContext {
    class_name: String,
    accent_light: &'static str,
    accent_dark: &'static str,
    manifest: Vec<ManifestEntry>,
    manifest_block: String,
    files_block: String,
    changes_block: String,
    class_dir: PathBuf,
}

fn synthesis_context(
    conn: &Connection,
    class_id: i64,
    scope: &str,
    // The transcripts listed with their corpus notes instead: naming them
    // again under the file listing would invite the job to read every
    // transcript in full — the read the corpus note exists to have paid for
    // once. A division's contributing lectures, or every lecture for the
    // master; none for a folder.
    listed_apart: BTreeSet<String>,
    // Whether the prompt has a `{changes}` to fill: a guide's does, and the
    // block costs a row lookup and a full diff; an exam's does not, and an
    // exam scoped to a division would be told about the division guide's row
    // rather than its own.
    with_changes: bool,
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
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    Ok(SynthesisContext {
        class_name,
        accent_light,
        accent_dark,
        files_block: files_block(conn, class_id, &manifest, &listed_apart)?,
        changes_block: if with_changes {
            changes_block(conn, class_id, scope, &class_dir, &manifest)?
        } else {
            String::new()
        },
        manifest,
        manifest_block,
        class_dir,
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
            BTreeSet::new(),
            true,
            &format!("no indexed files in {module_rel} — rescan the class first"),
        )?;
        let cards_rel = cards_rel_path(&output_rel);
        let prompt = render_guide_prompt(
            &ctx,
            &GuideBlocks {
                name: &module_name,
                output_rel: &output_rel,
                generated_at_label,
                // A folder is not one of the course's divisions, so nothing is
                // mapped to it — a lecture reaches a guide through its unit.
                corpus: "(none — this guide is scoped to a folder rather than to one of the \
                         course's divisions, so no lecture is mapped to it)",
                hints: "(none — a folder is not one of the course's divisions, and what the \
                        professor flagged is kept per division)",
                objectives: "(none — a folder is not one of the course's divisions, and the \
                             syllabus states objectives per division)",
                cards_rel: &cards_rel,
            },
        );
        let payload = serde_json::to_string(&GuidePayload {
            scope: module_rel.to_string(),
            rel_path: output_rel.clone(),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
            cards_rel_path: Some(cards_rel),
        })?;
        (ctx.class_dir, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    fs::create_dir_all(class_dir.join(CARDS_DIR))?;
    crate::jobs::enqueue_module_guide(app, class_id, module_rel, &prompt, payload)
}

/// SPEC §8.1: synthesis for one of the course's own divisions — a Module, a
/// Week or a Part, whatever that course calls it (SPEC §5).
///
/// Its sources are those of SPEC §8.5: files under the unit's folder where it
/// has one and under the week folders its weeks name, files Canvas attributed
/// to it, and its corpus notes — which is how a lecture stored under `Weeks/`
/// reaches a guide scoped by topic, and how the deck filed beside it does. No
/// unit of these four courses has a folder, so the notes and the week folders
/// are all of it, and a division with neither has nothing to build from and
/// says so rather than producing a guide out of nothing.
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
        let scope = unit_scope(unit_id);
        if has_active_job(&conn, class_id, "module_guide", &scope)? {
            bail!("a synthesis for {unit_name} is already queued or running");
        }

        let (ctx, corpus) = unit_context(&conn, class_id, unit_id, &unit_name, &scope, true)?;
        let output_rel = unit_guide_rel_path(&unit_name);
        let cards_rel = cards_rel_path(&output_rel);
        let hints = hints_for(&conn, class_id, Some(&[unit_id]))?;
        let objectives = objectives_block(&unit_objectives(&conn, unit_id)?);
        let prompt = render_guide_prompt(
            &ctx,
            &GuideBlocks {
                name: &unit_name,
                output_rel: &output_rel,
                generated_at_label,
                corpus: &corpus,
                hints: &hints,
                objectives: &objectives,
                cards_rel: &cards_rel,
            },
        );
        let payload = serde_json::to_string(&GuidePayload {
            scope: scope.clone(),
            rel_path: output_rel,
            source_manifest: serde_json::to_string(&ctx.manifest)?,
            cards_rel_path: Some(cards_rel),
        })?;
        (ctx.class_dir, prompt, payload, scope)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    fs::create_dir_all(class_dir.join(CARDS_DIR))?;
    crate::jobs::enqueue_module_guide(app, class_id, &scope, &prompt, payload)
}

/// The sources of one of the course's own divisions (SPEC §8.5), for its guide
/// and its practice exam alike: the manifest over its folder and its mapped
/// lectures, plus the prompt block naming its distilled notes.
///
/// Refused when there is nothing a job could read. A division whose lectures
/// are filed but not yet distilled has a manifest — the transcripts count as
/// sources, so the guide goes stale when one changes — and yet nothing to
/// list: the transcripts are named through their notes rather than as files,
/// and there are no notes.
fn unit_context(
    conn: &Connection,
    class_id: i64,
    unit_id: i64,
    unit_name: &str,
    scope: &str,
    with_changes: bool,
) -> Result<(SynthesisContext, String)> {
    let nothing = format!(
        "nothing to build {unit_name} from yet — nothing is filed under its weeks, it has \
         no folder of its own, and no lecture mapped to it has been distilled. Add a \
         lecture or a file for one of its weeks, or distil a lecture already filed."
    );
    let notes = crate::lectures::corpus_notes(conn, class_id, unit_id)?;
    let listed_apart = crate::lectures::contributing_paths(conn, class_id, unit_id)?;
    let ctx = synthesis_context(conn, class_id, scope, listed_apart, with_changes, &nothing)?;
    if notes.is_empty() && ctx.files_block.is_empty() {
        bail!("{nothing}");
    }
    Ok((ctx, crate::lectures::corpus_block(&notes)))
}

/// Where a division's guide is written: `Study Guides/<Unit name>.html`
/// (SPEC §8.1). Named for the division, so a rename moves it (`units::upsert`).
pub(crate) fn unit_guide_rel_path(unit_name: &str) -> String {
    format!("{GUIDES_DIR}/{}.html", crate::units::folder_segment(unit_name))
}

/// Where a guide's cards sidecar is written: named for the guide file, under
/// `.classhub/cards/` (SPEC §8.1).
pub(crate) fn cards_rel_path(output_rel: &str) -> String {
    let stem = Path::new(output_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| output_rel.to_string());
    format!("{CARDS_DIR}/{}.json", stem.strip_suffix(".html").unwrap_or(&stem))
}

/// What a scope is called when it is shown or told to someone.
///
/// A scope is a storage key, and three of its shapes read as machinery: a
/// unit scope is a row id, and the app never shows the word "unit" (SPEC §5);
/// a session names a file path rather than a session; an exam names its
/// file. The division's name is the caller's to look up, since the key no
/// longer carries it; a unit scope from before ids, which no row answers to,
/// shows what it carries.
pub fn scope_label(scope: &str, unit_name: Option<&str>) -> String {
    if scope == MASTER_SCOPE {
        return "Semester Master".into();
    }
    if let Some(rest) = scope.strip_prefix(UNIT_SCOPE_PREFIX) {
        return unit_name.map(str::to_string).unwrap_or_else(|| rest.to_string());
    }
    let file_stem = |path: &str, ext: &str| {
        path.rsplit('/')
            .next()
            .unwrap_or(path)
            .trim_end_matches(ext)
            .to_string()
    };
    if let Some(path) = scope.strip_prefix(crate::db::SESSION_SCOPE_PREFIX) {
        return file_stem(path, ".md");
    }
    match scope.strip_prefix(PRACTICE_SCOPE_PREFIX) {
        Some(path) => file_stem(path, ".html"),
        None => scope.to_string(),
    }
}

/// The blocks a guide prompt takes beside its context.
struct GuideBlocks<'a> {
    name: &'a str,
    output_rel: &'a str,
    generated_at_label: &'a str,
    corpus: &'a str,
    hints: &'a str,
    objectives: &'a str,
    cards_rel: &'a str,
}

fn render_guide_prompt(ctx: &SynthesisContext, blocks: &GuideBlocks<'_>) -> String {
    PROMPT_TEMPLATE
        .replace("{class}", &ctx.class_name)
        .replace("{module}", blocks.name)
        .replace("{output}", blocks.output_rel)
        .replace("{cards}", blocks.cards_rel)
        .replace("{accent_light}", ctx.accent_light)
        .replace("{accent_dark}", ctx.accent_dark)
        .replace("{generated_at}", blocks.generated_at_label)
        .replace("{files}", &ctx.files_block)
        .replace("{corpus}", blocks.corpus)
        .replace("{hints}", blocks.hints)
        .replace("{objectives}", blocks.objectives)
        .replace("{changes}", &ctx.changes_block)
        .replace("{manifest}", &ctx.manifest_block)
}

/// The `{hints}` block for a scope (SPEC §8.1): a division's rows, or every
/// row of the class for the master and the semester exam.
fn hints_for(conn: &Connection, class_id: i64, units: Option<&[i64]>) -> Result<String> {
    Ok(crate::lectures::hints_block(&crate::lectures::list_hints(conn, class_id)?, units))
}

/// A division's stated objectives, off its row (SPEC §11).
fn unit_objectives(conn: &Connection, unit_id: i64) -> Result<Vec<String>> {
    let stored: Option<String> = conn
        .query_row("SELECT objectives FROM units WHERE id = ?1", [unit_id], |row| row.get(0))
        .optional()?
        .flatten();
    Ok(crate::units::objectives_of(stored.as_deref()))
}

/// The `{objectives}` block for one division: the syllabus's list, or the
/// line saying it states none.
fn objectives_block(objectives: &[String]) -> String {
    if objectives.is_empty() {
        return "(the syllabus states none for this division)".to_string();
    }
    objectives
        .iter()
        .map(|line| format!("- {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The master's `{modules}` roster (SPEC §8.2): the course's own divisions in
/// its own order, each with its start date and, for a Part, its week range —
/// never the folders the material happens to sit in.
pub(crate) fn roster_block(units: &[crate::units::UnitInfo]) -> String {
    if units.is_empty() {
        return "(the course declares no divisions yet — group the material by the folders it \
                sits in, and say so)"
            .to_string();
    }
    units
        .iter()
        .map(|unit| {
            let mut line = format!("- {}", unit.name);
            match (unit.first_week, unit.last_week) {
                (Some(first), Some(last)) => line.push_str(&format!(" (Weeks {first}–{last})")),
                _ => {}
            }
            if let Some(starts) = &unit.starts_on {
                line.push_str(&format!(" (from {starts})"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The master's `{objectives}`: each division's stated objectives under its
/// name, only for the divisions that state any.
fn master_objectives_block(conn: &Connection, units: &[crate::units::UnitInfo]) -> Result<String> {
    let mut out = String::new();
    for unit in units {
        let objectives = unit_objectives(conn, unit.id)?;
        if objectives.is_empty() {
            continue;
        }
        out.push_str(&format!("{}:\n{}\n", unit.name, objectives_block(&objectives)));
    }
    if out.is_empty() {
        return Ok("(the syllabus states none per division)".to_string());
    }
    Ok(out.trim_end().to_string())
}

/// The `{changes}` block (SPEC §8.1): for a rewrite, the date of the guide on
/// record and every source added, changed or removed since, which the prompt
/// marks with the `New since` chip; for a first write, the line saying no
/// chip applies. The earlier guide itself is never an input.
fn changes_block(
    conn: &Connection,
    class_id: i64,
    scope: &str,
    class_dir: &Path,
    current: &[ManifestEntry],
) -> Result<String> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT generated_at, source_manifest FROM guides WHERE class_id = ?1 AND scope = ?2",
            params![class_id, scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((generated_at, stored)) = row else {
        return Ok("This is the first guide for this scope: no earlier guide exists, so no \
                   entry is new and the `New since` chip is not used anywhere."
            .to_string());
    };
    let date = short_date(generated_at);
    let diff = manifest_diff(&stored, current, |path| {
        current_hash(conn, class_id, class_dir, path).ok().flatten()
    });
    if !diff.is_stale() {
        return Ok(format!(
            "This is a rewrite of the guide written on {date}. Its sources have not changed \
             since, so no entry is new: use no `New since` chip. The earlier guide is not an \
             input — do not read it; rebuild every entry from the sources."
        ));
    }
    let mut out = format!(
        "This is a rewrite of the guide written on {date}, and these sources changed since. \
         Mark every entry that draws on them with the `New since {date}` chip (design \
         contract), and cite nothing under a removed path:\n"
    );
    for (list, note) in [
        (&diff.added, "added"),
        (&diff.changed, "changed"),
        (&diff.removed, "removed — no longer a source"),
    ] {
        for path in list {
            out.push_str(&format!("- {path} ({note})\n"));
        }
    }
    out.push_str(
        "\nThe earlier guide is not an input: do not read it, and rebuild every entry from the \
         sources rather than carrying anything forward.",
    );
    Ok(out)
}

/// `Sep 3` for a unix stamp, in the machine's zone — the words the chip and
/// the row use.
pub(crate) fn short_date(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%b %-d").to_string())
        .unwrap_or_default()
}

/// SPEC §8.2: semester master synthesis — full re-synthesis from every extract
/// and every corpus note, transcripts read through their notes, never from
/// module guides. Runs exclusively (jobs.rs).
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
        // Every distilled lecture is listed through its note (SPEC §8.5), the
        // way a division's are, so the file listing leaves those transcripts
        // out; one not yet distilled has no note to stand in for it and stays
        // listed as the source material it is.
        let contributions = crate::lectures::list_contributions(&conn, class_id)?;
        let listed_apart: BTreeSet<String> = contributions
            .iter()
            .filter(|c| c.distilled)
            .map(|c| c.rel_path.clone())
            .collect();
        // Scope 'master' = every indexed file in the class (SPEC §7 step 5).
        let ctx = synthesis_context(
            &conn,
            class_id,
            MASTER_SCOPE,
            listed_apart,
            true,
            "no indexed files in this class — rescan first",
        )?;
        let units = crate::units::list_units(&conn, class_id)?;
        let notes: Vec<(String, String)> = contributions
            .iter()
            .filter(|c| c.distilled)
            .map(|c| (c.corpus_rel_path.clone(), c.rel_path.clone()))
            .collect();
        let cards_rel = cards_rel_path(MASTER_OUTPUT);
        let prompt = MASTER_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{output}", MASTER_OUTPUT)
            .replace("{cards}", &cards_rel)
            .replace("{modules}", &roster_block(&units))
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{files}", &ctx.files_block)
            .replace("{corpus}", &crate::lectures::corpus_block(&notes))
            .replace("{hints}", &hints_for(&conn, class_id, None)?)
            .replace("{objectives}", &master_objectives_block(&conn, &units)?)
            .replace("{changes}", &ctx.changes_block)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&GuidePayload {
            scope: MASTER_SCOPE.to_string(),
            rel_path: MASTER_OUTPUT.to_string(),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
            cards_rel_path: Some(cards_rel),
        })?;
        (ctx.class_dir, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(GUIDES_DIR))?;
    fs::create_dir_all(class_dir.join(CARDS_DIR))?;
    crate::jobs::enqueue_master_guide(app, class_id, &prompt, payload, None)
}

/// Rides `QueuedJob.payload` for practice jobs: what finalize verifies and
/// records. A payload from before exams had rows carries the path alone, and
/// is finalized as it always was — the file checked, no row.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PracticePayload {
    rel_path: String,
    #[serde(default)]
    source_manifest: Option<String>,
}

/// SPEC §8.3: practice exam synthesis, from chat (M8) or the workspace's
/// Practice exam action (M19). Scope is a folder rel path, `unit:<id>` for
/// one of the course's own divisions — drawing on the same sources as that
/// division's guide (SPEC §8.5) — or `master` (the whole semester); `focus`
/// narrows topics. `date_label` names the file (`<scope> — <date>.html`), so
/// it must be filename-safe (YYYY-MM-DD), and it is the day the rubric's next
/// assessment is measured from.
pub fn generate_practice(
    app: &AppHandle,
    class_id: i64,
    scope: &str,
    focus: Option<&str>,
    generated_at_label: &str,
    date_label: &str,
) -> Result<(i64, String)> {
    let (class_dir, output_rel, prompt, payload) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);

        if has_active_job(&conn, class_id, "practice", scope)? {
            bail!("a practice exam for this scope is already queued or running");
        }
        let (scope_label, (ctx, corpus), hints) = match unit_scope_id(scope) {
            Some(unit_id) => {
                let unit_name: String = conn
                    .query_row(
                        "SELECT name FROM units WHERE id = ?1 AND class_id = ?2",
                        params![unit_id, class_id],
                        |row| row.get(0),
                    )
                    .optional()?
                    .context("that division is no longer in this course's structure")?;
                let sources = unit_context(&conn, class_id, unit_id, &unit_name, scope, false)?;
                let hints = hints_for(&conn, class_id, Some(&[unit_id]))?;
                (unit_name, sources, hints)
            }
            None => {
                let label = if scope == MASTER_SCOPE {
                    "Semester".to_string()
                } else {
                    Path::new(scope)
                        .file_name()
                        .context("invalid module path")?
                        .to_string_lossy()
                        .into_owned()
                };
                // The semester scope lists its transcripts through their
                // notes, as the master does; a folder is not one of the
                // course's divisions, so no lecture is mapped to it.
                let (listed_apart, corpus, hints) = if scope == MASTER_SCOPE {
                    let contributions = crate::lectures::list_contributions(&conn, class_id)?;
                    let notes: Vec<(String, String)> = contributions
                        .iter()
                        .filter(|c| c.distilled)
                        .map(|c| (c.corpus_rel_path.clone(), c.rel_path.clone()))
                        .collect();
                    (
                        contributions
                            .iter()
                            .filter(|c| c.distilled)
                            .map(|c| c.rel_path.clone())
                            .collect(),
                        crate::lectures::corpus_block(&notes),
                        hints_for(&conn, class_id, None)?,
                    )
                } else {
                    (
                        BTreeSet::new(),
                        "(none — only an exam scoped to one of the course's divisions or to the \
                         semester draws on distilled lectures; the files above are the whole of \
                         this scope)"
                            .to_string(),
                        "(none — a folder is not one of the course's divisions, and what the \
                         professor flagged is kept per division)"
                            .to_string(),
                    )
                };
                let ctx = synthesis_context(
                    &conn,
                    class_id,
                    scope,
                    listed_apart,
                    false,
                    "no indexed files in that scope — rescan the class first",
                )?;
                (label, (ctx, corpus), hints)
            }
        };

        // Same-day exams for the same label get a numeric suffix instead of
        // silently overwriting the earlier one. A division's name can carry a
        // slash or a colon, which a file name cannot.
        let class_dir = ctx.class_dir.clone();
        let base = format!(
            "{PRACTICE_DIR}/{} — {date_label}",
            crate::units::folder_segment(&scope_label)
        );
        let output_rel = practice_output_rel(&class_dir, &base, &claimed_practice_paths(&conn, class_id)?);

        let prompt = PRACTICE_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{scope_label}", &scope_label)
            .replace(
                "{focus}",
                focus.map(str::trim).filter(|f| !f.is_empty()).unwrap_or(
                    "none — cover the whole scope evenly, weighted toward what an exam would test",
                ),
            )
            .replace("{output}", &output_rel)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{files}", &ctx.files_block)
            .replace("{corpus}", &corpus)
            .replace("{hints}", &hints)
            .replace("{assessment}", &assessment_block(&conn, class_id, date_label)?);
        let payload = serde_json::to_string(&PracticePayload {
            rel_path: output_rel.clone(),
            source_manifest: Some(serde_json::to_string(&ctx.manifest)?),
        })?;
        (class_dir, output_rel, prompt, payload)
    };

    fs::create_dir_all(class_dir.join(PRACTICE_DIR))?;
    let job_id = crate::jobs::enqueue_practice(app, class_id, scope, &prompt, payload)?;
    Ok((job_id, output_rel))
}

/// The `{assessment}` block of the exam prompt (SPEC §8.3): the class's
/// categories with their weights, the next open quiz or exam on the calendar
/// from `today`, and the kinds of assessment the calendar holds — what the
/// rubric is matched to instead of a guess at the volume.
pub(crate) fn assessment_block(conn: &Connection, class_id: i64, today: &str) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT name, weight FROM grade_categories WHERE class_id = ?1
         ORDER BY weight DESC, name",
    )?;
    let weights = stmt
        .query_map([class_id], |row| {
            let name: String = row.get(0)?;
            let weight: f64 = row.get(1)?;
            Ok(if weight > 0.0 {
                format!("{name} {}%", trim_percent(weight))
            } else {
                format!("{name} (no weight stated)")
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let next: Option<(String, String, String)> = conn
        .query_row(
            "SELECT title, kind, due_at FROM deadlines
             WHERE class_id = ?1 AND status = 'open' AND kind IN ('quiz', 'exam')
               AND substr(due_at, 1, 10) >= ?2
             ORDER BY due_at LIMIT 1",
            params![class_id, today],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let mut kinds_stmt = conn.prepare(
        "SELECT DISTINCT kind FROM deadlines WHERE class_id = ?1 ORDER BY kind",
    )?;
    let kinds = kinds_stmt
        .query_map([class_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut lines = Vec::new();
    if !weights.is_empty() {
        lines.push(format!("- Grade weights: {}", weights.join(" · ")));
    }
    match next {
        Some((title, kind, due)) => lines.push(format!(
            "- Next assessment on the calendar: {title} ({kind}) on {}",
            due.chars().take(10).collect::<String>()
        )),
        None => lines.push("- No quiz or exam is dated on the calendar yet".to_string()),
    }
    if !kinds.is_empty() {
        lines.push(format!("- Kinds of assessment the calendar holds: {}", kinds.join(", ")));
    }
    if weights.is_empty() && kinds.is_empty() {
        return Ok("(no weights or dated assessments are recorded for this class — weight the \
                   rubric toward what the material itself emphasizes, and say so in the cover)"
            .to_string());
    }
    Ok(lines.join("\n"))
}

/// `50` for 50.0, `12.5` for 12.5.
fn trim_percent(weight: f64) -> String {
    if (weight - weight.round()).abs() < f64::EPSILON {
        format!("{}", weight.round() as i64)
    } else {
        format!("{weight}")
    }
}

/// The first free name under `base`: not on disk, and not claimed by an exam
/// still being written. The second check matters because a scope's label is
/// not its scope — the folder `Module 1` and a division called `Module 1`
/// both label their exams `Module 1`, and the active-job guard keys on scope,
/// so two such jobs can run at once and the loser would overwrite the winner.
fn practice_output_rel(class_dir: &Path, base: &str, claimed: &BTreeSet<String>) -> String {
    let mut output_rel = format!("{base}.html");
    let mut n = 2;
    while class_dir.join(&output_rel).exists() || claimed.contains(&output_rel) {
        output_rel = format!("{base} ({n}).html");
        n += 1;
    }
    output_rel
}

/// The output paths of this class's queued and running practice jobs.
fn claimed_practice_paths(conn: &Connection, class_id: i64) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT payload FROM jobs
         WHERE kind = 'practice' AND class_id = ?1 AND status IN ('queued', 'running')",
    )?;
    let paths = stmt
        .query_map([class_id], |row| row.get::<_, Option<String>>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .filter_map(|payload| serde_json::from_str::<PracticePayload>(&payload).ok())
        .map(|payload| payload.rel_path)
        .collect();
    Ok(paths)
}

/// Practice completion (job runner, before the row leaves `running`): a
/// "succeeded" job with no exam on disk is a failure, and an exam that landed
/// gets its row — scoped by its file, its manifest the listed sources widened
/// by what the log shows the run read (SPEC §8.3).
pub fn finalize_practice(
    app: &AppHandle,
    class_id: i64,
    payload: &str,
    read: &BTreeSet<String>,
) -> Result<()> {
    let payload: PracticePayload =
        serde_json::from_str(payload).context("parsing practice payload")?;
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let class_dir = crate::scanner::class_dir(&conn, class_id)?;
    let abs = class_dir.join(&payload.rel_path);
    let written = fs::metadata(&abs)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if !written {
        bail!("no exam file written at {}", payload.rel_path);
    }
    let Some(listed) = payload.source_manifest else {
        return Ok(());
    };
    let listed = serde_json::from_str::<Vec<ManifestEntry>>(&listed).unwrap_or_default();
    let manifest = union_manifest(&conn, class_id, &class_dir, listed, read)?;
    upsert_guide(
        &conn,
        class_id,
        &format!("{PRACTICE_SCOPE_PREFIX}{}", payload.rel_path),
        &payload.rel_path,
        &serde_json::to_string(&manifest)?,
    )
}

/// One practice exam as the workspace lists it: the file, and its row's
/// freshness where one exists — exams written before rows carry none.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PracticeInfo {
    pub name: String,
    pub rel_path: String,
    pub modified_at: i64,
    pub scope: Option<String>,
    pub stale: Option<bool>,
    pub diff: Option<ManifestDiff>,
}

/// Practice exams for the workspace listing — the directory is the truth for
/// what exists, and the guides table for what each was built from.
pub fn list_practice(conn: &Connection, class_id: i64) -> Result<Vec<PracticeInfo>> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let rows: std::collections::HashMap<String, GuideInfo> =
        guides_where(conn, class_id, crate::db::is_practice_scope)?
            .into_iter()
            .map(|g| (g.rel_path.clone(), g))
            .collect();
    Ok(crate::notes::list_dir_files(&class_dir.join(PRACTICE_DIR), PRACTICE_DIR)
        .into_iter()
        .filter(|f| f.name.to_lowercase().ends_with(".html"))
        .map(|f| {
            let row = rows.get(&f.rel_path);
            PracticeInfo {
                scope: row.map(|g| g.scope.clone()),
                stale: row.map(|g| g.stale),
                diff: row.map(|g| g.diff.clone()),
                name: f.name,
                rel_path: f.rel_path,
                modified_at: f.modified_at,
            }
        })
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

/// One card of a cards sidecar, as a digest or a guide writes it.
#[derive(Deserialize)]
struct RawCard {
    front: String,
    back: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    topic: Option<String>,
}

/// Checks a cards file's shape — an array of `{front, back, source, topic}`,
/// both sides non-empty; an empty array is valid — and returns the count.
/// Nothing reads the cards until M37, so the check is the whole contract.
pub(crate) fn parse_cards(json: &str) -> Result<usize> {
    let items: Vec<serde_json::Value> =
        serde_json::from_str(json).context("the cards file is not a JSON array")?;
    for (index, item) in items.iter().enumerate() {
        let card: RawCard = serde_json::from_value(item.clone())
            .with_context(|| format!("card {} is not {{front, back, source, topic}}", index + 1))?;
        if card.front.trim().is_empty() || card.back.trim().is_empty() {
            bail!("card {} has an empty side", index + 1);
        }
        let _ = (card.source, card.topic);
    }
    Ok(items.len())
}

pub fn finalize_job(
    app: &AppHandle,
    class_id: i64,
    payload: &str,
    read: &BTreeSet<String>,
) -> Result<()> {
    let payload: GuidePayload =
        serde_json::from_str(payload).context("parsing guide payload")?;
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let class_dir = crate::scanner::class_dir(&conn, class_id)?;
    let abs = class_dir.join(&payload.rel_path);
    let written = fs::metadata(&abs)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false);
    if !written {
        bail!("no guide file written at {}", payload.rel_path);
    }
    // The guide on disk is this run's whatever the cards did, so the row is
    // written first: left unwritten, it would describe the earlier run under
    // a file the new one replaced.
    let listed = serde_json::from_str::<Vec<ManifestEntry>>(&payload.source_manifest)
        .unwrap_or_default();
    let manifest = union_manifest(&conn, class_id, &class_dir, listed, read)?;
    upsert_guide(
        &conn,
        class_id,
        &payload.scope,
        &payload.rel_path,
        &serde_json::to_string(&manifest)?,
    )?;
    // The cards sidecar is required on the same terms as the guide (SPEC
    // §8.1): the file is the contract, and a run that skipped it did not
    // finish — the job is demoted, with the row already telling the truth.
    if let Some(cards_rel) = &payload.cards_rel_path {
        let json = fs::read_to_string(class_dir.join(cards_rel))
            .with_context(|| format!("no cards file written at {cards_rel}"))?;
        parse_cards(&json).with_context(|| format!("the cards file at {cards_rel} is malformed"))?;
    }
    Ok(())
}

fn upsert_guide(
    conn: &Connection,
    class_id: i64,
    scope: &str,
    rel_path: &str,
    manifest_json: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(class_id, scope) DO UPDATE SET
           rel_path = excluded.rel_path,
           generated_at = excluded.generated_at,
           source_manifest = excluded.source_manifest",
        params![class_id, scope, rel_path, now(), manifest_json],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Reads (staleness computed on demand, SPEC §7 step 5)

pub fn list_guides(conn: &Connection, class_id: i64) -> Result<Vec<GuideInfo>> {
    guides_where(conn, class_id, |_| true)
}

/// The rows `keep` admits by scope, each with its diff. The diff is the
/// expensive part — a scope's current set, then a probe or a disk hash per
/// widened entry — so a caller that wants only the guides, or only the exams,
/// says so before it is computed rather than discarding it after.
fn guides_where(
    conn: &Connection,
    class_id: i64,
    keep: impl Fn(&str) -> bool,
) -> Result<Vec<GuideInfo>> {
    // The division's name rides along for the label: a unit scope is its id.
    let mut stmt = conn.prepare(
        "SELECT g.scope, g.rel_path, g.generated_at, g.source_manifest, u.name
         FROM guides g
         LEFT JOIN units u ON u.class_id = g.class_id AND g.scope = 'unit:' || u.id
         WHERE g.class_id = ?1 ORDER BY g.scope",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Resolved once for the resolver below; a root that cannot be resolved
    // leaves every widened entry unresolved, which reads as gone.
    let class_dir = crate::scanner::class_dir(conn, class_id).ok();
    // One note read by three guides is hashed once per listing, not thrice.
    let hashes: std::cell::RefCell<std::collections::HashMap<String, Option<String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());

    let mut guides = Vec::with_capacity(rows.len());
    for (scope, rel_path, generated_at, manifest_json, unit_name) in rows {
        if !keep(&scope) {
            continue;
        }
        let current = current_manifest(conn, class_id, &scope)?;
        // An entry outside the scope's own set — a note, a page mirror, a
        // file the job found through --add-dir — is resolved to what it is
        // now, so a widened manifest reads honestly rather than stale forever.
        let diff = manifest_diff(&manifest_json, &current, |path| {
            if let Some(known) = hashes.borrow().get(path) {
                return known.clone();
            }
            let hash = class_dir
                .as_deref()
                .and_then(|dir| current_hash(conn, class_id, dir, path).ok().flatten());
            hashes.borrow_mut().insert(path.to_string(), hash.clone());
            hash
        });
        guides.push(GuideInfo {
            stale: diff.is_stale(),
            diff,
            session: crate::db::is_session_scope(&scope),
            practice: crate::db::is_practice_scope(&scope),
            label: scope_label(&scope, unit_name.as_deref()),
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
/// listing carries its own affordance for a stale session; an exam is not
/// regenerated either.
pub fn stale_guide_count(conn: &Connection, class_id: i64) -> Result<i64> {
    Ok(guides_where(conn, class_id, |scope| {
        !crate::db::is_session_scope(scope) && !crate::db::is_practice_scope(scope)
    })?
    .iter()
    .filter(|g| g.stale)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A same-day exam takes the next free suffix, whether the earlier one is
    /// already on disk or still being written by a queued job.
    #[test]
    fn a_practice_exam_never_takes_a_name_on_disk_or_in_the_queue() {
        let conn = crate::db::memory_db();
        let dir = std::env::temp_dir().join(format!("classhub-practice-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(PRACTICE_DIR)).expect("dir");
        let base = format!("{PRACTICE_DIR}/Module 1 \u{2014} 2026-09-02");

        let claimed = claimed_practice_paths(&conn, 3).expect("none queued");
        assert!(claimed.is_empty());
        assert_eq!(practice_output_rel(&dir, &base, &claimed), format!("{base}.html"));

        fs::write(dir.join(format!("{base}.html")), "earlier today").expect("write");
        assert_eq!(practice_output_rel(&dir, &base, &claimed), format!("{base} (2).html"));

        let payload = serde_json::to_string(&PracticePayload { rel_path: format!("{base} (2).html"), source_manifest: None })
            .expect("payload");
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, payload, created_at, owner_pid)
             VALUES ('practice', 3, 'unit:1', 'running', ?1, 1, 1)",
            [payload],
        )
        .expect("job");
        let claimed = claimed_practice_paths(&conn, 3).expect("claimed");
        assert_eq!(practice_output_rel(&dir, &base, &claimed), format!("{base} (3).html"));
        let _ = fs::remove_dir_all(&dir);
    }

    /// The refusal the unit guide and the unit exam share (SPEC §8.5): a
    /// division builds from its folder, from what its week folders hold, or
    /// from its distilled notes. One whose lectures are filed but not
    /// distilled has a manifest — the transcripts are what staleness watches —
    /// and nothing a job could read; one with none of the three has nothing at
    /// all. Both are refused before anything is made.
    #[test]
    fn a_division_builds_from_its_folder_its_weeks_or_its_notes_and_from_nothing_else() {
        let conn = crate::db::memory_db();
        let root = std::env::temp_dir().join(format!("classhub-unit-context-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let class_dir = root.join("Biostatistics for AI");
        fs::create_dir_all(&class_dir).expect("class dir");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let name = "Week 2 \u{2014} Study Designs";
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, source)
             VALUES (3, 2, 'week', ?1, 'syllabus')",
            [name],
        )
        .expect("unit");
        let unit_id: i64 = conn
            .query_row("SELECT id FROM units WHERE class_id = 3", [], |row| row.get(0))
            .expect("id");
        let scope = unit_scope(unit_id);
        let refused = |conn: &Connection| match unit_context(conn, 3, unit_id, name, &scope, true) {
            Err(e) => format!("{e:#}"),
            Ok(_) => panic!("built a division from nothing"),
        };

        // Neither a folder nor a lecture.
        assert!(refused(&conn).starts_with("nothing to build Week 2"), "{}", refused(&conn));

        // Filed but not distilled.
        let transcript = "Weeks/Week 02 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md";
        let note_rel = ".classhub/corpus/Week 2 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md";
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (3, ?1, 'abc', 1, 1, 'md')",
            [transcript],
        )
        .expect("file");
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (3, ?1, ?2, 0, 1, 1, 1, ?3, 'Whole session', 'high', 'applied', 1)",
            params![unit_id, transcript, note_rel],
        )
        .expect("contribution");
        assert!(refused(&conn).starts_with("nothing to build Week 2"), "{}", refused(&conn));

        // A deck filed beside the transcript is the week's material, and the
        // week is this division: it builds, with the deck listed and the
        // transcript still named only through its note — which there is none of.
        let deck = "Weeks/Week 02 \u{2014} Study Designs/deck.pdf";
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (3, ?1, 'deck', 1, 1, 'pdf')",
            [deck],
        )
        .expect("deck");
        let (ctx, corpus) = unit_context(&conn, 3, unit_id, name, &scope, true).expect("builds from the deck");
        assert!(ctx.files_block.contains(&format!("- source: {deck}")), "{}", ctx.files_block);
        assert!(!ctx.files_block.contains(transcript), "{}", ctx.files_block);
        assert!(corpus.starts_with("(none"), "{corpus}");
        assert!(ctx.manifest.iter().any(|e| e.rel_path == deck), "{}", ctx.manifest_block);
        conn.execute("DELETE FROM files WHERE rel_path = ?1", [deck]).expect("remove deck");

        // Distilled: the note is the source, named with its transcript, and
        // the transcript stays in the manifest.
        let note = class_dir.join(note_rel);
        fs::create_dir_all(note.parent().expect("parent")).expect("corpus dir");
        fs::write(&note, "# distilled").expect("note");
        let (ctx, corpus) = unit_context(&conn, 3, unit_id, name, &scope, true).expect("builds from the note");
        assert!(ctx.files_block.is_empty(), "{}", ctx.files_block);
        assert!(
            corpus.contains(&format!("- {note_rel}\n  transcript: {transcript}")),
            "{corpus}"
        );
        assert!(ctx.manifest.iter().any(|e| e.rel_path == transcript), "{:?}", ctx.manifest_block);

        // Folder only: a division with material and no lecture.
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, rel_path, source)
             VALUES (3, 1, 'module', 'Module 1', 'Module 1', 'canvas')",
            [],
        )
        .expect("unit");
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (3, 'Module 1/Slides/deck.pptx', 'def', 1, 1, 'pptx')",
            [],
        )
        .expect("file");
        let module_id: i64 = conn
            .query_row("SELECT id FROM units WHERE name = 'Module 1'", [], |row| row.get(0))
            .expect("id");
        let (ctx, corpus) = unit_context(&conn, 3, module_id, "Module 1", &unit_scope(module_id), true)
            .expect("builds from the folder");
        assert!(ctx.files_block.contains("- source: Module 1/Slides/deck.pptx"), "{}", ctx.files_block);
        assert!(corpus.starts_with("(none"), "{corpus}");
        let _ = fs::remove_dir_all(&root);
    }
    /// The listing carries each guide's label — the division's name for a
    /// unit scope, since the key is its id and the reader never sees it.
    #[test]
    fn a_guide_s_label_is_the_division_s_name() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, source)
             VALUES (24, 1, 2, 'week', 'Week 2 — Responsible AI', 'syllabus')",
            [],
        )
        .expect("unit");
        for (scope, rel_path) in [
            ("unit:24", "Study Guides/Week 2 — Responsible AI.html"),
            ("master", "Study Guides/Semester Master.html"),
            ("Module 1", "Study Guides/Module 1.html"),
        ] {
            conn.execute(
                "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
                 VALUES (1, ?1, ?2, 1, '[]')",
                [scope, rel_path],
            )
            .expect("guide");
        }
        let guides = list_guides(&conn, 1).expect("list");
        let label = |scope: &str| guides.iter().find(|g| g.scope == scope).expect(scope).label.clone();
        assert_eq!(label("unit:24"), "Week 2 — Responsible AI");
        assert_eq!(label("master"), "Semester Master");
        assert_eq!(label("Module 1"), "Module 1");
    }
    /// The master's roster (SPEC §8.2) is the course's own divisions in its
    /// own order, each with its date or its week range, never a folder.
    #[test]
    fn the_master_s_roster_is_rendered_from_the_divisions() {
        let unit = |ordinal: i64, kind: &str, name: &str, starts_on: Option<&str>, weeks: Option<(i64, i64)>| {
            crate::units::UnitInfo {
                id: ordinal,
                ordinal,
                kind: kind.into(),
                name: name.into(),
                number: Some(ordinal),
                rel_path: None,
                starts_on: starts_on.map(Into::into),
                ends_on: None,
                first_week: weeks.map(|(f, _)| f),
                last_week: weeks.map(|(_, l)| l),
                source: "syllabus".into(),
                materials: None,
            }
        };
        let block = roster_block(&[
            unit(1, "week", "Week 1 — Introduction", Some("2026-08-20"), None),
            unit(2, "week", "Reading Days — No Class", None, None),
            unit(3, "part", "Part II: Alignment", None, Some((9, 12))),
        ]);
        assert_eq!(
            block,
            "- Week 1 — Introduction (from 2026-08-20)\n- Reading Days — No Class\n- Part II: Alignment (Weeks 9–12)"
        );
        assert!(roster_block(&[]).starts_with("(the course declares no divisions yet"));
    }

    /// The cards file's contract (SPEC §8.1, §8.4): an array of two-sided
    /// cards, empty allowed; anything else fails the run that wrote it.
    #[test]
    fn the_cards_file_is_checked_for_its_shape() {
        assert_eq!(parse_cards("[]").unwrap(), 0);
        assert_eq!(
            parse_cards(r#"[{"front":"MCAR?","back":"Missing completely at random","source":"00:50","topic":"missingness"},{"front":"a","back":"b"}]"#).unwrap(),
            2
        );
        for bad in [r#"{"front":"x","back":"y"}"#, r#"[{"front":"x"}]"#, r#"[{"front":"","back":"y"}]"#, "nope"] {
            assert!(parse_cards(bad).is_err(), "{bad}");
        }
        assert_eq!(cards_rel_path("Study Guides/Week 3 — Data.html"), ".classhub/cards/Week 3 — Data.json");
        assert_eq!(cards_rel_path(MASTER_OUTPUT), ".classhub/cards/Semester Master.json");
        assert_eq!(scope_label("practice:Study Guides/Practice/Week 3 — 2026-09-08.html", None), "Week 3 — 2026-09-08");
    }

    /// The exam's `{assessment}` block (SPEC §8.3): the weights, the next open
    /// quiz or exam from the given day, and the kinds on the calendar.
    #[test]
    fn the_assessment_block_names_the_weights_and_the_next_quiz() {
        let conn = crate::db::memory_db();
        assert!(assessment_block(&conn, 3, "2026-09-08").unwrap().starts_with("(no weights"));
        for (name, weight) in [("Assignments", 50.0), ("Quizzes", 20.0), ("Survey", 0.0)] {
            conn.execute(
                "INSERT INTO grade_categories (class_id, name, weight) VALUES (3, ?1, ?2)",
                rusqlite::params![name, weight],
            )
            .unwrap();
        }
        for (title, kind, due) in [("Quiz 1", "quiz", "2026-09-03"), ("Homework 1", "assignment", "2026-09-13"), ("Quiz 2", "quiz", "2026-09-24")] {
            conn.execute(
                "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
                 VALUES (3, ?1, ?2, ?3, 'open', 'syllabus')",
                rusqlite::params![title, kind, due],
            )
            .unwrap();
        }
        let block = assessment_block(&conn, 3, "2026-09-08").unwrap();
        assert_eq!(
            block,
            "- Grade weights: Assignments 50% · Quizzes 20% · Survey (no weight stated)\n\
             - Next assessment on the calendar: Quiz 2 (quiz) on 2026-09-24\n\
             - Kinds of assessment the calendar holds: assignment, quiz"
        );
    }
}
