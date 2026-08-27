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
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tauri::AppHandle;

use crate::db::{EXTRACTS_DIR, INBOX_DIR, emit_hub_change, now, with_conn};
use crate::scanner::APP_MANAGED_DIRS;

const PROMPT_TEMPLATE: &str = include_str!("../prompts/sort.md");

// ---------------------------------------------------------------------------
// Types

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxFile {
    pub name: String,
    pub size: i64,
    /// The user dismissed this file's proposal ("leave in inbox") and no new
    /// pending proposal exists: it stops counting and stops being re-proposed
    /// automatically; only a manual SORT INBOX includes it again.
    pub dismissed: bool,
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
    /// Per-file staging failures ("name: reason") — the batch survives them.
    pub failed: Vec<String>,
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

    // A drop is a batch: one bad member costs itself, never the rest, and
    // whatever staged is always announced and sorted.
    let mut staged = Vec::new();
    let mut skipped_folders = 0usize;
    let mut failed = Vec::new();
    for raw in paths {
        let path = PathBuf::from(raw);
        let name = match path.file_name() {
            Some(name) => name.to_string_lossy().into_owned(),
            None => {
                failed.push(format!("{raw}: no file name"));
                continue;
            }
        };
        if name.starts_with('.') {
            continue;
        }
        // fs::metadata resolves symlinks, so a link to a folder counts as a
        // folder skip instead of failing the copy below.
        let meta = match fs::metadata(&path) {
            Ok(meta) => meta,
            Err(e) => {
                failed.push(format!("{name}: {e}"));
                continue;
            }
        };
        if meta.is_dir() {
            // Staging stays predictable: files only, and the skip is surfaced.
            skipped_folders += 1;
            continue;
        }
        let target = free_slot(&inbox, &name);
        match fs::copy(&path, &target) {
            Ok(_) => staged.push(
                target
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            ),
            Err(e) => failed.push(format!("{name}: {e}")),
        }
    }
    if staged.is_empty() && skipped_folders == 0 && failed.is_empty() {
        bail!("nothing was staged");
    }

    // A fresh copy at a path supersedes a dismissal recorded for a file that
    // has since left the inbox (free_slot suffixes the name while the old
    // file is still there, so a match means the old file is gone) — a new
    // drop is a new decision, and the sort job must see it.
    if !staged.is_empty() {
        with_conn(app, |conn| {
            for name in &staged {
                conn.execute(
                    "DELETE FROM move_proposals
                     WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'dismissed'",
                    params![class_id, format!("{INBOX_DIR}/{name}")],
                )?;
            }
            Ok(())
        })?;
    }

    let job_id = if staged.is_empty() {
        None
    } else {
        enqueue_sort_job(app, class_id)?
    };
    emit_hub_change(app, "proposals"); // unproposed inbox files count on the card badge
    Ok(StageResult {
        staged,
        skipped_folders,
        failed,
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

/// Enqueues unless a sort job for the class is already queued/running —
/// files dropped while one runs are invisible to its prompt, and the
/// follow-up run after it settles (enqueue_followup) covers them.
fn enqueue_sort_job(app: &AppHandle, class_id: i64) -> Result<Option<i64>> {
    let prompt = with_conn(app, |conn| {
        if has_active_sort(conn, class_id)? {
            return Ok(None);
        }
        build_prompt(conn, class_id, false)
    })?;
    // Enqueued outside the DB lock — the job runner takes the lock itself.
    match prompt {
        Some(prompt) => Ok(Some(crate::jobs::enqueue_sort(app, class_id, &prompt)?)),
        None => Ok(None),
    }
}

/// Called by the job runner after a sort job succeeds and its row settles:
/// if fresh files arrived while it ran, another run picks them up — the
/// queue's "AWAITING PROPOSAL" is a promise the system keeps. Best-effort;
/// a failure here only costs the automatic follow-up (SORT INBOX remains).
pub fn enqueue_followup(app: &AppHandle, class_id: i64) {
    match enqueue_sort_job(app, class_id) {
        Ok(Some(job_id)) => eprintln!("sort follow-up enqueued as job {job_id}"),
        Ok(None) => {}
        Err(e) => eprintln!("sort follow-up failed to enqueue: {e:#}"),
    }
}

/// Manual trigger: retry after a failed job, files left sitting in the inbox,
/// or an explicit re-sort — manual runs cover every inbox file, dismissed
/// ones included (the escape hatch out of a dismissal).
pub fn run_sort_job(app: &AppHandle, class_id: i64) -> Result<i64> {
    let prompt = with_conn(app, |conn| {
        if has_active_sort(conn, class_id)? {
            bail!("a sort job for this class is already queued or running");
        }
        build_prompt(conn, class_id, true)
    })?
    .context("the inbox is empty — drop files onto the workspace first")?;
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

/// SPEC §10 step 2: the job receives the inbox listing + the class folder
/// tree. Automatic runs (post-drop, follow-up) cover only fresh files —
/// nothing already pending (the user may be reviewing those cards) and
/// nothing dismissed; a manual SORT INBOX re-proposes everything. Ok(None)
/// means there is nothing in scope to sort.
fn build_prompt(conn: &Connection, class_id: i64, manual: bool) -> Result<Option<String>> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let (pending, dismissed) = proposal_sources(conn, class_id)?;
    let inbox: Vec<InboxFile> = list_inbox(&class_dir)
        .into_iter()
        .filter(|f| {
            if manual {
                return true;
            }
            let key = format!("{INBOX_DIR}/{}", f.name);
            !pending.contains(&key) && !dismissed.contains(&key)
        })
        .collect();
    if inbox.is_empty() {
        return Ok(None);
    }
    let class_name: String = conn.query_row(
        "SELECT display_name FROM classes WHERE id = ?1",
        [class_id],
        |row| row.get(0),
    )?;

    let inbox_block = inbox
        .iter()
        .map(|f| {
            let mut line = format!(
                "- {INBOX_DIR}/{} ({})",
                f.name,
                crate::tools::format_size(f.size)
            );
            if let Some(hint) = transcript_hint(&class_dir, &f.name) {
                line.push_str(&format!("\n  {hint}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut tree_lines = Vec::new();
    let mut file_count = 0usize;
    crate::scanner::walk_tree(&class_dir, &class_dir, 0, &mut tree_lines, &mut file_count);
    let tree_block = if tree_lines.is_empty() {
        "(no folders yet — this class has no material)".to_string()
    } else {
        tree_lines.join("\n")
    };

    Ok(Some(
        PROMPT_TEMPLATE
            .replace("{class}", &class_name)
            .replace("{inbox}", &inbox_block)
            .replace("{tree}", &tree_block),
    ))
}

// ---------------------------------------------------------------------------
// Queue state

/// Files currently sitting in the inbox (flat — staging copies flat).
/// Lecture transcripts are the one inbox file whose name says nothing useful —
/// they are all "<date> — Lecture.md" — so the listing carries a line of their
/// subject matter and the sorter routes them by what the session was about.
/// Bounded: enough of the head to reach the speaker list and the first
/// paragraph, never the whole lecture.
fn transcript_hint(class_dir: &Path, name: &str) -> Option<String> {
    if !name.to_lowercase().ends_with(".md") {
        return None;
    }
    let path = class_dir.join(INBOX_DIR).join(name);
    let mut head = vec![0u8; 4096];
    let read = {
        use std::io::Read;
        let mut file = fs::File::open(&path).ok()?;
        file.read(&mut head).ok()?
    };
    head.truncate(read);
    crate::transcripts::describe(&String::from_utf8_lossy(&head))
}

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
            Some(InboxFile {
                name,
                size,
                dismissed: false, // filled in from move_proposals by the callers
            })
        })
        .collect();
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files
}

/// Pending and dismissed source paths for a class, in cheap form (no
/// reasoning strings — this also feeds the per-card badge on every
/// dashboard refetch). A source counts as dismissed only while no newer
/// pending proposal exists for it.
fn proposal_sources(
    conn: &Connection,
    class_id: i64,
) -> Result<(HashSet<String>, HashSet<String>)> {
    let mut stmt = conn.prepare(
        "SELECT source_rel_path, status FROM move_proposals
         WHERE class_id = ?1 AND status IN ('pending', 'dismissed')",
    )?;
    let rows = stmt.query_map([class_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut pending = HashSet::new();
    let mut dismissed = HashSet::new();
    for row in rows {
        let (source, status) = row?;
        if status == "pending" {
            pending.insert(source);
        } else {
            dismissed.insert(source);
        }
    }
    dismissed.retain(|s| !pending.contains(s));
    Ok((pending, dismissed))
}

pub fn sort_state(conn: &Connection, class_id: i64) -> Result<SortState> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let (_, dismissed) = proposal_sources(conn, class_id)?;
    let inbox = list_inbox(&class_dir)
        .into_iter()
        .map(|mut file| {
            file.dismissed = dismissed.contains(&format!("{INBOX_DIR}/{}", file.name));
            file
        })
        .collect();
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
    Ok(SortState { inbox, proposals })
}

/// Card badge (SPEC §10 step 5): everything actually waiting on a decision —
/// pending proposals plus inbox files that are neither proposed nor
/// dismissed. A dismissed file is a decision already made; it stops counting.
pub fn pending_count(conn: &Connection, class_id: i64) -> Result<i64> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let (pending, dismissed) = proposal_sources(conn, class_id)?;
    let fresh = list_inbox(&class_dir)
        .iter()
        .filter(|f| {
            let key = format!("{INBOX_DIR}/{}", f.name);
            !pending.contains(&key) && !dismissed.contains(&key)
        })
        .count();
    Ok(pending.len() as i64 + fresh as i64)
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
    let entries = crate::jobs::parse_entries(result_text)?;
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
    emit_hub_change(app, "proposals");
    Ok(summary)
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

/// Class-relative path hygiene: no traversal, no absolutes. An absolute path
/// is rejected rather than silently reinterpreted as class-relative — the
/// input meant something quite different from what would happen.
fn clean_rel(path: &str) -> Result<String> {
    let trimmed = path.trim();
    if trimmed.starts_with('/') {
        bail!("'{path}' is absolute — paths are class-relative");
    }
    let trimmed = trimmed.trim_end_matches('/');
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

/// Shared destination rules for job proposals and approval overrides (re-run
/// at approval time, where the fs operations actually happen).
/// The one destination policy, shared with chat's `propose_file_moves` so the
/// two proposal paths cannot enforce different rules.
pub(crate) fn validate_dest(class_dir: &Path, source_rel: &str, dest_rel: &str) -> Result<()> {
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
    let dest_abs = class_dir.join(dest_rel);
    if dest_abs.is_dir() {
        bail!("destination is a folder — include the file name");
    }
    if dest_abs.exists() {
        bail!("destination already exists");
    }
    ensure_no_symlink_ancestors(class_dir, dest_rel)?;
    Ok(())
}

/// No destination may route through a symlinked directory — create_dir_all
/// and rename would physically write outside the class folder while the
/// index records the file as inside it.
fn ensure_no_symlink_ancestors(class_dir: &Path, dest_rel: &str) -> Result<()> {
    let mut current = class_dir.to_path_buf();
    let parent = Path::new(dest_rel).parent().unwrap_or(Path::new(""));
    for component in parent.components() {
        current.push(component);
        if let Ok(meta) = fs::symlink_metadata(&current) {
            if meta.file_type().is_symlink() {
                bail!(
                    "destination goes through a symlink ('{}')",
                    current.display()
                );
            }
        } // missing components will be created as real directories
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

        // Both branches go through clean_rel — the stored destination was
        // validated when recorded, but the approve path is the last gate
        // before an fs operation and re-checks everything it relies on.
        let dest_rel = match dest_override {
            Some(over) => clean_rel(&over)?,
            None => clean_rel(&proposed_dest)?,
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
    emit_hub_change(app, "proposals");
    if approve {
        emit_hub_change(app, "files"); // the tree on disk changed
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
    // The rename has already happened; an Err below rolls it back explicitly.
    // A hard crash in the window between them is not covered: the file is at
    // the destination while the index and the pending proposal still name the
    // source. The next scan repairs the index, so the cost is one
    // re-extraction rather than lost data.
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
    let extracts = class_dir.join(EXTRACTS_DIR);
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
        let extracts = class_dir.join(EXTRACTS_DIR);
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
            let new_extract = recorded.map(|_| format!("{EXTRACTS_DIR}/{dest_rel}.md"));
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

