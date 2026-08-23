//! SPEC §10 — drop-to-sort (propose-and-confirm): staging dropped files into
//! `_Inbox/`, the read-only `sort_proposal` job that suggests destinations as
//! strict JSON, and the approval flow that performs the moves.
//!
//! The contract that matters: no file ever moves without explicit approval.
//! Drops are COPIED into the inbox (originals untouched), the job can only
//! read, and the filesystem is touched exclusively by `resolve_proposal` with
//! approve=true — which also updates the file index and writes the
//! `audit_log` entry.

use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::scanner::APP_MANAGED_DIRS;

const INBOX_DIR: &str = "_Inbox";
const EXTRACTS_PREFIX: &str = ".classhub/extracts";
const PROMPT_TEMPLATE: &str = include_str!("../prompts/sort.md");
/// Cap on file lines in the prompt's tree listing — generous for a class
/// folder, bounded if one ever grows huge (folders are always all listed).
const MAX_TREE_FILES: usize = 200;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn with_conn<T>(app: &AppHandle, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db = app.state::<crate::Db>();
    let guard = db.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    f(&guard)
}

/// Same push the chat write tools use (src/lib/query.ts maps areas to query
/// invalidations): `proposals` for queue/badge changes, `files` after a move.
fn emit_change(app: &AppHandle, area: &str) {
    let _ = app.emit("hub-changed", json!({ "area": area }));
}

