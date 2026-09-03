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

use crate::db::{EXTRACTS_DIR, INBOX_DIR, WEEKS_DIR, emit_hub_change, now, with_conn};
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
    /// Class-relative, `_Inbox/...` for sort-job and Canvas rows; anywhere for
    /// chat rows.
    pub source_rel_path: String,
    pub dest_rel_path: String,
    pub reasoning: String,
    /// high|medium|low from sort jobs; NULL on chat and Canvas proposals —
    /// neither is a model rating its own guess.
    pub confidence: Option<String>,
    /// chat | sort_job | canvas
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
    //
    // The inbox sits inside the write-scope guard's walk (SPEC §6), so a drop
    // during a running job is a change the guard would pin on the job; the
    // `sort.staged` row is how it tells the app's own writes apart. Both
    // writes land as one transaction, and a failure is logged rather than
    // propagated: the files are on disk by now, and failing the drop over a
    // finished copy would earn a duplicate on the retry (the same reasoning
    // `lectures::add` states for its own row).
    if !staged.is_empty() {
        let recorded = with_conn(app, |conn| {
            let rel_paths: Vec<String> = staged
                .iter()
                .map(|name| format!("{INBOX_DIR}/{name}"))
                .collect();
            let tx = conn.unchecked_transaction()?;
            for rel_path in &rel_paths {
                tx.execute(
                    "DELETE FROM move_proposals
                     WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'dismissed'",
                    params![class_id, rel_path],
                )?;
            }
            crate::db::audit(
                &tx,
                "sort.staged",
                json!({ "classId": class_id, "staged": rel_paths }),
            )?;
            tx.commit()?;
            Ok(())
        });
        if let Err(e) = recorded {
            eprintln!("class {class_id}: recording a drop of {} file(s) failed: {e:#}", staged.len());
        }
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
///
/// Every path that puts a file in the inbox goes through this, so no ingestion
/// route can overwrite what another one left there waiting for approval.
pub(crate) fn free_slot(dir: &Path, name: &str) -> PathBuf {
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
    // Enqueued outside the DB lock — the job runner takes the lock itself, and
    // its insert is what settles "one sort per class"; the check above only
    // saves building a prompt nothing will run.
    match prompt {
        Some(prompt) => crate::jobs::enqueue_sort(app, class_id, None, &prompt),
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
    crate::jobs::enqueue_sort(app, class_id, None, &prompt)?
        .context("a sort job for this class is already queued or running")
}

/// SORT BY CONTENT on a Canvas card (SPEC §7.2): a sort job over that one
/// file, and — because it was asked for — the one case where the sort's
/// destination replaces the folder Canvas filed the file in. The job's
/// `scope` carries the file, which is what `finalize_job` reads to know this
/// run was explicit; an automatic sort keeps deferring to Canvas.
pub fn sort_by_content(app: &AppHandle, proposal_id: i64) -> Result<i64> {
    let (class_id, source_rel, prompt) = with_conn(app, |conn| {
        let (class_id, source_rel, status, source): (i64, String, String, String) = conn
            .query_row(
                "SELECT class_id, source_rel_path, status, source
                 FROM move_proposals WHERE id = ?1",
                [proposal_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?
            .context("proposal not found")?;
        if status != "pending" {
            bail!("this proposal was already resolved");
        }
        if source != "canvas" {
            bail!("only a Canvas placement is sorted by content — this one already was");
        }
        if has_active_sort(conn, class_id)? {
            bail!("a sort job for this class is already queued or running");
        }
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let name = source_rel
            .strip_prefix(&format!("{INBOX_DIR}/"))
            .context("the proposal's source is not an inbox file")?;
        let file = list_inbox(&class_dir)
            .into_iter()
            .find(|f| f.name == name)
            .with_context(|| format!("'{source_rel}' is no longer in the inbox"))?;
        let prompt = render_prompt(conn, class_id, &class_dir, &[file])?;
        Ok((class_id, source_rel, prompt))
    })?;
    // The active-sort check above ran under the lock and this runs after it;
    // the insert re-checks under the write lock, so a second click or the
    // other process cannot both get a job.
    crate::jobs::enqueue_sort(app, class_id, Some(&source_rel), &prompt)?
        .context("a sort job for this class is already queued or running")
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
    let placed = canvas_placed(conn, class_id)?;
    let inbox: Vec<InboxFile> = list_inbox(&class_dir)
        .into_iter()
        .filter(|f| {
            let key = format!("{INBOX_DIR}/{}", f.name);
            // Out of scope even for a manual run: Canvas already said where
            // this one belongs, and "Change destination" on the card is the way
            // to disagree with that.
            if placed.contains(&key) {
                return false;
            }
            if manual {
                return true;
            }
            !pending.contains(&key) && !dismissed.contains(&key)
        })
        .collect();
    if inbox.is_empty() {
        return Ok(None);
    }
    Ok(Some(render_prompt(conn, class_id, &class_dir, &inbox)?))
}

/// The prompt itself, over whichever inbox files the caller put in scope.
fn render_prompt(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    inbox: &[InboxFile],
) -> Result<String> {
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
            if let Some(hint) = transcript_hint(class_dir, &f.name) {
                line.push_str(&format!("\n  {hint}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    let mut tree_lines = Vec::new();
    let mut file_count = 0usize;
    crate::scanner::walk_tree(class_dir, class_dir, 0, &mut tree_lines, &mut file_count);
    let tree_block = if tree_lines.is_empty() {
        "(no folders yet — this class has no material)".to_string()
    } else {
        tree_lines.join("\n")
    };

    // Every class's names, not just this one's. Without it each class is sorted
    // in isolation and coins its own word for the same kind of material.
    let vocabulary = crate::scanner::folder_vocabulary(conn)?;
    let vocabulary_block = if vocabulary.is_empty() {
        "(no folders anywhere yet — you are naming the first one)".to_string()
    } else {
        vocabulary
            .iter()
            .map(|(name, classes)| {
                format!(
                    "- {name} — used by {classes} class{}",
                    if *classes == 1 { "" } else { "es" }
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // The week folders a transcript can be routed to. Named exactly as the app
    // builds them (SPEC §4), because that name is what joins the lecture back to
    // the course's own division — a folder the sorter coined itself would file
    // the transcript somewhere real and map it to nothing.
    let slots = crate::units::week_slots(conn, class_id)?;
    let weeks_block = if slots.is_empty() {
        "  (this course publishes no schedule — leave a transcript in the inbox and say \
         so, rather than inventing a week for it)"
            .to_string()
    } else {
        slots
            .iter()
            .map(|slot| {
                let meets = slot
                    .meets_on
                    .as_deref()
                    .map(|d| format!(" — meets {d}"))
                    .unwrap_or_default();
                format!("  - {WEEKS_DIR}/{}{meets}", slot.folder)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(PROMPT_TEMPLATE
        .replace("{class}", &class_name)
        .replace("{inbox}", &inbox_block)
        .replace("{tree}", &tree_block)
        .replace("{weeks}", &weeks_block)
        .replace("{vocabulary}", &vocabulary_block))
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
    // `read` may legally return fewer bytes than asked for, and a short one
    // that stops before the `source:` line makes a real transcript look like
    // any other note — which changes where the sorter files it.
    let mut head = Vec::with_capacity(4096);
    {
        use std::io::Read;
        let file = fs::File::open(&path)
            .inspect_err(|e| eprintln!("transcript hint: opening {} — {e}", path.display()))
            .ok()?;
        file.take(4096)
            .read_to_end(&mut head)
            .inspect_err(|e| eprintln!("transcript hint: reading {} — {e}", path.display()))
            .ok()?;
    }
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

/// Inbox files Canvas has already placed and that are still awaiting approval.
///
/// These are out of scope for a sort job however it was started. Canvas's
/// destination is where the professor filed the material, so asking a model to
/// name a folder for it spends a job to produce a guess that `upsert_proposal`
/// would then have to ignore.
fn canvas_placed(conn: &Connection, class_id: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT source_rel_path FROM move_proposals
         WHERE class_id = ?1 AND status = 'pending' AND source = 'canvas'",
    )?;
    let rows = stmt.query_map([class_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<HashSet<_>>>()?)
}

/// Resolves every pending proposal whose file is no longer on disk, so the
/// queue and the badge agree with the folder rather than with a row.
///
/// A proposal names a file the app expects to move; sorted by hand in Finder,
/// or already moved from another build's queue, it leaves a card whose
/// APPROVE can only fail. Dismissed rather than deleted: dismissal is terminal
/// per path (a file that comes back to that inbox path is a fresh drop, which
/// `stage_files` clears the row for), and the audit row keeps the proposal
/// itself. Runs before either reader answers, which is what keeps the two in
/// step — a chat-side count of the table is the one reader this does not sit
/// in front of.
fn dismiss_vanished(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<()> {
    // A class folder that is not there — an unmounted volume, a root setting
    // mid-change — is not a folder every proposed file has left.
    if !class_dir.is_dir() {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT id, source_rel_path, dest_rel_path, reasoning, confidence, source, created_at
         FROM move_proposals WHERE class_id = ?1 AND status = 'pending'",
    )?;
    let vanished: Vec<(i64, serde_json::Value)> = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                json!({
                    "proposalId": row.get::<_, i64>(0)?,
                    "classId": class_id,
                    "sourceRelPath": row.get::<_, String>(1)?,
                    "destRelPath": row.get::<_, String>(2)?,
                    "reasoning": row.get::<_, String>(3)?,
                    "confidence": row.get::<_, Option<String>>(4)?,
                    "proposedBy": row.get::<_, String>(5)?,
                    "createdAt": row.get::<_, i64>(6)?,
                }),
            ))
        })?
        .filter_map(|row| match row {
            // A stored path is read through the same hygiene a new one gets:
            // every producer validated it on the way in, and a row that would
            // not pass now is one to leave alone rather than stat outside the
            // class folder. Stats are per proposal because a listing could not
            // tell a missing file from an unreadable one (`file_is_gone`).
            Ok((id, source_rel, payload))
                if clean_rel(&source_rel)
                    .map(|rel| file_is_gone(&class_dir.join(rel)))
                    .unwrap_or(false) =>
            {
                Some(Ok((id, payload)))
            }
            Ok(_) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if vanished.is_empty() {
        return Ok(());
    }
    dismiss_rows(conn, vanished)?;
    Ok(())
}

/// Whether a proposed file has definitely left: nothing at the path, or
/// something that is not a file. "Cannot tell" — a permission or I/O error, an
/// unreadable folder on the way — is not "gone", because a dismissal is
/// terminal per path and a transient failure must not become one.
fn file_is_gone(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(meta) => !meta.is_file(),
        Err(e) => e.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Pending move proposals across every class, for the chat overview — after
/// the vanish pass in each, so chat counts what the cards count.
pub fn pending_move_count(conn: &Connection) -> Result<i64> {
    let mut stmt = conn.prepare("SELECT id FROM classes")?;
    let class_ids = stmt
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for class_id in class_ids {
        match crate::scanner::class_dir(conn, class_id) {
            Ok(class_dir) => reconcile_vanished(conn, class_id, &class_dir),
            Err(e) => eprintln!("class {class_id}: no folder to reconcile against: {e:#}"),
        }
    }
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM move_proposals WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?)
}

/// The vanish pass as the readers run it: best-effort housekeeping on the way
/// to an answer. A failure here — the other process holding the write lock
/// past the busy timeout, say — costs one stale badge, not the dashboard,
/// which `list_classes` would otherwise fail for every class at once.
fn reconcile_vanished(conn: &Connection, class_id: i64, class_dir: &Path) {
    if let Err(e) = dismiss_vanished(conn, class_id, class_dir) {
        eprintln!("class {class_id}: reconciling vanished proposals failed: {e:#}");
    }
}

/// Dismisses the rows the vanish pass selected, in one transaction, and says
/// how many it actually dismissed. The other process may have resolved a row
/// between the read and here — an approve is exactly what makes a file leave
/// the inbox — so each write is conditional on the row still being pending,
/// and the audit row is written only by the call that dismissed it.
fn dismiss_rows(conn: &Connection, vanished: Vec<(i64, serde_json::Value)>) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut dismissed = 0usize;
    for (id, payload) in vanished {
        let changed = tx.execute(
            "UPDATE move_proposals SET status = 'dismissed', resolved_at = ?1
             WHERE id = ?2 AND status = 'pending'",
            params![now(), id],
        )?;
        if changed == 0 {
            continue;
        }
        crate::db::audit(&tx, "sort.proposal_vanished", payload)?;
        dismissed += 1;
    }
    tx.commit()?;
    Ok(dismissed)
}

pub fn sort_state(conn: &Connection, class_id: i64) -> Result<SortState> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    reconcile_vanished(conn, class_id, &class_dir);
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
    reconcile_vanished(conn, class_id, &class_dir);
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
///
/// `scope` is the one file an explicit SORT BY CONTENT was pressed for, or
/// None for a run over the inbox; only that file's entry may replace a Canvas
/// placement.
pub fn finalize_job(
    app: &AppHandle,
    class_id: i64,
    scope: Option<&str>,
    result_text: &str,
) -> Result<String> {
    let entries = crate::jobs::parse_entries(result_text)?;
    let summary = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        record_entries(conn, class_id, &class_dir, scope, &entries)
    })?;
    emit_hub_change(app, "proposals");
    Ok(summary)
}

/// Records what a sort job returned, entry by entry, and says what it did.
/// Zero recorded proposals is an error the caller demotes to job failure —
/// except for a scoped run whose file was resolved while it ran, which is an
/// outcome rather than a failure.
fn record_entries(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    scope: Option<&str>,
    entries: &[serde_json::Value],
) -> Result<String> {
    // A scoped run is for one file. Resolved while the job ran — approved,
    // which is what moved it out of the inbox, or left in the inbox — the
    // sort's answer arrived too late: recording it would fail the job over
    // an approve that succeeded, or bring back a card that was declined.
    if let Some(file) = scope {
        if !has_pending(conn, class_id, file)? {
            return Ok(format!(
                "{file} was resolved while the sort ran — nothing recorded"
            ));
        }
    }
    let mut recorded = 0usize;
    let mut ignored = 0usize;
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
        let valid = match validate_entry(class_dir, &entry) {
            Ok(valid) => valid,
            Err(e) => {
                skipped.push(format!("{} ({e:#})", entry.file));
                continue;
            }
        };
        let wrote = match scope {
            // A scoped run records only its file: the prompt named one, and
            // an entry for another is the model overreaching.
            Some(file) if file != valid.source_rel => {
                ignored += 1;
                continue;
            }
            Some(_) => override_canvas_placement(conn, class_id, &valid)?,
            None => upsert_proposal(
                conn,
                class_id,
                "sort_job",
                &valid.source_rel,
                &valid.dest_rel,
                &valid.reasoning,
                valid.confidence.as_deref(),
            )?,
        };
        if wrote {
            recorded += 1;
        } else {
            skipped.push(format!("{} (Canvas placement kept)", entry.file));
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
    if ignored > 0 {
        summary.push_str(&format!(" · ignored {ignored} file(s) outside the sort's scope"));
    }
    Ok(summary)
}

/// Whether a pending proposal exists for a source path in the class.
fn has_pending(conn: &Connection, class_id: i64, source_rel: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM move_proposals
         WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'",
        params![class_id, source_rel],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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
/// row instead of stacking. The one path all three producers take, so the rule
/// cannot drift between them.
///
/// Canvas is the exception the sort job defers to. Where Canvas filed a file is
/// an *observation* — the professor put it in that folder — while a sort job's
/// destination is an *inference* from a filename and a tree. Letting the guess
/// replace the record is how a slide deck Canvas had placed under `Slides/`
/// ended up proposed for a `Week 1/Slides/` that nobody had said existed.
///
/// Chat is not that exception: a chat move is the user asking for one, which is
/// a decision rather than a guess. Either way the card's own "Change
/// destination" remains the way to retarget, and SORT BY CONTENT
/// (`override_canvas_placement`) is the explicit request that lets a sort's
/// destination stand in for Canvas's.
pub(crate) fn upsert_proposal(
    conn: &Connection,
    class_id: i64,
    source: &str,
    source_rel: &str,
    dest_rel: &str,
    reasoning: &str,
    confidence: Option<&str>,
) -> Result<bool> {
    let held: Option<String> = conn
        .query_row(
            "SELECT source FROM move_proposals
             WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'",
            params![class_id, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    match held.as_deref() {
        // Refused, and says so: a caller counting proposals must not count
        // one the record kept out.
        Some("canvas") if source == "sort_job" => Ok(false),
        Some(_) => {
            replace_pending(conn, class_id, source, source_rel, dest_rel, reasoning, confidence)?;
            Ok(true)
        }
        None => {
            conn.execute(
                "INSERT INTO move_proposals
                 (class_id, source_rel_path, dest_rel_path, reasoning, confidence,
                  source, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
                params![class_id, source_rel, dest_rel, reasoning, confidence, source, now()],
            )?;
            Ok(true)
        }
    }
}

/// Rewrites the pending row for a source in place. Source and confidence reset
/// too: replacing a sort job's pending row must not leave its HIGH chip
/// attributed to a chat destination.
fn replace_pending(
    conn: &Connection,
    class_id: i64,
    source: &str,
    source_rel: &str,
    dest_rel: &str,
    reasoning: &str,
    confidence: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE move_proposals
         SET dest_rel_path = ?1, reasoning = ?2, confidence = ?3,
             source = ?4, created_at = ?5
         WHERE class_id = ?6 AND source_rel_path = ?7 AND status = 'pending'",
        params![dest_rel, reasoning, confidence, source, now(), class_id, source_rel],
    )?;
    Ok(())
}

/// The sort's destination in place of Canvas's, for the one file SORT BY
/// CONTENT was pressed for. The professor's folder moves onto the card's
/// reasoning, so the override reads as a disagreement with a placement rather
/// than as Canvas never having said anything. Says whether it wrote: a row
/// retargeted by chat while the job ran is replaced the ordinary way, and a
/// row no longer pending is left alone rather than brought back.
fn override_canvas_placement(
    conn: &Connection,
    class_id: i64,
    valid: &ValidEntry,
) -> Result<bool> {
    let held: Option<(String, String)> = conn
        .query_row(
            "SELECT source, dest_rel_path FROM move_proposals
             WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'",
            params![class_id, valid.source_rel],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let reasoning = match held {
        None => return Ok(false),
        Some((source, _)) if source != "canvas" => valid.reasoning.clone(),
        Some((_, canvas_dest)) => {
            let placement = match canvas_dest.rsplit_once('/') {
                Some((dir, _)) => format!("under \"{dir}\""),
                None => "in the class root".to_string(),
            };
            format!("Canvas files it {placement} — {}", valid.reasoning)
        }
    };
    replace_pending(
        conn,
        class_id,
        "sort_job",
        &valid.source_rel,
        &valid.dest_rel,
        &reasoning,
        valid.confidence.as_deref(),
    )?;
    Ok(true)
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
    // SPEC §8.5: refiling a lecture into a different week is how a wrong unit
    // is corrected, so the map has to travel with the file rather than being
    // left naming a path nothing is at.
    let effects =
        crate::lectures::refile_lecture(&tx, class_id, class_dir, source_rel, dest_rel)?;
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
    // Only now: the database can no longer roll back, so the corpus note moves
    // against a record that already says it moved. Before the commit it stays
    // put, which is what keeps the caller's undo path — the transcript rename
    // and the extract artifacts — the whole of what a failure has to reverse.
    effects.apply();
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


#[cfg(test)]
mod tests {
    use super::*;

    fn propose(conn: &Connection, source: &str, dest: &str) {
        upsert_proposal(
            conn,
            1,
            source,
            &format!("{INBOX_DIR}/deck.pdf"),
            dest,
            "because",
            None,
        )
        .expect("propose");
    }

    fn held(conn: &Connection) -> (String, String, i64) {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM move_proposals", [], |r| r.get(0))
            .expect("count");
        let (dest, source) = conn
            .query_row(
                "SELECT dest_rel_path, source FROM move_proposals WHERE class_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("one row");
        (dest, source, count)
    }

    /// Canvas's destination is an observation — the professor filed it there.
    /// A sort job's is an inference from a filename and a tree, and a guess
    /// must not overwrite the record.
    #[test]
    fn a_sort_job_does_not_replace_what_canvas_placed() {
        let conn = crate::db::memory_db();
        propose(&conn, "canvas", "Slides/deck.pdf");
        propose(&conn, "sort_job", "Week 1/Slides/deck.pdf");
        assert_eq!(
            held(&conn),
            ("Slides/deck.pdf".into(), "canvas".into(), 1),
            "a sort job overwrote Canvas, or stacked a second card"
        );
    }

    /// A chat move is the user asking for one, which is a decision rather than
    /// a guess — so it retargets, and the card stops claiming Canvas said so.
    #[test]
    fn a_chat_move_retargets_a_canvas_placement() {
        let conn = crate::db::memory_db();
        propose(&conn, "canvas", "Slides/deck.pdf");
        propose(&conn, "chat", "Readings/deck.pdf");
        assert_eq!(
            held(&conn),
            ("Readings/deck.pdf".into(), "chat".into(), 1)
        );
    }

    /// One pending proposal per source file, whichever producer re-proposes it.
    #[test]
    fn a_re_proposal_replaces_rather_than_stacks() {
        let conn = crate::db::memory_db();
        propose(&conn, "sort_job", "Slides/deck.pdf");
        propose(&conn, "sort_job", "Readings/deck.pdf");
        assert_eq!(
            held(&conn),
            ("Readings/deck.pdf".into(), "sort_job".into(), 1)
        );
    }

    /// SORT BY CONTENT is the one explicit request that lets a sort's
    /// destination replace Canvas's — and the professor's folder stays on the
    /// card, in the reasoning, so the override reads as a disagreement.
    #[test]
    fn an_explicit_content_sort_replaces_the_canvas_placement_and_names_its_folder() {
        let conn = crate::db::memory_db();
        propose(&conn, "canvas", "Slides/deck.pdf");
        let valid = ValidEntry {
            source_rel: format!("{INBOX_DIR}/deck.pdf"),
            dest_rel: "Module 2/Slides/deck.pdf".into(),
            reasoning: "the deck covers Module 2's topics".into(),
            confidence: Some("high".into()),
        };
        assert!(override_canvas_placement(&conn, 1, &valid).expect("override"));
        assert_eq!(
            held(&conn),
            ("Module 2/Slides/deck.pdf".into(), "sort_job".into(), 1)
        );
        let row = |conn: &Connection| -> (String, Option<String>) {
            conn.query_row(
                "SELECT reasoning, confidence FROM move_proposals WHERE class_id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row")
        };
        let (reasoning, confidence) = row(&conn);
        assert_eq!(
            reasoning,
            "Canvas files it under \"Slides\" — the deck covers Module 2's topics"
        );
        assert_eq!(confidence.as_deref(), Some("high"));

        // A placement in the class root is named as such, not as a quoted
        // folder called "the class folder".
        conn.execute("DELETE FROM move_proposals", []).expect("clear");
        propose(&conn, "canvas", "deck.pdf");
        assert!(override_canvas_placement(&conn, 1, &valid).expect("override"));
        assert_eq!(
            row(&conn).0,
            "Canvas files it in the class root — the deck covers Module 2's topics"
        );

        // Resolved while the job ran: nothing to override, and nothing is
        // brought back.
        conn.execute("UPDATE move_proposals SET status = 'dismissed'", [])
            .expect("dismiss");
        assert!(!override_canvas_placement(&conn, 1, &valid).expect("override"));
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM move_proposals WHERE status = 'pending'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(pending, 0, "a declined card came back");
    }

    /// A scoped run is for one file: an entry for any other inbox file is
    /// ignored rather than recorded, a Canvas row kept by the automatic rule
    /// counts as skipped rather than recorded, and a scoped file resolved
    /// while the job ran is an outcome rather than a failure.
    #[test]
    fn a_scoped_sort_records_only_its_file() {
        let dir = std::env::temp_dir().join(format!("classhub-scoped-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(INBOX_DIR)).expect("inbox");
        for name in ["asked.pdf", "other.pdf"] {
            fs::write(dir.join(INBOX_DIR).join(name), "x").expect("file");
        }
        let conn = crate::db::memory_db();
        for name in ["asked.pdf", "other.pdf"] {
            upsert_proposal(
                &conn,
                1,
                "canvas",
                &format!("{INBOX_DIR}/{name}"),
                &format!("Slides/{name}"),
                "Canvas files it under \"Slides\"",
                None,
            )
            .expect("propose");
        }
        let entries: Vec<serde_json::Value> = ["asked.pdf", "other.pdf"]
            .iter()
            .map(|name| {
                json!({
                    "file": format!("{INBOX_DIR}/{name}"),
                    "destination_rel_path": format!("Module 2/Slides/{name}"),
                    "reasoning": "read the deck",
                    "confidence": "high",
                })
            })
            .collect();
        let asked = format!("{INBOX_DIR}/asked.pdf");

        let summary = record_entries(&conn, 1, &dir, Some(&asked), &entries).expect("record");
        assert!(summary.starts_with("1 move proposal(s)"), "{summary}");
        assert!(summary.contains("ignored 1 file(s)"), "{summary}");
        let source_of = |name: &str| -> String {
            conn.query_row(
                "SELECT source FROM move_proposals WHERE source_rel_path = ?1",
                [format!("{INBOX_DIR}/{name}")],
                |r| r.get(0),
            )
            .expect("row")
        };
        assert_eq!(source_of("asked.pdf"), "sort_job");
        assert_eq!(source_of("other.pdf"), "canvas", "a scoped run touched another file's Canvas row");

        // Unscoped, the automatic rule keeps the Canvas row and the run records
        // nothing rather than counting a refusal as a proposal.
        let refused = record_entries(&conn, 1, &dir, None, &entries[1..]);
        let message = format!("{:#}", refused.expect_err("nothing was recorded"));
        assert!(message.contains("Canvas placement kept"), "{message}");
        assert_eq!(source_of("other.pdf"), "canvas");

        // Scoped to a file resolved while the job ran: an outcome, not a
        // failure, and no card comes back.
        conn.execute(
            "UPDATE move_proposals SET status = 'approved' WHERE source_rel_path = ?1",
            [&asked],
        )
        .expect("approve");
        let summary = record_entries(&conn, 1, &dir, Some(&asked), &entries).expect("record");
        assert!(summary.contains("resolved while the sort ran"), "{summary}");
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM move_proposals WHERE source_rel_path = ?1 AND status = 'pending'",
                [&asked],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(pending, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A pending proposal whose file has left the inbox is resolved with its
    /// row on record, and one whose file is still there is left alone — the
    /// badge and the queue then agree with the disk.
    #[test]
    fn a_proposal_whose_file_is_gone_is_dismissed_with_its_row_on_record() {
        let dir = std::env::temp_dir().join(format!("classhub-vanished-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(INBOX_DIR)).expect("inbox");
        fs::write(dir.join(INBOX_DIR).join("still-here.pdf"), "x").expect("file");

        // A file the pass cannot stat — its folder is unreadable — is not gone.
        let locked = dir.join(INBOX_DIR).join("locked");
        fs::create_dir_all(&locked).expect("locked");
        fs::write(locked.join("unreadable.pdf"), "x").expect("file");

        let conn = crate::db::memory_db();
        for name in ["still-here.pdf", "sorted-by-hand.pdf", "locked/unreadable.pdf"] {
            upsert_proposal(
                &conn,
                1,
                "canvas",
                &format!("{INBOX_DIR}/{name}"),
                &format!("Slides/{name}"),
                "Canvas files it under \"Slides\"",
                None,
            )
            .expect("propose");
        }
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("lock");
        }
        let pass = dismiss_vanished(&conn, 1, &dir);
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("unlock");
        }
        pass.expect("pass");

        let status = |name: &str| -> String {
            conn.query_row(
                "SELECT status FROM move_proposals WHERE source_rel_path = ?1",
                [format!("{INBOX_DIR}/{name}")],
                |r| r.get(0),
            )
            .expect("row")
        };
        assert_eq!(status("still-here.pdf"), "pending");
        assert_eq!(status("sorted-by-hand.pdf"), "dismissed");
        assert_eq!(
            status("locked/unreadable.pdf"),
            "pending",
            "a file that cannot be read was treated as gone"
        );
        let audit: String = conn
            .query_row(
                "SELECT payload FROM audit_log WHERE action = 'sort.proposal_vanished'",
                [],
                |r| r.get(0),
            )
            .expect("one audit row");
        let payload: serde_json::Value = serde_json::from_str(&audit).expect("json");
        assert_eq!(payload["sourceRelPath"], format!("{INBOX_DIR}/sorted-by-hand.pdf"));
        assert_eq!(payload["destRelPath"], "Slides/sorted-by-hand.pdf");
        assert_eq!(payload["proposedBy"], "canvas");

        // A folder that is not there is not a folder every file has left.
        let _ = fs::remove_dir_all(&dir);
        dismiss_vanished(&conn, 1, &dir).expect("pass");
        assert_eq!(status("still-here.pdf"), "pending");
    }

    /// Two processes share the table: a row the pass selected as pending can be
    /// approved by the other one before the write lands — the approve is what
    /// moved the file — and the dismissal must then neither overwrite the
    /// approval nor leave a "vanished" row contradicting the real move.
    #[test]
    fn a_row_the_other_process_resolved_meanwhile_is_left_alone() {
        let conn = crate::db::memory_db();
        upsert_proposal(
            &conn,
            1,
            "sort_job",
            &format!("{INBOX_DIR}/deck.pdf"),
            "Slides/deck.pdf",
            "because",
            Some("high"),
        )
        .expect("propose");
        let id: i64 = conn
            .query_row("SELECT id FROM move_proposals", [], |r| r.get(0))
            .expect("id");
        // Selected as vanished, then approved elsewhere before the write.
        conn.execute(
            "UPDATE move_proposals SET status = 'approved' WHERE id = ?1",
            [id],
        )
        .expect("approve");
        let dismissed = dismiss_rows(&conn, vec![(id, json!({ "proposalId": id }))])
            .expect("dismiss");
        assert_eq!(dismissed, 0);
        let status: String = conn
            .query_row("SELECT status FROM move_proposals WHERE id = ?1", [id], |r| r.get(0))
            .expect("status");
        assert_eq!(status, "approved", "the approval was overwritten");
        let audit_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'sort.proposal_vanished'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(audit_rows, 0, "a vanished row was written for a move that happened");
    }
}