// ---------------------------------------------------------------------------
// Types

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxFile {
    pub name: String,
    pub size: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proposal {
    pub id: i64,
    /// Class-relative, `_Inbox/...` for sort-job rows; anywhere for chat rows.
    pub source_rel_path: String,
    pub dest_rel_path: String,
    pub reasoning: String,
    /// high|medium|low from sort jobs; NULL on chat proposals.
    pub confidence: Option<String>,
    /// chat | sort_job
    pub source: String,
    pub created_at: i64,
}

/// The workspace queue: what is in the inbox plus every pending proposal.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SortState {
    pub inbox: Vec<InboxFile>,
    pub proposals: Vec<Proposal>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageResult {
    pub staged: Vec<String>,
    pub skipped_folders: usize,
    /// The auto-enqueued sort job; None when one was already queued/running.
    pub job_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// Staging (SPEC §10 step 1)

/// Copies dropped files into `<Class>/_Inbox/` — originals untouched — and
/// enqueues a sort job over the inbox unless one is already active.
pub fn stage_files(app: &AppHandle, class_id: i64, paths: &[String]) -> Result<StageResult> {
    let class_dir = with_conn(app, |conn| crate::scanner::class_dir(conn, class_id))?;
    if !class_dir.is_dir() {
        bail!("class folder not found: {}", class_dir.display());
    }
    let inbox = class_dir.join(INBOX_DIR);
    fs::create_dir_all(&inbox)?;

    let mut staged = Vec::new();
    let mut skipped_folders = 0usize;
    for raw in paths {
        let path = PathBuf::from(raw);
        let meta =
            fs::symlink_metadata(&path).with_context(|| format!("no such file: {raw}"))?;
        if meta.is_dir() {
            // Staging stays predictable: files only, and the skip is reported.
            skipped_folders += 1;
            continue;
        }
        let name = path
            .file_name()
            .with_context(|| format!("dropped path has no file name: {raw}"))?
            .to_string_lossy()
            .into_owned();
        if name.starts_with('.') {
            continue;
        }
        let target = free_slot(&inbox, &name);
        fs::copy(&path, &target).with_context(|| format!("copying {name} into the inbox"))?;
        staged.push(
            target
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        );
    }
    if staged.is_empty() && skipped_folders == 0 {
        bail!("nothing was staged");
    }

    let job_id = if staged.is_empty() {
        None
    } else {
        enqueue_sort_job(app, class_id)?
    };
    emit_change(app, "proposals"); // unproposed inbox files count on the card badge
    Ok(StageResult {
        staged,
        skipped_folders,
        job_id,
    })
}

/// `name.pdf` → `name (2).pdf` when the inbox already holds that name.
fn free_slot(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    let mut n = 2;
    loop {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

// ---------------------------------------------------------------------------
// The sort job (SPEC §10 step 2)

/// Enqueues unless a sort job for the class is already queued/running (its
/// output covers the inbox as it reads it; later drops enqueue again).
fn enqueue_sort_job(app: &AppHandle, class_id: i64) -> Result<Option<i64>> {
    let prompt = with_conn(app, |conn| {
        if has_active_sort(conn, class_id)? {
            return Ok(None);
        }
        build_prompt(conn, class_id).map(Some)
    })?;
    // Enqueued outside the DB lock — the job runner takes the lock itself.
    match prompt {
        Some(prompt) => Ok(Some(crate::jobs::enqueue_sort(app, class_id, &prompt)?)),
        None => Ok(None),
    }
}

/// Manual trigger: retry after a failed job, or files left sitting in the inbox.
pub fn run_sort_job(app: &AppHandle, class_id: i64) -> Result<i64> {
    let prompt = with_conn(app, |conn| {
        if has_active_sort(conn, class_id)? {
            bail!("a sort job for this class is already queued or running");
        }
        build_prompt(conn, class_id)
    })?;
    crate::jobs::enqueue_sort(app, class_id, &prompt)
}

fn has_active_sort(conn: &Connection, class_id: i64) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = 'sort_proposal' AND class_id = ?1
           AND status IN ('queued', 'running')",
        [class_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// SPEC §10 step 2: the job receives the inbox listing + the class folder tree.
fn build_prompt(conn: &Connection, class_id: i64) -> Result<String> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let class_name: String = conn.query_row(
        "SELECT display_name FROM classes WHERE id = ?1",
        [class_id],
        |row| row.get(0),
    )?;

    let inbox = list_inbox(&class_dir);
    if inbox.is_empty() {
        bail!("the inbox is empty — drop files onto the workspace first");
    }
    let inbox_block = inbox
        .iter()
        .map(|f| format!("- {INBOX_DIR}/{} ({})", f.name, format_size(f.size)))
        .collect::<Vec<_>>()
        .join("\n");

    let mut tree_lines = Vec::new();
    let mut file_count = 0usize;
    walk_tree(&class_dir, &class_dir, 0, &mut tree_lines, &mut file_count);
    let tree_block = if tree_lines.is_empty() {
        "(no folders yet — this class has no material)".to_string()
    } else {
        tree_lines.join("\n")
    };

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{inbox}", &inbox_block)
        .replace("{tree}", &tree_block))
}

/// Depth-first listing (`dir/` lines, then files) mirroring the scanner's
/// exclusions: app-managed dirs at the top level, hidden entries and symlinks
/// everywhere. File lines stop at MAX_TREE_FILES; folders are always listed.
fn walk_tree(
    dir: &Path,
    class_dir: &Path,
    depth: usize,
    out: &mut Vec<String>,
    file_count: &mut usize,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if depth == 0 && APP_MANAGED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            dirs.push(entry.path());
        } else {
            files.push(entry.path());
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| {
        a.to_string_lossy()
            .to_lowercase()
            .cmp(&b.to_string_lossy().to_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);

    for path in dirs {
        if let Ok(rel) = path.strip_prefix(class_dir) {
            out.push(format!("{}/", rel.to_string_lossy()));
        }
        walk_tree(&path, class_dir, depth + 1, out, file_count);
    }
    for path in files {
        *file_count += 1;
        if *file_count == MAX_TREE_FILES + 1 {
            out.push("… (more files omitted)".to_string());
        }
        if *file_count > MAX_TREE_FILES {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(class_dir) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
}

// ---------------------------------------------------------------------------
// Queue state

/// Files currently sitting in the inbox (flat — staging copies flat).
fn list_inbox(class_dir: &Path) -> Vec<InboxFile> {
    let Ok(entries) = fs::read_dir(class_dir.join(INBOX_DIR)) else {
        return Vec::new();
    };
    let mut files: Vec<InboxFile> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let size = e.metadata().map(|m| m.len() as i64).unwrap_or(0);
            Some(InboxFile { name, size })
        })
        .collect();
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files
}

pub fn sort_state(conn: &Connection, class_id: i64) -> Result<SortState> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let mut stmt = conn.prepare(
        "SELECT id, source_rel_path, dest_rel_path, reasoning, confidence, source, created_at
         FROM move_proposals WHERE class_id = ?1 AND status = 'pending' ORDER BY id",
    )?;
    let proposals = stmt
        .query_map([class_id], |row| {
            Ok(Proposal {
                id: row.get(0)?,
                source_rel_path: row.get(1)?,
                dest_rel_path: row.get(2)?,
                reasoning: row.get(3)?,
                confidence: row.get(4)?,
                source: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(SortState {
        inbox: list_inbox(&class_dir),
        proposals,
    })
}

/// Card badge (SPEC §10 step 5): everything waiting in the sorting flow —
/// pending proposals plus inbox files nothing has proposed for yet.
pub fn pending_count(conn: &Connection, class_id: i64) -> Result<i64> {
    let state = sort_state(conn, class_id)?;
    let proposed: HashSet<&str> = state
        .proposals
        .iter()
        .map(|p| p.source_rel_path.as_str())
        .collect();
    let unproposed = state
        .inbox
        .iter()
        .filter(|f| !proposed.contains(format!("{INBOX_DIR}/{}", f.name).as_str()))
        .count();
    Ok(state.proposals.len() as i64 + unproposed as i64)
}

// ---------------------------------------------------------------------------
// Job completion (jobs.rs calls this on success, before the row leaves 'running')

/// One entry of the job's contracted JSON array. `create_folders` rides the
/// contract for the reader's benefit but isn't needed to execute — approval
/// creates every missing destination parent — so unknown/extra fields are
/// simply ignored here.
#[derive(Deserialize)]
struct RawProposal {
    file: String,
    destination_rel_path: String,
    #[serde(default)]
    reasoning: String,
    confidence: Option<String>,
}

/// Parses and records the job's proposals. Returns the job summary; zero
/// recorded proposals is an error the caller demotes to job failure.
pub fn finalize_job(app: &AppHandle, class_id: i64, result_text: &str) -> Result<String> {
    let entries = parse_entries(result_text)?;
    let summary = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let mut recorded = 0usize;
        let mut skipped = Vec::new();
        // Per-entry tolerance throughout: one malformed or invalid entry
        // costs that entry, never the rest of the batch.
        for (index, raw) in entries.iter().enumerate() {
            let entry: RawProposal = match serde_json::from_value(raw.clone()) {
                Ok(entry) => entry,
                Err(e) => {
                    skipped.push(format!("entry {} (malformed: {e})", index + 1));
                    continue;
                }
            };
            match validate_entry(&class_dir, &entry) {
                Ok(valid) => {
                    upsert_proposal(conn, class_id, &valid)?;
                    recorded += 1;
                }
                Err(e) => skipped.push(format!("{} ({e:#})", entry.file)),
            }
        }
        if recorded == 0 {
            bail!(
                "no valid proposals in the job output{}",
                if skipped.is_empty() {
                    String::new()
                } else {
                    format!(" — skipped: {}", skipped.join("; "))
                }
            );
        }
        let mut summary = format!("{recorded} move proposal(s) awaiting approval");
        if !skipped.is_empty() {
            summary.push_str(&format!(" · skipped {}", skipped.join("; ")));
        }
        Ok(summary)
    })?;
    emit_change(app, "proposals");
    Ok(summary)
}

/// The prompt contracts a bare JSON array as the final message; tolerate a
/// fenced block or stray prose around it. A candidate `[` counts only when
/// the next non-whitespace character is `{` (or `]`), so a bracket inside
/// prose — e.g. a filename like `[draft] notes.pdf` — never wins the slice;
/// the stream deserializer then stops at the array's end, so trailing prose
/// is harmless too.
fn parse_entries(text: &str) -> Result<Vec<Value>> {
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices('[') {
        let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
        if !matches!(next, Some(b'{') | Some(b']')) {
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(Value::Array(entries))) = stream.next() {
            return Ok(entries);
        }
    }
    bail!("no JSON array of proposals in the job output")
}

struct ValidEntry {
    source_rel: String,
    dest_rel: String,
    reasoning: String,
    confidence: Option<String>,
}

fn validate_entry(class_dir: &Path, entry: &RawProposal) -> Result<ValidEntry> {
    let source_rel = clean_rel(&entry.file)?;
    if !source_rel.starts_with(&format!("{INBOX_DIR}/")) {
        bail!("source is not an inbox file");
    }
    if !class_dir.join(&source_rel).is_file() {
        bail!("no longer in the inbox");
    }
    let dest_rel = clean_rel(&entry.destination_rel_path)?;
    validate_dest(class_dir, &source_rel, &dest_rel)?;
    let confidence = entry
        .confidence
        .as_deref()
        .map(str::to_lowercase)
        .filter(|c| ["high", "medium", "low"].contains(&c.as_str()));
    let reasoning = match entry.reasoning.trim() {
        "" => "proposed by the sort job".to_string(),
        r => r.to_string(),
    };
    Ok(ValidEntry {
        source_rel,
        dest_rel,
        reasoning,
        confidence,
    })
}

/// Class-relative path hygiene: no traversal, no absolutes.
fn clean_rel(path: &str) -> Result<String> {
    let trimmed = path.trim().trim_matches('/');
    if trimmed.is_empty() {
        bail!("empty path");
    }
    if Path::new(trimmed)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("'{path}' must be a class-relative path");
    }
    Ok(trimmed.to_string())
}

/// Shared destination rules for job proposals and approval overrides.
fn validate_dest(class_dir: &Path, source_rel: &str, dest_rel: &str) -> Result<()> {
    if Path::new(dest_rel).file_name().is_none() {
        bail!("destination must include the file name");
    }
    // The scanner hides dot-entries at every depth, so a dotted segment
    // anywhere would make the moved file vanish from the app.
    for segment in dest_rel.split('/') {
        if segment.starts_with('.') {
            bail!("destination contains a hidden folder ('{segment}') — the app would never show it");
        }
    }
    // App-managed dirs are excluded at class-folder top level only (same
    // depth the scanner applies), so only the first segment is checked.
    let first = dest_rel.split('/').next().unwrap_or("");
    if APP_MANAGED_DIRS.contains(&first) {
        bail!("destination targets an app-managed folder");
    }
    if dest_rel == source_rel {
        bail!("destination equals the current path");
    }
    if class_dir.join(dest_rel).exists() {
        bail!("destination already exists");
    }
    Ok(())
}

/// One pending proposal per source file — a re-proposal replaces the pending
/// row instead of stacking (same rule as chat's propose_file_moves).
fn upsert_proposal(conn: &Connection, class_id: i64, v: &ValidEntry) -> Result<()> {
    let updated = conn.execute(
        "UPDATE move_proposals
         SET dest_rel_path = ?1, reasoning = ?2, confidence = ?3,
             source = 'sort_job', created_at = ?4
         WHERE class_id = ?5 AND source_rel_path = ?6 AND status = 'pending'",
        params![v.dest_rel, v.reasoning, v.confidence, now(), class_id, v.source_rel],
    )?;
    if updated == 0 {
        conn.execute(
            "INSERT INTO move_proposals
             (class_id, source_rel_path, dest_rel_path, reasoning, confidence,
              source, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'sort_job', 'pending', ?6)",
            params![class_id, v.source_rel, v.dest_rel, v.reasoning, v.confidence, now()],
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Resolution (SPEC §10 steps 3–4)

/// Approve performs the move (creating folders as proposed), updates the file
/// index, and writes the audit_log entry. Decline parks the row as dismissed —
/// the file stays exactly where it is.
pub fn resolve_proposal(
    app: &AppHandle,
    proposal_id: i64,
    approve: bool,
    dest_override: Option<String>,
) -> Result<String> {
    let summary = with_conn(app, |conn| {
        let row = conn
            .query_row(
                "SELECT class_id, source_rel_path, dest_rel_path, status, source, confidence
                 FROM move_proposals WHERE id = ?1",
                [proposal_id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                },
            )
            .optional()?
            .context("proposal not found")?;
        let (class_id, source_rel, proposed_dest, status, source, confidence) = row;
        if status != "pending" {
            bail!("this proposal was already resolved");
        }

        if !approve {
            conn.execute(
                "UPDATE move_proposals SET status = 'dismissed', resolved_at = ?1 WHERE id = ?2",
                params![now(), proposal_id],
            )?;
            return Ok(format!("left in place — {source_rel}"));
        }

        let dest_rel = match dest_override {
            Some(over) => clean_rel(&over)?,
            None => proposed_dest,
        };
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let src_abs = class_dir.join(&source_rel);
        if !src_abs.is_file() {
            bail!("'{source_rel}' is no longer on disk — leave or dismiss this proposal");
        }
        validate_dest(&class_dir, &source_rel, &dest_rel)?;

        // The move itself: folders created as proposed, then a same-volume rename.
        let dest_abs = class_dir.join(&dest_rel);
        if let Some(parent) = dest_abs.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&src_abs, &dest_abs)
            .with_context(|| format!("moving {source_rel} to {dest_rel}"))?;

        // Everything after the rename lands as one unit; on failure the DB
        // rolls back and the file (plus any moved extract artifacts) goes
        // back where it was, so the observable states are exactly "nothing
        // happened" or "everything happened".
        match record_move(
            conn,
            class_id,
            &class_dir,
            &source_rel,
            &dest_rel,
            proposal_id,
            &source,
            confidence.as_deref(),
        ) {
            Ok(()) => Ok(format!("moved — {source_rel} → {dest_rel}")),
            Err(e) => {
                undo_artifact_moves(&class_dir, &source_rel, &dest_rel);
                if let Err(undo) = fs::rename(&dest_abs, &src_abs) {
                    return Err(e.context(format!(
                        "recording the move failed AND the file could not be moved \
                         back ({undo}) — it is on disk at {dest_rel}; rescan the class"
                    )));
                }
                Err(e.context("recording the move failed — the file was moved back"))
            }
        }
    })?;
    emit_change(app, "proposals");
    if approve {
        emit_change(app, "files"); // the tree on disk changed
    }
    Ok(summary)
}

/// The whole DB side of an approved move in one transaction — stale-row
/// cleanup, index update, audit entry, proposal resolution — so a failure
/// anywhere leaves no partial commit and the caller can undo the rename.
#[allow(clippy::too_many_arguments)]
fn record_move(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
    proposal_id: i64,
    proposed_by: &str,
    confidence: Option<&str>,
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    // A stale index row can still occupy the destination (file deleted in
    // Finder, no rescan since) — validate_dest only checks the disk. Clear
    // it so the rel_path rewrite cannot hit UNIQUE(class_id, rel_path).
    tx.execute(
        "DELETE FROM files WHERE class_id = ?1 AND rel_path = ?2",
        params![class_id, dest_rel],
    )?;
    update_index(&tx, class_id, class_dir, source_rel, dest_rel)?;
    tx.execute(
        "INSERT INTO audit_log (action, payload, created_at)
         VALUES ('sort.move', ?1, ?2)",
        params![
            json!({
                "proposalId": proposal_id,
                "classId": class_id,
                "from": source_rel,
                "to": dest_rel,
                "proposedBy": proposed_by,
                "confidence": confidence,
            })
            .to_string(),
            now()
        ],
    )?;
    tx.execute(
        "UPDATE move_proposals
         SET status = 'approved', dest_rel_path = ?1, resolved_at = ?2 WHERE id = ?3",
        params![dest_rel, now(), proposal_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Best-effort reversal of `update_index`'s artifact renames, for the
/// failure path: each relocated extract file goes back to its source mirror.
fn undo_artifact_moves(class_dir: &Path, source_rel: &str, dest_rel: &str) {
    let extracts = class_dir.join(EXTRACTS_PREFIX);
    for suffix in [".md", ".pdf", ".pdf.sha256"] {
        let new = extracts.join(format!("{dest_rel}{suffix}"));
        let old = extracts.join(format!("{source_rel}{suffix}"));
        if new.is_file() && !old.exists() {
            let _ = fs::rename(&new, &old);
        }
    }
}

/// SPEC §10 step 4, "updates the file index": an indexed source keeps its row
/// (rel_path rewritten) and its extract artifacts move with it — the mirror
/// rule of SPEC §4 — so no extraction is re-spent; an inbox file was never
/// indexed (_Inbox is excluded from scans), so it gets a fresh row and the
/// next auto-extract picks it up as new material.
fn update_index(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
) -> Result<()> {
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM files WHERE class_id = ?1 AND rel_path = ?2",
            params![class_id, source_rel],
            |row| row.get(0),
        )
        .optional()?;

    if let Some(file_id) = existing {
        // The markdown extract plus any PPTX-conversion sidecars mirror the
        // source rel_path — move them along so nothing goes stale.
        let extracts = class_dir.join(EXTRACTS_PREFIX);
        for suffix in [".md", ".pdf", ".pdf.sha256"] {
            let old = extracts.join(format!("{source_rel}{suffix}"));
            if old.is_file() {
                let new = extracts.join(format!("{dest_rel}{suffix}"));
                if let Some(parent) = new.parent() {
                    fs::create_dir_all(parent)?;
                }
                if let Err(e) = fs::rename(&old, &new) {
                    eprintln!("extract artifact move failed ({}): {e}", old.display());
                }
            }
        }
        let recorded: Option<String> = conn.query_row(
            "SELECT extract_rel_path FROM files WHERE id = ?1",
            [file_id],
            |row| row.get::<_, Option<String>>(0),
        )?;
        // Point the row at the relocated extract only if it actually landed.
        // A row must never claim an extract that is not on disk — the source
        // hash didn't change, so the pipeline would trust the dangling path
        // forever; clearing the extract columns makes the next scan re-extract.
        let md_ok = extracts.join(format!("{dest_rel}.md")).is_file();
        if recorded.is_some() && !md_ok {
            conn.execute(
                "UPDATE files SET rel_path = ?1, extract_rel_path = NULL,
                        extracted_at = NULL, extracted_sha256 = NULL
                 WHERE id = ?2",
                params![dest_rel, file_id],
            )?;
        } else {
            let new_extract = recorded.map(|_| format!("{EXTRACTS_PREFIX}/{dest_rel}.md"));
            conn.execute(
                "UPDATE files SET rel_path = ?1, extract_rel_path = ?2 WHERE id = ?3",
                params![dest_rel, new_extract, file_id],
            )?;
        }
    } else {
        let abs = class_dir.join(dest_rel);
        let meta = fs::metadata(&abs)?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let sha256 = crate::scanner::hash_file(&abs)?;
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(class_id, rel_path) DO UPDATE SET
               sha256 = excluded.sha256, size = excluded.size,
               mtime = excluded.mtime, kind = excluded.kind",
            params![
                class_id,
                dest_rel,
                sha256,
                meta.len() as i64,
                mtime,
                crate::scanner::kind_for(&abs)
            ],
        )?;
    }
    Ok(())
}

fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
