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

use crate::db::{EXTRACTS_DIR, INBOX_DIR, WEEKS_DIR, emit_hub_change, notify, now, with_conn};
use crate::scanner::APP_MANAGED_DIRS;
use crate::units::WeekReading;

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
    /// chat | sort_job | canvas | by_name — the last a file named for a week,
    /// proposed into its week folder from its Materials row (SPEC §10).
    pub source: String,
    pub created_at: i64,
    /// SPEC §10: on a Canvas card whose destination names a week — in the
    /// file's name, a module the course reads as a week, or Canvas's own
    /// folder — the week folder as a second destination, derived on every
    /// read (`week_alternative`) and never stored. Absent on every other card.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alternative: Option<WeekAlternative>,
}

/// The week folder a Canvas card also offers (SPEC §10): where the file
/// would be proposed from its row once it landed where Canvas put it, read a
/// step earlier so it reaches the week folder in one approval instead of two.
#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct WeekAlternative {
    pub week: i64,
    pub dest_rel_path: String,
    /// The by-name card's own words: which reading found the week, and which
    /// division counts the file under that folder.
    pub reasoning: String,
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

/// `name.pdf` → `name (2).pdf` when the folder already holds that name.
///
/// Every path that puts a file in the inbox goes through this, so no ingestion
/// route can overwrite what another one left there waiting for approval;
/// `beside_existing` asks the same question of a week folder, where the answer
/// is a proposed destination that approval re-checks before any rename.
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

/// SPEC §10: a file named for a week the course declares, proposed into that
/// week's folder from its row in Materials. Applied's Week 2 deck sits under
/// `Slides/`, where Canvas filed it, and Part I reads it only once it is under
/// `Weeks/Week 02/` (SPEC §8.5); Canvas's placement outranks an automatic sort
/// (SPEC §7.2), so the proposal exists because the row was clicked. Approval
/// is the ordinary move, extract and all. A folder named for a week files its
/// contents the same way, one card per file (`folder_filing`); a file named
/// for a module files under that week where the course reads its modules as
/// weeks (`units::modules_read_as_weeks`), and the card says so.
pub fn file_under_week(app: &AppHandle, class_id: i64, rel_path: &str) -> Result<WeekFiling> {
    // The moves land one by one, so a folder's batch that stops part-way
    // has still moved what came before: those are announced with their
    // Undo before the error is raised, or the way back would die with it.
    let mut moved = Vec::new();
    let filed = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        filing_with(conn, class_id, &class_dir, rel_path, &mut moved)
    });
    let filed = match filed {
        Ok(filed) => filed,
        Err(e) => {
            if !moved.is_empty() {
                emit_hub_change(app, "proposals");
                emit_hub_change(app, "files");
                let count = moved.len();
                notify(
                    app,
                    format!("Filed {count} of the folder's files before it stopped — the rest stayed"),
                    moved,
                    Some(class_id),
                );
            }
            return Err(e);
        }
    };
    emit_hub_change(app, "proposals");
    emit_hub_change(app, "files");
    let text = match filed.moved {
        1 => format!("Filed {} under {}/", filed.name, folder_of(&filed.dest_rel)),
        n => format!("Filed {n} files under {}/", filed.dest_rel),
    };
    notify(app, text, filed.audit_ids.clone(), Some(class_id));
    Ok(filed)
}

/// What a filing click did (SPEC §10): where it landed — a file's own
/// destination, or a folder's under the week folder — how many files moved,
/// one for a file and every file under a folder, and the audit rows the
/// notice's `Undo` reverses.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeekFiling {
    pub dest_rel: String,
    pub moved: usize,
    pub audit_ids: Vec<i64>,
    /// The file's or folder's own name, for the notice.
    pub name: String,
}

/// The folder half of a class-relative path, for a notice.
pub(crate) fn folder_of(rel_path: &str) -> &str {
    rel_path.rsplit_once('/').map(|(dir, _)| dir).unwrap_or("")
}

fn week_filing(conn: &Connection, class_id: i64, class_dir: &Path, rel_path: &str) -> Result<WeekFiling> {
    filing_with(conn, class_id, class_dir, rel_path, &mut Vec::new())
}

/// `week_filing` with the audit rows of the moves it made so far collected
/// into `moved` as each lands, so a caller sees them when a later move fails.
fn filing_with(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    rel_path: &str,
    moved: &mut Vec<i64>,
) -> Result<WeekFiling> {
    let source_rel = clean_rel(rel_path)?;
    // An inbox file is the sorter's, and Canvas may have placed it: the row
    // never offers one, and a caller that asked would be replacing a Canvas
    // card by a route the record does not argue for (`upsert_proposal`).
    if source_rel == INBOX_DIR || source_rel.starts_with(&format!("{INBOX_DIR}/")) {
        bail!("'{source_rel}' is in the inbox — its card there is the way to file it");
    }
    let name = Path::new(&source_rel)
        .file_name()
        .and_then(|n| n.to_str())
        .context("the path names no file")?;
    let source_abs = class_dir.join(&source_rel);
    // The walk shows no symlink and follows none (SPEC §7), so no row offers
    // one; a filing that followed it would enumerate the link's target and
    // propose moving what sits outside the class folder.
    if fs::symlink_metadata(&source_abs).is_ok_and(|m| m.file_type().is_symlink()) {
        bail!("'{source_rel}' is a symlink — the tree shows none, and filing follows none");
    }
    if source_abs.is_dir() {
        return folder_filing(conn, class_id, class_dir, &source_rel, name, moved);
    }
    if !source_abs.is_file() {
        bail!("'{source_rel}' is not on disk — rescan the class");
    }
    // The one reading the walk also takes (`units::named_week_reading`), so
    // the row's offer and this destination cannot part; the reason says which
    // reading it was.
    let modules_are_weeks = crate::units::modules_read_as_weeks(conn, class_id)?;
    let (week, reading) = match crate::units::named_week_reading(name, modules_are_weeks) {
        Some((week, reading)) => (week, reading_words(week, reading)),
        // A module this course does not read as a week: a row still offering
        // it was drawn before a scan recorded the course's own modules.
        None => match crate::units::module_in_name(name) {
            Some(module) => bail!(
                "'{name}' names Module {module}, and this course does not read a module as a \
                 week — rescan to refresh the row"
            ),
            None => bail!("'{name}' carries no week in its name"),
        },
    };
    let (slot, folder_rel) = week_target(conn, class_id, &source_rel, name, week)?;
    let dest_rel = format!("{folder_rel}/{name}");
    validate_dest(class_dir, &source_rel, &dest_rel)?;
    refuse_held(conn, class_id, &source_rel, &dest_rel)?;
    let reasoning = week_reason(&reading, &folder_rel, &slot.unit_name);
    // The click is the move (SPEC §10): the record of it is an approved
    // by-name row, written in the move's own transaction, so the queue's
    // history reads as it did when the click wrote a card, and the audit
    // row is the same `sort.move`.
    let audit_id = move_file(
        conn,
        class_id,
        class_dir,
        &source_rel,
        &dest_rel,
        Recorded {
            proposal: ProposalRow::New { source: "by_name", reasoning: &reasoning },
            proposed_by: "by_name",
            confidence: None,
            action: "sort.move",
            extra: json!({ "reasoning": reasoning }),
        },
    )?;
    moved.push(audit_id);
    Ok(WeekFiling { dest_rel, moved: 1, audit_ids: vec![audit_id], name: name.to_string() })
}

/// A folder named for a week files what it holds: every file under it moves
/// to the folder's own name under the week folder, so the professor's
/// grouping survives and a division, which counts a file under its week
/// folder at any depth (SPEC §8.5), reads them all. Every destination is
/// validated before anything moves, so a collision names the file and moves
/// nothing; the moves are one batch, one notice, one `Undo`. `Weeks/` and a
/// week folder are where filing lands and are refused; so is a folder
/// holding no file.
fn folder_filing(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    name: &str,
    moved: &mut Vec<i64>,
) -> Result<WeekFiling> {
    if source_rel == WEEKS_DIR {
        bail!("'{WEEKS_DIR}' is where lectures are filed, not something to file");
    }
    if source_rel
        .strip_prefix(&format!("{WEEKS_DIR}/"))
        .is_some_and(|rest| !rest.contains('/'))
    {
        bail!("'{name}' is a week folder — filing lands there");
    }
    let week = crate::units::week_in_name(name)
        .with_context(|| format!("'{name}' carries no week in its name"))?;
    let (slot, folder_rel) = week_target(conn, class_id, source_rel, name, week)?;
    let dest_folder = format!("{folder_rel}/{name}");
    let mut inside = Vec::new();
    collect_files(&class_dir.join(source_rel), "", &mut inside)?;
    if inside.is_empty() {
        bail!("'{name}' holds no file to file");
    }
    let moves: Vec<(String, String)> = inside
        .iter()
        .map(|path| (format!("{source_rel}/{path}"), format!("{dest_folder}/{path}")))
        .collect();
    for (from, to) in &moves {
        validate_dest(class_dir, from, to).with_context(|| format!("'{from}'"))?;
        refuse_held(conn, class_id, from, to)?;
    }
    // The week folder, not the destination folder: a nested file lands deeper
    // than `dest_folder`, and the week folder is the claim true of every card.
    let reasoning = week_reason(
        &format!("Its folder is named for Week {week}."),
        &folder_rel,
        &slot.unit_name,
    );
    // Each move is its own rename and transaction, as an approval's is; a
    // failure part-way — the other build holding the write lock past the busy
    // timeout — leaves the earlier moves made, each with its audit row, which
    // the notice's `Undo` reverses. The validation above is what keeps a
    // collision from being that failure.
    let batch = format!("by-name-{}-{class_id}", now());
    for (from, to) in &moves {
        let audit_id = move_file(
            conn,
            class_id,
            class_dir,
            from,
            to,
            Recorded {
                proposal: ProposalRow::New { source: "by_name", reasoning: &reasoning },
                proposed_by: "by_name",
                confidence: None,
                action: "sort.move",
                extra: json!({ "reasoning": reasoning, "batch": batch }),
            },
        )
        .with_context(|| format!("'{from}' — the files before it moved"))?;
        moved.push(audit_id);
    }
    Ok(WeekFiling {
        dest_rel: dest_folder,
        moved: moves.len(),
        audit_ids: moved.clone(),
        name: name.to_string(),
    })
}

/// What the queue already holds against a filing (SPEC §10). A pending card
/// for the source from another route — chat's, since a sort or Canvas card
/// names an inbox file — is the reader's own earlier ask, which a click must
/// not move the file from under without a word; and a pending card from any
/// source already heading for the destination — two folders sharing a leaf
/// name would each claim it — would fail only at the second approval. Both
/// are refused by name. A pending by-name card is left out of the first
/// check for the one writer that still makes one: the sync's card for a
/// loose file whose name reads a module as a week (`propose_loose_by_name`),
/// which a re-sync refreshes through `upsert_proposal` rather than refusing.
/// A row's click writes no card at all — it is the move, with an approved
/// row for the record — so nothing of its own is ever pending here.
fn refuse_held(conn: &Connection, class_id: i64, source_rel: &str, dest_rel: &str) -> Result<()> {
    let other_route: Option<String> = conn
        .query_row(
            "SELECT source FROM move_proposals
             WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'
               AND source != 'by_name'",
            params![class_id, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(route) = other_route {
        bail!("'{source_rel}' already has a card in the inbox from {route} — resolve it there first");
    }
    let claimant: Option<String> = conn
        .query_row(
            "SELECT source_rel_path FROM move_proposals
             WHERE class_id = ?1 AND dest_rel_path = ?2 AND status = 'pending'
               AND source_rel_path != ?3",
            params![class_id, dest_rel, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(other) = claimant {
        bail!("'{dest_rel}' is already proposed for '{other}'");
    }
    Ok(())
}

/// The week folder a filing lands in, refused for a week the course does not
/// declare and for a source already under that folder at any depth — the
/// division counts it there (SPEC §8.5), and lifting it out of a subfolder is
/// not a filing. The row hides the action by the same rule; this is the
/// enforcement.
fn week_target(
    conn: &Connection,
    class_id: i64,
    source_rel: &str,
    name: &str,
    week: i64,
) -> Result<(crate::units::WeekSlot, String)> {
    let slot = crate::units::slot_for_week(conn, class_id, week)?
        .with_context(|| format!("this course declares no week {week}"))?;
    let folder_rel = format!("{WEEKS_DIR}/{}", slot.folder);
    if source_rel.starts_with(&format!("{folder_rel}/")) {
        bail!("'{name}' is already under {folder_rel}");
    }
    Ok((slot, folder_rel))
}

/// The week folder a Canvas card also offers (SPEC §10). Where Canvas filed a
/// file is the professor's placement and stays the card's destination (SPEC
/// §7.2); this is the destination the file's row — or its folder's — would
/// offer once the file landed there, read a step earlier. The file's own name
/// first, through the reading the row takes (`units::named_week_reading`),
/// landing at `Weeks/<week folder>/<name>`; else the first folder on Canvas's
/// path whose name carries a week, landing under that folder's own name inside
/// the week folder, the way a folder's row files its contents. The first of
/// those readings that names a week the course declares decides, so a name
/// reading a week the course lacks yields to its folder, as the folder's own
/// row would file it; none when no reading does, and none for a destination
/// already under its week folder. Pure over the course's slots, so `sort_state` reads the rows once
/// per queue. The click that takes it is an approval, not a proposal:
/// `resolve_proposal` runs `validate_dest` and nothing else, so `sort_state`
/// withholds an alternative another pending card already claims — the half of
/// `refuse_held` a by-name card gets — and a collision is never offered.
fn week_alternative(
    slots: &[crate::units::WeekSlot],
    modules_are_weeks: bool,
    dest_rel: &str,
) -> Option<WeekAlternative> {
    let (folder, name) = dest_rel.rsplit_once('/').unwrap_or(("", dest_rel));
    let segments: Vec<&str> = folder.split('/').collect();
    let named = crate::units::named_week_reading(name, modules_are_weeks)
        .map(|(week, reading)| (week, reading_words(week, reading), name.to_string()));
    // `Weeks/`'s own child is a week folder, where filing lands, and carries
    // no week on the walk either (`scanner::walk_dir`); a Canvas folder
    // literally named `Weeks` must not yield a week folder nested in another.
    let from_folder = segments.iter().enumerate().filter_map(|(at, segment)| {
        if at == 1 && segments[0] == WEEKS_DIR {
            return None;
        }
        let week = crate::units::week_in_name(segment)?;
        Some((
            week,
            format!("Canvas files it under \"{segment}\", a folder named for Week {week}."),
            format!("{}/{name}", segments[at..].join("/")),
        ))
    });
    let (week, reading, tail, slot) = named
        .into_iter()
        .chain(from_folder)
        .find_map(|(week, reading, tail)| {
            let slot = slots.iter().find(|slot| slot.week == week)?;
            Some((week, reading, tail, slot))
        })?;
    let folder_rel = format!("{WEEKS_DIR}/{}", slot.folder);
    if dest_rel.starts_with(&format!("{folder_rel}/")) {
        return None;
    }
    Some(WeekAlternative {
        week,
        dest_rel_path: format!("{folder_rel}/{tail}"),
        reasoning: week_reason(&reading, &folder_rel, &slot.unit_name),
    })
}

/// An alternative whose week-folder destination a file of that name already
/// occupies lands at the next free name — Biostatistics re-uploaded its Week 3
/// coding notebook on 2026-09-08, and the earlier one already sat under the
/// week folder — the way a download lands in the inbox (`free_slot`), with the
/// reason saying the earlier file stays. Nothing is overwritten (SPEC §4), and
/// the click is not spent on `validate_dest`'s refusal. A row's own filing of a
/// file already in the tree onto a taken name stays refused: two copies in
/// the tree are the reader's to reconcile, while a re-upload is new material.
fn beside_existing(class_dir: &Path, mut alt: WeekAlternative) -> WeekAlternative {
    let Some((folder_rel, name)) = alt.dest_rel_path.rsplit_once('/') else {
        return alt;
    };
    // A shape the slot cannot name — a tail that is no file name — is left as
    // it came, the way the split above leaves a path with no folder.
    let Some(landed) = free_slot(&class_dir.join(folder_rel), name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return alt;
    };
    if landed == name {
        return alt;
    }
    // The cost is said with the landing: a division counts every file under
    // its week folder (SPEC §8.5), so its guide reads both copies until the
    // reader removes one — the app never deletes source material.
    alt.reasoning.push_str(&format!(
        " {folder_rel} already holds {name}, which stays; this one lands beside it as {landed}, \
         and the week's guide reads both until one is removed."
    ));
    alt.dest_rel_path = format!("{folder_rel}/{landed}");
    alt
}

/// A file Canvas keeps in no folder, proposed by its name where the name reads
/// a week the course declares (SPEC §7.2): the by-name card its row would
/// offer, written at staging time in the sort job's place, since the reading
/// is the row's own and costs nothing — Applied's Week 2 notebook came loose
/// on 2026-09-03 and spent a sort job to reach the folder its name said.
/// Canvas placed nothing, so no placement is overridden and no Canvas card is
/// replaced. Returns the card's destination when one was written; `None`
/// where the name reads no week, the course lacks it, or the destination is
/// taken or held, so the file stays loose for the sorter. A guard's refusal is
/// a decision and stays `None`; a database failure underneath it is not one
/// and propagates, as the write below would.
pub(crate) fn propose_loose_by_name(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    name: &str,
) -> Result<Option<String>> {
    let modules_are_weeks = crate::units::modules_read_as_weeks(conn, class_id)?;
    let Some((week, reading)) = crate::units::named_week_reading(name, modules_are_weeks) else {
        return Ok(None);
    };
    // The one place the week-folder rule lives, shared with the row's filing.
    let (slot, folder_rel) = match week_target(conn, class_id, source_rel, name, week) {
        Ok(target) => target,
        Err(e) => return refused_or_failed(e),
    };
    let dest_rel = format!("{folder_rel}/{name}");
    if let Err(e) = validate_dest(class_dir, source_rel, &dest_rel) {
        return refused_or_failed(e);
    }
    if let Err(e) = refuse_held(conn, class_id, source_rel, &dest_rel) {
        return refused_or_failed(e);
    }
    let reasoning = week_reason(
        &format!("Canvas keeps it in no folder. {}", reading_words(week, reading)),
        &folder_rel,
        &slot.unit_name,
    );
    if !upsert_proposal(conn, class_id, "by_name", source_rel, &dest_rel, &reasoning, None)? {
        return Ok(None);
    }
    Ok(Some(dest_rel))
}

// ---------------------------------------------------------------------------
// Filing without a card (SPEC §7.2, §10): where the sync puts a file

/// Where a file the sync staged goes, and why (SPEC §7.2).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoFiling {
    /// Moved on the spot: an observation, not a guess.
    Filed { dest_rel: String, reasoning: String },
    /// A module read as a week: the Canvas card with its week alternative, as
    /// before — Design Studio's `Module 2` spans two weeks, and that reading
    /// is knowingly wrong there (SPEC §10 step 7).
    ModuleCard,
    /// Canvas keeps it loose and its name reads no week: the sorter's.
    Loose,
}

/// The destination rule for a file the sync staged, pure over the course's
/// slots: `Weeks/<week folder>/<name>` when the file's own name carries a
/// week the course declares (the reading a Materials row takes); under the
/// folder's own name inside the week folder when Canvas's folder carries the
/// week (the alternative's folder reading); else Canvas's folder, mapped
/// onto the tree's vocabulary, as the card's destination was. A module the
/// course reads as a week keeps its card, and a loose file whose name reads
/// no week keeps its sort job. `canvas_dest` is the card's destination —
/// Canvas's folder with the name — or `None` for a file Canvas keeps loose.
pub(crate) fn auto_filing(
    slots: &[crate::units::WeekSlot],
    modules_are_weeks: bool,
    name: &str,
    canvas_dest: Option<&str>,
    canvas_reason: &str,
) -> AutoFiling {
    let week_folder = |week: i64| {
        slots
            .iter()
            .find(|slot| slot.week == week)
            .map(|slot| (format!("{WEEKS_DIR}/{}", slot.folder), slot.unit_name.as_str()))
    };
    match crate::units::named_week_reading(name, modules_are_weeks) {
        Some((week, WeekReading::Week)) => {
            if let Some((folder_rel, unit_name)) = week_folder(week) {
                let reading = match canvas_dest {
                    Some(_) => format!("{canvas_reason}, and its name carries Week {week}."),
                    None => format!("Canvas keeps it in no folder. Its name carries Week {week}."),
                };
                return AutoFiling::Filed {
                    dest_rel: format!("{folder_rel}/{name}"),
                    reasoning: week_reason(&reading, &folder_rel, unit_name),
                };
            }
        }
        Some((_, WeekReading::Module)) => return AutoFiling::ModuleCard,
        None => {}
    }
    let Some(canvas_dest) = canvas_dest else {
        return AutoFiling::Loose;
    };
    let folder = folder_of(canvas_dest);
    let segments: Vec<&str> = folder.split('/').collect();
    for (at, segment) in segments.iter().enumerate() {
        if at == 1 && segments[0] == WEEKS_DIR {
            break;
        }
        let Some(week) = crate::units::week_in_name(segment) else {
            continue;
        };
        if let Some((folder_rel, unit_name)) = week_folder(week) {
            return AutoFiling::Filed {
                dest_rel: format!("{folder_rel}/{}/{name}", segments[at..].join("/")),
                reasoning: week_reason(
                    &format!("{canvas_reason}, a folder named for Week {week}."),
                    &folder_rel,
                    unit_name,
                ),
            };
        }
    }
    AutoFiling::Filed {
        dest_rel: canvas_dest.to_string(),
        reasoning: format!("{canvas_reason}."),
    }
}

/// A file the sync placed, moved on the spot (SPEC §7.2): an approved Canvas
/// row for the record and the `canvas.filed` audit row under `batch`, whose
/// undo returns the file to the inbox with the card pending. A destination a
/// file of that name already occupies is taken at the next free name, the
/// way a download lands in the inbox.
pub(crate) fn file_now(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
    reasoning: &str,
    batch: &str,
) -> Result<(String, i64)> {
    let dest_rel = beside_taken(class_dir, dest_rel);
    validate_dest(class_dir, source_rel, &dest_rel)?;
    // A card another route holds for the file — chat's, or a sort the reader
    // asked for — is the reader's own ask; the sync's placement does not walk
    // over it. Its own Canvas card is the row this move resolves.
    let other_route: Option<String> = conn
        .query_row(
            "SELECT source FROM move_proposals
             WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'
               AND source NOT IN ('canvas', 'by_name')",
            params![class_id, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(route) = other_route {
        bail!("'{source_rel}' has a card from {route} — left to it");
    }
    let claimant: Option<String> = conn
        .query_row(
            "SELECT source_rel_path FROM move_proposals
             WHERE class_id = ?1 AND dest_rel_path = ?2 AND status = 'pending'
               AND source_rel_path != ?3",
            params![class_id, dest_rel, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(other) = claimant {
        bail!("'{dest_rel}' is already proposed for '{other}'");
    }
    // The card the sync itself wrote earlier, if one is waiting, becomes the
    // record of this move rather than a second row — written, like a fresh
    // row, inside the move's own transaction.
    let waiting: Option<i64> = conn
        .query_row(
            "SELECT id FROM move_proposals
             WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'",
            params![class_id, source_rel],
            |row| row.get(0),
        )
        .optional()?;
    let proposal = match waiting {
        Some(id) => ProposalRow::Waiting { id, reasoning },
        None => ProposalRow::New { source: "canvas", reasoning },
    };
    let audit_id = move_file(
        conn,
        class_id,
        class_dir,
        source_rel,
        &dest_rel,
        Recorded {
            proposal,
            proposed_by: "canvas",
            confidence: None,
            action: "canvas.filed",
            extra: json!({ "reasoning": reasoning, "batch": batch }),
        },
    )?;
    Ok((dest_rel, audit_id))
}

/// The next free name at a destination a file already occupies (`free_slot`),
/// or the destination itself.
fn beside_taken(class_dir: &Path, dest_rel: &str) -> String {
    let Some((folder_rel, name)) = dest_rel.rsplit_once('/') else {
        return dest_rel.to_string();
    };
    match free_slot(&class_dir.join(folder_rel), name).file_name() {
        Some(landed) => format!("{folder_rel}/{}", landed.to_string_lossy()),
        None => dest_rel.to_string(),
    }
}

/// A filing guard's refusal leaves a loose file to the sorter; a database
/// failure raised underneath the same guard is not a refusal and propagates,
/// so a busy timeout is never reported as "the name carried no week".
fn refused_or_failed<T>(e: anyhow::Error) -> Result<Option<T>> {
    if e.downcast_ref::<rusqlite::Error>().is_some() {
        Err(e)
    } else {
        Ok(None)
    }
}

/// The words a by-name card gives the reading that found its week (SPEC §10).
fn reading_words(week: i64, reading: WeekReading) -> String {
    match reading {
        WeekReading::Week => format!("Its name carries Week {week}."),
        WeekReading::Module => format!(
            "Its name carries Module {week}, and this course divides itself into weeks, \
             so Module {week} is Week {week}."
        ),
    }
}

/// How every by-name reason closes: the week folder the file lands in, and
/// the division that counts it there (SPEC §8.5).
fn week_reason(reading: &str, folder_rel: &str, unit_name: &str) -> String {
    format!("{reading} Under {folder_rel}, it counts among the sources of {unit_name}.")
}

/// The files under a folder, as paths relative to it, by the scanner's rule:
/// no dot-entries, no symlinks. Sorted, so the cards land in one order.
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || entry.file_type()?.is_symlink() {
            continue;
        }
        let rel = if prefix.is_empty() { name } else { format!("{prefix}/{name}") };
        if entry.file_type()?.is_dir() {
            collect_files(&entry.path(), &rel, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
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
    let placed = recorded_placements(conn, class_id)?;
    let inbox: Vec<InboxFile> = list_inbox(&class_dir)
        .into_iter()
        .filter(|f| {
            let key = format!("{INBOX_DIR}/{}", f.name);
            // Out of scope even for a manual run: Canvas, or the file's own
            // name, already said where this one belongs, and "Move to…" on
            // the card is the way to disagree with that.
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

/// Inbox files whose pending card came from a record rather than a guess:
/// Canvas's placement, or the file's own name read by the sync for a file
/// Canvas keeps loose (SPEC §7.2).
///
/// These are out of scope for a sort job however it was started. Canvas's
/// destination is where the professor filed the material, and a by-name card
/// is the week the name states, so asking a model to name a folder for either
/// spends a job to produce a guess that `upsert_proposal` would then have to
/// ignore. A by-name card the reader declined is not here — dismissal takes it
/// out of the automatic runs, and a manual Sort the inbox is what reads the
/// file by content after that.
fn recorded_placements(conn: &Connection, class_id: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT source_rel_path FROM move_proposals
         WHERE class_id = ?1 AND status = 'pending' AND source IN ('canvas', 'by_name')",
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
    let mut proposals = stmt
        .query_map([class_id], |row| {
            Ok(Proposal {
                id: row.get(0)?,
                source_rel_path: row.get(1)?,
                dest_rel_path: row.get(2)?,
                reasoning: row.get(3)?,
                confidence: row.get(4)?,
                source: row.get(5)?,
                created_at: row.get(6)?,
                alternative: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Read from the course's rows each time, never from the card: a rescan
    // that records a module turns the module reading off, and a renamed
    // week's folder follows its name (SPEC §10).
    if proposals.iter().any(|p| p.source == "canvas") {
        let modules_are_weeks = crate::units::modules_read_as_weeks(conn, class_id)?;
        let slots = crate::units::week_slots(conn, class_id)?;
        // The queue's claims, as `refuse_held` reads them for a by-name card:
        // a destination another pending card already heads for — a by-name
        // card from a Materials row for a same-named file in the tree — is
        // not offered again, since the second approval could only fail on it.
        let mut taken: HashSet<String> =
            proposals.iter().map(|p| p.dest_rel_path.clone()).collect();
        for card in proposals.iter_mut().filter(|p| p.source == "canvas") {
            card.alternative = week_alternative(&slots, modules_are_weeks, &card.dest_rel_path)
                .map(|alt| beside_existing(&class_dir, alt))
                .filter(|alt| taken.insert(alt.dest_rel_path.clone()));
        }
    }
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
/// row instead of stacking. The one path all four producers take, so the rule
/// cannot drift between them.
///
/// A Canvas card and a by-name card are the records the sort job defers to.
/// Where Canvas filed a file is an *observation* — the professor put it in that
/// folder — and a by-name card is the week the file's own name states, while a
/// sort job's destination is an *inference* from a filename and a tree. Letting
/// the guess replace the record is how a slide deck Canvas had placed under
/// `Slides/` ended up proposed for a `Week 1/Slides/` that nobody had said
/// existed; a sort run started before the sync wrote a by-name card could
/// answer for that file too, and its entry is kept out the same way.
///
/// A chat proposal is the reader asking, so it replaces a Canvas row. A by-name
/// proposal has two writers, and neither replaces a placement: a Materials
/// row's click takes a file in the tree, and `week_filing` refuses an inbox
/// source before this is reached; the sync's own card for a file Canvas keeps
/// loose (`propose_loose_by_name`) does take an inbox source, and `refuse_held`
/// turns it away while a pending card from another route holds that source.
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
        Some("canvas" | "by_name") if source == "sort_job" => Ok(false),
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
    let moved = with_conn(app, |conn| approve_in_conn(conn, proposal_id, approve, dest_override, None))?;
    emit_hub_change(app, "proposals");
    if let Some(moved) = &moved {
        emit_hub_change(app, "files"); // the tree on disk changed
        notify(
            app,
            format!("Filed {} under {}/", moved.name, folder_of(&moved.dest_rel)),
            vec![moved.audit_id],
            Some(moved.class_id),
        );
        return Ok(format!("moved — {} → {}", moved.source_rel, moved.dest_rel));
    }
    Ok("left in place".to_string())
}

/// What an approval moved.
struct Moved {
    class_id: i64,
    source_rel: String,
    dest_rel: String,
    name: String,
    audit_id: i64,
}

/// One card's resolution: a dismissal parks the row, an approval moves the
/// file through `move_file`. `None` for a dismissal.
fn approve_in_conn(
    conn: &Connection,
    proposal_id: i64,
    approve: bool,
    dest_override: Option<String>,
    batch: Option<&str>,
) -> Result<Option<Moved>> {
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
        return Ok(None);
    }

    // Both branches go through clean_rel — the stored destination was
    // validated when recorded, but the approve path is the last gate
    // before an fs operation and re-checks everything it relies on.
    let dest_rel = match dest_override {
        Some(over) => clean_rel(&over)?,
        None => clean_rel(&proposed_dest)?,
    };
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let audit_id = move_file(
        conn,
        class_id,
        &class_dir,
        &source_rel,
        &dest_rel,
        Recorded {
            proposal: ProposalRow::Existing(proposal_id),
            proposed_by: &source,
            confidence: confidence.as_deref(),
            action: "sort.move",
            extra: json!({ "batch": batch }),
        },
    )?;
    let name = source_rel.rsplit('/').next().unwrap_or(&source_rel).to_string();
    Ok(Some(Moved { class_id, source_rel, dest_rel, name, audit_id }))
}

/// Approve all over a class's pending cards (SPEC §10): each the ordinary
/// approval with its audit row, under one batch and one `Undo`; a card whose
/// move fails is left pending and named.
pub fn approve_all(app: &AppHandle, class_id: i64) -> Result<crate::deadlines::BatchOutcome> {
    let batch = format!("approve-all-{}-{class_id}", now());
    let (outcome, audit_ids) = with_conn(app, |conn| approve_all_in_conn(conn, class_id, &batch))?;
    emit_hub_change(app, "proposals");
    if !outcome.approved.is_empty() {
        emit_hub_change(app, "files");
        let count = outcome.approved.len();
        let text = if count == 1 { "Filed 1 file".to_string() } else { format!("Filed {count} files") };
        notify(app, text, audit_ids, Some(class_id));
    }
    Ok(outcome)
}

/// Approve all on the connection: every pending card of the class in id
/// order, the outcome and the audit rows of the moves made.
fn approve_all_in_conn(
    conn: &Connection,
    class_id: i64,
    batch: &str,
) -> Result<(crate::deadlines::BatchOutcome, Vec<i64>)> {
    let ids: Vec<i64> = conn
        .prepare(
            "SELECT id FROM move_proposals
             WHERE class_id = ?1 AND status = 'pending' ORDER BY id",
        )?
        .query_map([class_id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut approved = Vec::new();
    let mut skipped = Vec::new();
    let mut audit_ids = Vec::new();
    for id in ids {
        match approve_in_conn(conn, id, true, None, Some(batch)) {
            Ok(Some(moved)) => {
                approved.push(id);
                audit_ids.push(moved.audit_id);
            }
            Ok(None) => {}
            Err(e) => skipped.push(format!("{e:#}")),
        }
    }
    Ok((crate::deadlines::BatchOutcome { approved, skipped }, audit_ids))
}

/// The `move_proposals` row a move writes, inside its own transaction, so a
/// move that fails leaves no row saying it happened (SPEC §5).
pub(crate) enum ProposalRow<'a> {
    /// A card the reader approved: set approved with the destination.
    Existing(i64),
    /// A move made without a card — a by-name click, a file the sync placed
    /// — gets an approved row for the record, so an undo has a row to put
    /// back to pending or dismissed.
    New { source: &'a str, reasoning: &'a str },
    /// A card the sync itself wrote earlier, now the record of this move
    /// rather than a second row.
    Waiting { id: i64, reasoning: &'a str },
}

/// What a move records beside its paths: the row it writes, who proposed
/// it, the audit action and anything else the payload carries — a reason,
/// a batch id.
pub(crate) struct Recorded<'a> {
    pub proposal: ProposalRow<'a>,
    pub proposed_by: &'a str,
    pub confidence: Option<&'a str>,
    /// `sort.move` for an approval or a by-name click, `canvas.filed` for a
    /// file the sync placed.
    pub action: &'a str,
    pub extra: serde_json::Value,
}

/// The move itself, the one path every filing takes (SPEC §10 step 4): the
/// last checks before an fs operation, folders created as proposed, a
/// same-volume rename, then the database side as one unit — on whose failure
/// the file and any moved extract artifacts go back where they were, so the
/// observable states are exactly "nothing happened" or "everything
/// happened". Returns the audit row's id, which the notice's `Undo` names.
pub(crate) fn move_file(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
    mut recorded: Recorded<'_>,
) -> Result<i64> {
    let src_abs = class_dir.join(source_rel);
    if !src_abs.is_file() {
        bail!("'{source_rel}' is no longer on disk — leave or dismiss this proposal");
    }
    validate_dest(class_dir, source_rel, dest_rel)?;

    // The folders this move creates ride its audit row, so an undo removes
    // exactly those, once empty, and never a folder that was already there.
    let created = missing_parents(class_dir, dest_rel);
    let dest_abs = class_dir.join(dest_rel);
    if let Some(parent) = dest_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&src_abs, &dest_abs)
        .with_context(|| format!("moving {source_rel} to {dest_rel}"))?;

    if !created.is_empty() {
        if let Some(into) = recorded.extra.as_object_mut() {
            into.insert("createdDirs".to_string(), json!(created));
        }
    }
    match record_move(conn, class_id, class_dir, source_rel, dest_rel, &recorded) {
        Ok(audit_id) => Ok(audit_id),
        Err(e) => {
            undo_artifact_moves(class_dir, source_rel, dest_rel);
            if let Err(undo) = fs::rename(&dest_abs, &src_abs) {
                return Err(e.context(format!(
                    "recording the move failed AND the file could not be moved \
                     back ({undo}) — it is on disk at {dest_rel}; rescan the class"
                )));
            }
            Err(e.context("recording the move failed — the file was moved back"))
        }
    }
}

/// The class-relative folders a move to `dest_rel` would create, outermost
/// first: every ancestor of the destination not yet on disk.
fn missing_parents(class_dir: &Path, dest_rel: &str) -> Vec<String> {
    let mut created = Vec::new();
    let mut path = String::new();
    let segments: Vec<&str> = dest_rel.split('/').collect();
    for segment in &segments[..segments.len().saturating_sub(1)] {
        path = if path.is_empty() { segment.to_string() } else { format!("{path}/{segment}") };
        if !class_dir.join(&path).exists() {
            created.push(path.clone());
        }
    }
    created
}

/// The whole DB side of a move in one transaction — stale-row cleanup, index
/// update, audit entry, proposal resolution — so a failure anywhere leaves no
/// partial commit and the caller can undo the rename.
fn record_move(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
    recorded: &Recorded<'_>,
) -> Result<i64> {
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
    // The row and the move commit together: a move that fails past here rolls
    // its row back with the rest, and a retry finds no row claiming it moved.
    let proposal_id = match recorded.proposal {
        ProposalRow::Existing(id) => {
            tx.execute(
                "UPDATE move_proposals
                 SET status = 'approved', dest_rel_path = ?1, resolved_at = ?2 WHERE id = ?3",
                params![dest_rel, now(), id],
            )?;
            id
        }
        ProposalRow::New { source, reasoning } => {
            tx.execute(
                "INSERT INTO move_proposals
                 (class_id, source_rel_path, dest_rel_path, reasoning, confidence,
                  source, status, created_at, resolved_at)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, 'approved', ?6, ?6)",
                params![class_id, source_rel, dest_rel, reasoning, source, now()],
            )?;
            tx.last_insert_rowid()
        }
        ProposalRow::Waiting { id, reasoning } => {
            tx.execute(
                "UPDATE move_proposals
                 SET dest_rel_path = ?1, reasoning = ?2, source = 'canvas',
                     status = 'approved', resolved_at = ?3
                 WHERE id = ?4",
                params![dest_rel, reasoning, now(), id],
            )?;
            id
        }
    };
    let mut payload = json!({
        "proposalId": proposal_id,
        "classId": class_id,
        "from": source_rel,
        "to": dest_rel,
        "proposedBy": recorded.proposed_by,
        "confidence": recorded.confidence,
    });
    if let (Some(into), Some(extra)) = (payload.as_object_mut(), recorded.extra.as_object()) {
        for (key, value) in extra {
            if !value.is_null() {
                into.insert(key.clone(), value.clone());
            }
        }
    }
    let audit_id = crate::db::audit(&tx, recorded.action, payload)?;
    tx.commit()?;
    // Only now: the database can no longer roll back, so the corpus note moves
    // against a record that already says it moved. Before the commit it stays
    // put, which is what keeps the caller's undo path — the transcript rename
    // and the extract artifacts — the whole of what a failure has to reverse.
    effects.apply();
    Ok(audit_id)
}

// ---------------------------------------------------------------------------
// Undo (SPEC §6): a move reversed from its audit row

/// The inverse of `sort.move` and `canvas.filed`: the file goes back through
/// the same rename, refused by name when its old path is taken or the file
/// has left the destination. Back in the tree its index row and its extract
/// follow it; back in the inbox — unindexed, as every inbox file is — its row
/// and its mirror go, and its card returns to pending so the reader can
/// redirect it or sort it by content. A by-name click's row is dismissed
/// instead, so its Materials row offers `File under Week NN` again. Writes
/// `undo.<action>` with the paths of its own move, which the write guard
/// reads as it reads any app move.
pub(crate) fn undo_move(
    conn: &Connection,
    action: &str,
    payload: &serde_json::Value,
    audit_id: i64,
) -> Result<crate::deadlines::Undone> {
    let class_id = payload["classId"].as_i64().context("the row names no class")?;
    // The paths come off a payload the app wrote, and go through the same
    // gate the forward move's do: `clean_rel` is the last check before an fs
    // operation on either side.
    let from = clean_rel(payload["from"].as_str().context("the row names no source")?)?;
    let to = clean_rel(payload["to"].as_str().context("the row names no destination")?)?;
    let (from, to) = (from.as_str(), to.as_str());
    let proposed_by = payload["proposedBy"].as_str().unwrap_or("");
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let at_abs = class_dir.join(to);
    if !at_abs.is_file() {
        bail!("'{to}' is no longer where the move put it");
    }
    let back_abs = class_dir.join(from);
    if back_abs.exists() {
        bail!("'{from}' is taken — something else sits there now");
    }
    for segment in from.split('/') {
        if segment.starts_with('.') {
            bail!("'{from}' is under a hidden folder");
        }
    }
    ensure_no_symlink_ancestors(&class_dir, from)?;
    if let Some(parent) = back_abs.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(&at_abs, &back_abs).with_context(|| format!("moving {to} back to {from}"))?;

    let into_inbox = from == INBOX_DIR || from.starts_with(&format!("{INBOX_DIR}/"));
    let recorded = (|| -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        if into_inbox {
            tx.execute(
                "DELETE FROM files WHERE class_id = ?1 AND rel_path = ?2",
                params![class_id, to],
            )?;
        } else {
            tx.execute(
                "DELETE FROM files WHERE class_id = ?1 AND rel_path = ?2",
                params![class_id, from],
            )?;
            update_index(&tx, class_id, &class_dir, to, from)?;
        }
        let effects = crate::lectures::refile_lecture(&tx, class_id, &class_dir, to, from)?;
        if let Some(proposal_id) = payload["proposalId"].as_i64() {
            let status = if proposed_by == "by_name" { "dismissed" } else { "pending" };
            tx.execute(
                "UPDATE move_proposals SET status = ?1, resolved_at = NULL WHERE id = ?2",
                params![status, proposal_id],
            )?;
        }
        crate::db::audit(
            &tx,
            &format!("undo.{action}"),
            json!({ "auditId": audit_id, "classId": class_id, "from": to, "to": from }),
        )?;
        tx.commit()?;
        effects.apply();
        Ok(())
    })();
    if let Err(e) = recorded {
        if !into_inbox {
            undo_artifact_moves(&class_dir, to, from);
        }
        if let Err(back) = fs::rename(&back_abs, &at_abs) {
            return Err(e.context(format!(
                "recording the undo failed AND the file could not be moved back ({back}) — \
                 it is on disk at {from}; rescan the class"
            )));
        }
        return Err(e.context("recording the undo failed — the file stays where the move put it"));
    }
    if into_inbox {
        crate::extract::remove_mirror(&class_dir, to);
    }
    // The folders the move created go once empty, innermost first; one that
    // holds something since stays, as `remove_dir` refuses it.
    let created: Vec<&str> = payload["createdDirs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    for dir in created.iter().rev() {
        let _ = fs::remove_dir(class_dir.join(dir));
    }
    let name = to.rsplit('/').next().unwrap_or(to);
    let back_to = match folder_of(from) {
        "" => "the class folder".to_string(),
        folder => format!("{folder}/"),
    };
    Ok(crate::deadlines::Undone {
        what: format!("Returned {name} to {back_to}"),
        class_id: Some(class_id),
    })
}

/// Best-effort reversal of `update_index`'s artifact renames, for the
/// failure path: each relocated extract file goes back to its source mirror.
fn undo_artifact_moves(class_dir: &Path, source_rel: &str, dest_rel: &str) {
    for suffix in crate::extract::MIRROR_SUFFIXES {
        let new = crate::extract::mirror_entry(class_dir, dest_rel, suffix);
        let old = crate::extract::mirror_entry(class_dir, source_rel, suffix);
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
        // The markdown extract plus any conversion twin and its sidecar mirror
        // the source rel_path — move them along so nothing goes stale.
        for suffix in crate::extract::MIRROR_SUFFIXES {
            let old = crate::extract::mirror_entry(class_dir, source_rel, suffix);
            if old.is_file() {
                let new = crate::extract::mirror_entry(class_dir, dest_rel, suffix);
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
        let md_ok = crate::extract::mirror_entry(class_dir, dest_rel, ".md").is_file();
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

    /// A by-name card is the week the file's own name states, and a sort run
    /// that started before the sync wrote it must not answer over it either.
    #[test]
    fn a_sort_job_does_not_replace_a_by_name_card() {
        let conn = crate::db::memory_db();
        propose(&conn, "by_name", "Weeks/Week 03/deck.pdf");
        propose(&conn, "sort_job", "Slides/deck.pdf");
        assert_eq!(
            held(&conn),
            ("Weeks/Week 03/deck.pdf".into(), "by_name".into(), 1),
            "a sort job overwrote the name's card, or stacked a second one"
        );
    }

    /// A manual Sort the inbox re-proposes everything except what a record
    /// already placed: a pending Canvas or by-name card keeps its file out of
    /// the prompt, and a declined by-name card puts it back in scope.
    #[test]
    fn a_manual_sort_leaves_a_recorded_placement_to_its_card() {
        let root = std::env::temp_dir().join(format!("classhub-manual-scope-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let class_dir = crate::scanner::class_dir(&conn, 4).expect("class dir");
        fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
        for name in ["CAI6734_Week3_Colab.ipynb", "deck.pdf", "notes.pdf"] {
            fs::write(class_dir.join(INBOX_DIR).join(name), "x").expect("inbox file");
        }
        let carded = format!("{INBOX_DIR}/CAI6734_Week3_Colab.ipynb");
        upsert_proposal(&conn, 4, "by_name", &carded, "Weeks/Week 03/CAI6734_Week3_Colab.ipynb", "name", None)
            .expect("by-name card");
        upsert_proposal(&conn, 4, "canvas", &format!("{INBOX_DIR}/deck.pdf"), "Slides/deck.pdf", "canvas", None)
            .expect("canvas card");
        let prompt = build_prompt(&conn, 4, true).expect("prompt").expect("something in scope");
        assert!(prompt.contains("_Inbox/notes.pdf"), "the uncarded file is in scope");
        assert!(!prompt.contains("Colab.ipynb") && !prompt.contains("deck.pdf"), "{prompt}");
        // Declined: the automatic runs skip it, a manual one reads it.
        conn.execute(
            "UPDATE move_proposals SET status = 'dismissed' WHERE source_rel_path = ?1",
            [&carded],
        )
        .expect("declined");
        let automatic = build_prompt(&conn, 4, false).expect("prompt").expect("notes.pdf");
        assert!(!automatic.contains("Colab.ipynb"));
        let manual = build_prompt(&conn, 4, true).expect("prompt").expect("in scope");
        assert!(manual.contains("Colab.ipynb"), "a declined card leaves the file to a manual sort");
        let _ = fs::remove_dir_all(&root);
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
    /// SPEC §10: a file named for a week is filed into the week's folder from
    /// its row by one click — the move, its `sort.move` row naming the
    /// division that reads the folder, and an approved by-name row for the
    /// record; one already there, one named for a week the course lacks, and
    /// one named for none are refused.
    #[test]
    fn a_file_named_for_a_week_is_filed_into_its_week_folder() {
        let root = std::env::temp_dir().join(format!("classhub-week-filing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let class_dir = crate::scanner::class_dir(&conn, 4).expect("class dir");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I: Deep Learning', 1, 1, 8, 'syllabus')",
            [],
        )
        .expect("part");
        let deck = "Slides/CAI6734_Week2_Foundations.pdf";
        fs::create_dir_all(class_dir.join("Slides")).expect("slides");
        fs::write(class_dir.join(deck), "%PDF").expect("deck");
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (4, ?1, 'h', 4, 1, 'pdf')",
            [deck],
        )
        .expect("index row");

        let filed = week_filing(&conn, 4, &class_dir, deck).expect("filed");
        assert_eq!((filed.dest_rel.as_str(), filed.moved), ("Weeks/Week 02/CAI6734_Week2_Foundations.pdf", 1));
        assert_eq!(filed.name, "CAI6734_Week2_Foundations.pdf");
        let dest = filed.dest_rel;
        assert!(class_dir.join(&dest).is_file() && !class_dir.join(deck).exists(), "the click is the move");
        let (held_dest, source, status, reasoning): (String, String, String, String) = conn
            .query_row(
                "SELECT dest_rel_path, source, status, reasoning FROM move_proposals
                 WHERE class_id = 4 AND source_rel_path = ?1",
                [deck],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("row");
        assert_eq!((held_dest.as_str(), source.as_str(), status.as_str()), (dest.as_str(), "by_name", "approved"));
        assert!(reasoning.contains("Part I: Deep Learning"), "{reasoning}");
        let (action, payload): (String, String) = conn
            .query_row(
                "SELECT action, payload FROM audit_log WHERE id = ?1",
                [filed.audit_ids[0]],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("audit row");
        assert_eq!(action, "sort.move");
        assert!(payload.contains("\"proposedBy\":\"by_name\"") && payload.contains("\"reasoning\""), "{payload}");
        let indexed: String = conn
            .query_row("SELECT rel_path FROM files WHERE class_id = 4", [], |r| r.get(0))
            .expect("index");
        assert_eq!(indexed, dest, "the index row followed the file");

        // Already under its week folder: nothing to file.
        let err = week_filing(&conn, 4, &class_dir, &dest).err().expect("already there");
        assert!(format!("{err:#}").contains("already under"), "{err:#}");
        // A week the course does not declare, and a name carrying none.
        fs::write(class_dir.join("Slides/Week 17 wrap-up.pdf"), "%PDF").expect("week 17");
        let err = week_filing(&conn, 4, &class_dir, "Slides/Week 17 wrap-up.pdf").err().expect("no week 17");
        assert!(format!("{err:#}").contains("declares no week 17"), "{err:#}");
        fs::write(class_dir.join("Slides/deck.pdf"), "%PDF").expect("plain deck");
        let err = week_filing(&conn, 4, &class_dir, "Slides/deck.pdf").err().expect("no week in name");
        assert!(format!("{err:#}").contains("carries no week"), "{err:#}");

        // Two files, one name, one week: the second collides with the first
        // now filed, and is refused rather than moved beside it.
        fs::create_dir_all(class_dir.join("Readings")).expect("readings");
        fs::write(class_dir.join("Readings/CAI6734_Week2_Foundations.pdf"), "%PDF").expect("twin");
        let err = week_filing(&conn, 4, &class_dir, "Readings/CAI6734_Week2_Foundations.pdf")
            .err()
            .expect("the destination is taken");
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        assert!(class_dir.join("Readings/CAI6734_Week2_Foundations.pdf").is_file(), "refused, not moved");
        // Under the week folder at any depth: the division counts it there.
        fs::create_dir_all(class_dir.join("Weeks/Week 02/Extra")).expect("extra");
        fs::write(class_dir.join("Weeks/Week 02/Extra/CAI6734_Week2_Notes.pdf"), "%PDF").expect("nested");
        let err = week_filing(&conn, 4, &class_dir, "Weeks/Week 02/Extra/CAI6734_Week2_Notes.pdf")
            .err()
            .expect("nested under its folder");
        assert!(format!("{err:#}").contains("already under"), "{err:#}");
        // An inbox file is the sorter's, whatever its name says.
        fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
        fs::write(class_dir.join(format!("{INBOX_DIR}/CAI6734_Week2_Lab.pdf")), "%PDF").expect("inbox file");
        let err = week_filing(&conn, 4, &class_dir, &format!("{INBOX_DIR}/CAI6734_Week2_Lab.pdf"))
            .err()
            .expect("an inbox source");
        assert!(format!("{err:#}").contains("in the inbox"), "{err:#}");
        // A row clicked after the file was moved in Finder.
        fs::remove_file(class_dir.join("Slides/deck.pdf")).expect("gone");
        let err = week_filing(&conn, 4, &class_dir, "Slides/deck.pdf").err().expect("gone");
        assert!(format!("{err:#}").contains("not on disk"), "{err:#}");
        // A file another route's card holds is the reader's own ask, refused
        // by name rather than moved from under it.
        fs::write(class_dir.join("Slides/CAI6734_Week3_Deck.pdf"), "%PDF").expect("week 3 deck");
        conn.execute(
            "INSERT INTO move_proposals (class_id, source_rel_path, dest_rel_path, reasoning, source, status, created_at)
             VALUES (4, 'Slides/CAI6734_Week3_Deck.pdf', 'Decks/CAI6734_Week3_Deck.pdf', 'asked in chat', 'chat', 'pending', 0)",
            [],
        )
        .expect("chat card");
        let err = week_filing(&conn, 4, &class_dir, "Slides/CAI6734_Week3_Deck.pdf").err().expect("chat holds it");
        assert!(format!("{err:#}").contains("from chat"), "{err:#}");
        assert!(class_dir.join("Slides/CAI6734_Week3_Deck.pdf").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    /// SPEC §10: a folder named for a week files what it holds by one click —
    /// every file under it moved to the folder's own name inside the week
    /// folder as one batch, all or none on the validation — while `Weeks/`,
    /// a week folder and an empty folder are refused.
    #[test]
    fn a_folder_named_for_a_week_files_its_contents_as_one_batch() {
        let root = std::env::temp_dir().join(format!("classhub-folder-filing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let class_dir = crate::scanner::class_dir(&conn, 3).expect("class dir");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 3 — Data Quality', 3, '2026-09-03', 'syllabus')",
            [],
        )
        .expect("week");
        let folder = "Coding Material/Week 3 Coding Material";
        let seed = |class_dir: &Path| {
            for file in ["intro.Rmd", "intro.html", "R/sketchpad.R", "R/deep/helpers.R", ".DS_Store"] {
                let path = class_dir.join(folder).join(file);
                fs::create_dir_all(path.parent().expect("parent")).expect("dir");
                fs::write(path, "x").expect("file");
            }
        };
        seed(&class_dir);
        // A symlink inside the folder — to a file and to a folder outside the
        // class — is skipped the way the walk skips it, so neither moves.
        fs::write(root.join("outside.R"), "y <- 2").expect("outside file");
        fs::create_dir_all(root.join("outside-dir")).expect("outside dir");
        fs::write(root.join("outside-dir/inner.R"), "z <- 3").expect("outside inner");
        std::os::unix::fs::symlink(root.join("outside.R"), class_dir.join(folder).join("link.R")).expect("link");
        std::os::unix::fs::symlink(root.join("outside-dir"), class_dir.join(folder).join("Linked")).expect("dir link");

        let filed = week_filing(&conn, 3, &class_dir, folder).expect("filed");
        assert_eq!((filed.moved, filed.audit_ids.len()), (4, 4));
        let dest = filed.dest_rel;
        assert_eq!(dest, "Weeks/Week 03 — Data Quality/Week 3 Coding Material");
        for file in ["intro.Rmd", "intro.html", "R/sketchpad.R", "R/deep/helpers.R"] {
            assert!(class_dir.join(&dest).join(file).is_file(), "{file} moved");
            assert!(!class_dir.join(folder).join(file).exists(), "{file} left");
        }
        assert!(class_dir.join(folder).join("link.R").symlink_metadata().is_ok(), "the symlink stays");
        let mut stmt = conn
            .prepare(
                "SELECT source_rel_path, dest_rel_path, source, status, reasoning FROM move_proposals
                 WHERE class_id = 3 ORDER BY source_rel_path",
            )
            .expect("stmt");
        let rows: Vec<(String, String, String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
            .expect("rows")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(rows.len(), 4, "one row per file, none for the dot-entry or the symlinks: {rows:?}");
        // Two levels deep, the path inside the folder is kept whole.
        assert_eq!(rows[0].0, format!("{folder}/R/deep/helpers.R"));
        assert_eq!(rows[0].1, format!("{dest}/R/deep/helpers.R"));
        assert_eq!(rows[1].1, format!("{dest}/R/sketchpad.R"));
        assert_eq!(rows[3].0, format!("{folder}/intro.html"));
        assert_eq!(rows[3].1, format!("{dest}/intro.html"));
        assert!(rows.iter().all(|r| r.2 == "by_name" && r.3 == "approved"));
        // The reason names the week folder, which is true of the nested file too.
        assert!(
            rows[0].4.contains("named for Week 3")
                && rows[0].4.contains("Under Weeks/Week 03 — Data Quality,")
                && rows[0].4.contains("sources of Week 3 — Data Quality"),
            "{}",
            rows[0].4
        );
        // One batch: every audit row carries the same batch id.
        let batches: Vec<String> = conn
            .prepare("SELECT DISTINCT json_extract(payload, '$.batch') FROM audit_log WHERE action = 'sort.move'")
            .expect("stmt")
            .query_map([], |r| r.get(0))
            .expect("rows")
            .collect::<rusqlite::Result<_>>()
            .expect("rows");
        assert_eq!(batches.len(), 1, "{batches:?}");

        // A collision at one destination refuses the click and moves nothing.
        conn.execute("DELETE FROM move_proposals", []).expect("clear");
        fs::remove_dir_all(class_dir.join(&dest)).expect("clear the filed copies");
        fs::create_dir_all(class_dir.join(&dest)).expect("dest dir");
        fs::write(class_dir.join(&dest).join("intro.Rmd"), "already here").expect("collision");
        seed(&class_dir);
        let err = week_filing(&conn, 3, &class_dir, folder).err().expect("collision");
        assert!(format!("{err:#}").contains("intro.Rmd") && format!("{err:#}").contains("already exists"), "{err:#}");
        assert!(class_dir.join(folder).join("R/sketchpad.R").is_file(), "a refused click moved a file");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM move_proposals", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 0, "a refused click wrote rows");
        fs::remove_dir_all(class_dir.join(&dest)).expect("clear collision");
        fs::remove_dir_all(class_dir.join(WEEKS_DIR)).expect("clear the week folder");

        // A file under the folder holding chat's card is the reader's own ask,
        // named rather than retargeted; the card keeps its destination.
        conn.execute(
            "INSERT INTO move_proposals (class_id, source_rel_path, dest_rel_path, reasoning, source, status, created_at)
             VALUES (3, ?1, 'Slides/intro.Rmd', 'asked in chat', 'chat', 'pending', 0)",
            [format!("{folder}/intro.Rmd")],
        )
        .expect("chat card");
        let err = week_filing(&conn, 3, &class_dir, folder).err().expect("chat holds a file");
        assert!(format!("{err:#}").contains("intro.Rmd") && format!("{err:#}").contains("from chat"), "{err:#}");
        let (pending, chat_dest): (i64, String) = conn
            .query_row(
                "SELECT COUNT(*), MIN(dest_rel_path) FROM move_proposals WHERE status = 'pending'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("count");
        assert_eq!((pending, chat_dest.as_str()), (1, "Slides/intro.Rmd"), "the chat card was touched");
        conn.execute("DELETE FROM move_proposals", []).expect("clear");

        // Where filing lands is never filed, nor is the inbox itself; a folder
        // under its week folder is already counted; a folder with nothing in it
        // has nothing to file; a week the course lacks has no folder.
        let err = week_filing(&conn, 3, &class_dir, INBOX_DIR).err().expect("the inbox");
        assert!(format!("{err:#}").contains("in the inbox"), "{err:#}");
        fs::create_dir_all(class_dir.join("Labs/Week 17 Coding")).expect("week 17");
        fs::write(class_dir.join("Labs/Week 17 Coding/lab.R"), "x").expect("week 17 file");
        let err = week_filing(&conn, 3, &class_dir, "Labs/Week 17 Coding").err().expect("no week 17");
        assert!(format!("{err:#}").contains("declares no week 17"), "{err:#}");
        week_filing(&conn, 3, &class_dir, folder).expect("filed again");
        let err = week_filing(&conn, 3, &class_dir, WEEKS_DIR).err().expect("Weeks");
        assert!(format!("{err:#}").contains("where lectures are filed"), "{err:#}");
        let err = week_filing(&conn, 3, &class_dir, "Weeks/Week 03 — Data Quality").err().expect("week folder");
        assert!(format!("{err:#}").contains("is a week folder"), "{err:#}");
        let err = week_filing(&conn, 3, &class_dir, &dest).err().expect("already under");
        assert!(format!("{err:#}").contains("already under"), "{err:#}");
        fs::create_dir_all(class_dir.join("Labs/Week 3 empty")).expect("empty");
        let err = week_filing(&conn, 3, &class_dir, "Labs/Week 3 empty").err().expect("empty");
        assert!(format!("{err:#}").contains("holds no file"), "{err:#}");
        // A symlinked source, folder or file, is refused before either route.
        std::os::unix::fs::symlink(root.join("outside-dir"), class_dir.join("Labs/Week 3 Linked")).expect("source link");
        let err = week_filing(&conn, 3, &class_dir, "Labs/Week 3 Linked").err().expect("linked folder");
        assert!(format!("{err:#}").contains("is a symlink"), "{err:#}");
        std::os::unix::fs::symlink(root.join("outside.R"), class_dir.join("Labs/Week 3 notes.R")).expect("file link");
        let err = week_filing(&conn, 3, &class_dir, "Labs/Week 3 notes.R").err().expect("linked file");
        assert!(format!("{err:#}").contains("is a symlink"), "{err:#}");
        let _ = fs::remove_dir_all(&root);
    }

    /// SPEC §10: a file named for a module files under that week on a course
    /// whose divisions are weeks and that declares no module, with the reason
    /// saying so; a Part-numbered course reads no module as a week.
    #[test]
    fn a_module_in_a_name_is_a_week_where_the_course_reads_it_so() {
        let root = std::env::temp_dir().join(format!("classhub-module-filing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 3 — Data Quality', 3, '2026-09-03', 'syllabus')",
            [],
        )
        .expect("week");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I: Deep Learning', 1, 1, 8, 'syllabus')",
            [],
        )
        .expect("part");
        let deck = "Slides/Biostatistics_Module3_Slides_class.pptx";
        for class in [3, 4] {
            let class_dir = crate::scanner::class_dir(&conn, class).expect("class dir");
            fs::create_dir_all(class_dir.join("Slides")).expect("slides");
            fs::write(class_dir.join(deck), "PK").expect("deck");
        }

        let biostat = crate::scanner::class_dir(&conn, 3).expect("class dir");
        let filed = week_filing(&conn, 3, &biostat, deck).expect("proposed");
        assert_eq!(filed.dest_rel, "Weeks/Week 03 — Data Quality/Biostatistics_Module3_Slides_class.pptx");
        let reasoning: String = conn
            .query_row(
                "SELECT reasoning FROM move_proposals WHERE class_id = 3 AND source_rel_path = ?1",
                [deck],
                |r| r.get(0),
            )
            .expect("row");
        assert!(reasoning.starts_with("Its name carries Module 3, and this course divides itself into weeks"), "{reasoning}");

        // A folder named for a module is a module's folder, not a filing, even
        // on the course that reads a module-named file as a week.
        fs::create_dir_all(biostat.join("Module 1")).expect("module folder");
        fs::write(biostat.join("Module 1/notes.pdf"), "%PDF").expect("module file");
        let err = week_filing(&conn, 3, &biostat, "Module 1").err().expect("a module folder");
        assert!(format!("{err:#}").contains("carries no week"), "{err:#}");

        let applied = crate::scanner::class_dir(&conn, 4).expect("class dir");
        let err = week_filing(&conn, 4, &applied, deck).err().expect("a Part course");
        assert!(format!("{err:#}").contains("does not read a module as a week"), "{err:#}");
        let _ = fs::remove_dir_all(&root);
    }
    fn slot(week: i64, folder: &str, unit_name: &str) -> crate::units::WeekSlot {
        crate::units::WeekSlot {
            week,
            folder: folder.into(),
            unit_id: week,
            unit_name: unit_name.into(),
            unit_kind: "week".into(),
            meets_on: None,
        }
    }

    /// SPEC §10: a Canvas card whose destination names a week offers the
    /// week folder — the file's own reading first, else Canvas's folder's —
    /// and none for a week the course lacks or a destination already there.
    #[test]
    fn a_canvas_card_offers_the_week_folder_its_name_or_its_folder_reads() {
        let slots = [
            slot(1, "Week 01 — Introduction", "Week 1 — Introduction"),
            slot(3, "Week 03 — Data Quality", "Week 3 — Data Quality"),
            slot(4, "Week 04 — Probability", "Week 4 — Probability"),
        ];
        let alt = week_alternative(&slots, true, "Reading Material/Week 4 Sampling.pdf").expect("a week in the name");
        assert_eq!(
            alt,
            WeekAlternative {
                week: 4,
                dest_rel_path: "Weeks/Week 04 — Probability/Week 4 Sampling.pdf".into(),
                reasoning: "Its name carries Week 4. Under Weeks/Week 04 — Probability, it counts \
                            among the sources of Week 4 — Probability."
                    .into(),
            }
        );
        // A module, read as a week only where the course says so.
        let deck = "Slides/Biostatistics_Module3_Slides_class.pptx";
        let alt = week_alternative(&slots, true, deck).expect("a module read as a week");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 03 — Data Quality/Biostatistics_Module3_Slides_class.pptx");
        assert!(
            alt.reasoning.starts_with("Its name carries Module 3, and this course divides itself into weeks"),
            "{}",
            alt.reasoning
        );
        assert_eq!(week_alternative(&slots, false, deck), None, "a course that reads no module as a week");
        // Canvas's folder carries the week: the file keeps that folder's name
        // under the week folder, as a folder's row files its contents.
        let alt = week_alternative(&slots, true, "Week 1 - Introduction/Introduction.pdf").expect("a week on the folder");
        assert_eq!(
            alt,
            WeekAlternative {
                week: 1,
                dest_rel_path: "Weeks/Week 01 — Introduction/Week 1 - Introduction/Introduction.pdf".into(),
                reasoning: "Canvas files it under \"Week 1 - Introduction\", a folder named for Week 1. \
                            Under Weeks/Week 01 — Introduction, it counts among the sources of Week 1 — \
                            Introduction."
                    .into(),
            }
        );
        let alt = week_alternative(&slots, true, "Coding Material/Week 3 Coding Material/Intro.Rmd").expect("nested");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 03 — Data Quality/Week 3 Coding Material/Intro.Rmd");
        // A module-named folder is a module's folder, on the card as on the
        // row, even where the course reads a module-named file as a week.
        assert_eq!(week_alternative(&slots, true, "Module 3/deck.pptx"), None);
        // The outermost week-bearing folder decides, and everything inside it
        // travels, as that folder's row would file it.
        let alt = week_alternative(&slots, true, "Week 1 Materials/Week 3 Slides/x.pdf").expect("the outermost");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 01 — Introduction/Week 1 Materials/Week 3 Slides/x.pdf");
        // The file's own week wins over its folder's.
        let alt = week_alternative(&slots, true, "Week 1 - Introduction/Week 3 reading.pdf").expect("the file first");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 03 — Data Quality/Week 3 reading.pdf");
        // A name reading a week the course lacks yields to its folder, as the
        // folder's own row would file it; so does a folder reading one.
        let alt = week_alternative(&slots, true, "Week 1 - Introduction/Week 9 reading.pdf").expect("the folder's week");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 01 — Introduction/Week 1 - Introduction/Week 9 reading.pdf");
        let alt = week_alternative(&slots, true, "Week 9 Materials/Week 3 Slides/x.pdf").expect("the inner folder's week");
        assert_eq!(alt.dest_rel_path, "Weeks/Week 03 — Data Quality/Week 3 Slides/x.pdf");
        // A week the course lacks with nothing to yield to, a name and folder
        // carrying none, a destination already under its week folder at any
        // depth, no weeks.
        assert_eq!(week_alternative(&slots, true, "Reading Material/Week 9 reading.pdf"), None);
        assert_eq!(week_alternative(&slots, true, "AI Design Project/Guidelines.pdf"), None);
        assert_eq!(week_alternative(&slots, true, "Weeks/Week 03 — Data Quality/Week 3 reading.pdf"), None);
        assert_eq!(week_alternative(&slots, true, "Weeks/Week 03 — Data Quality/Slides/Week 3 reading.pdf"), None);
        // A Canvas folder named `Weeks`: its child is a week folder on the
        // walk's rule and reads none, so nothing nests one week folder in another.
        assert_eq!(week_alternative(&slots, true, "Weeks/Week 3/notes.pdf"), None);
        assert_eq!(week_alternative(&[], true, "Reading Material/Week 4 Sampling.pdf"), None);
    }

    /// SPEC §10: the queue derives the alternative on a Canvas card and on no
    /// other, from the course's rows as they stand.
    #[test]
    fn the_queue_carries_the_alternative_on_a_canvas_card_alone() {
        let root = std::env::temp_dir().join(format!("classhub-card-alternative-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 4 — Probability', 4, '2026-09-10', 'syllabus')",
            [],
        )
        .expect("week");
        let class_dir = crate::scanner::class_dir(&conn, 3).expect("class dir");
        fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
        for name in ["Week 4 Sampling.pdf", "Week 4 Notes.pdf", "Biostatistics_Module4_Slides_class.pptx"] {
            fs::write(class_dir.join(INBOX_DIR).join(name), "%PDF").expect("inbox file");
        }
        let card = |source: &str, name: &str, dest: &str, confidence: Option<&str>| {
            upsert_proposal(&conn, 3, source, &format!("{INBOX_DIR}/{name}"), dest, "because", confidence)
                .expect("proposed")
        };
        card("canvas", "Week 4 Sampling.pdf", "Reading Material/Week 4 Sampling.pdf", None);
        card("sort_job", "Week 4 Notes.pdf", "Slides/Week 4 Notes.pdf", Some("high"));
        card("canvas", "Biostatistics_Module4_Slides_class.pptx", "Slides/Biostatistics_Module4_Slides_class.pptx", None);

        let state = sort_state(&conn, 3).expect("state");
        let by_name: std::collections::HashMap<&str, &Proposal> =
            state.proposals.iter().map(|p| (p.source_rel_path.as_str(), p)).collect();
        let reading = by_name["_Inbox/Week 4 Sampling.pdf"].alternative.as_ref().expect("the reading's card");
        assert_eq!((reading.week, reading.dest_rel_path.as_str()), (4, "Weeks/Week 04 — Probability/Week 4 Sampling.pdf"));
        let deck = by_name["_Inbox/Biostatistics_Module4_Slides_class.pptx"].alternative.as_ref().expect("the deck's card");
        assert_eq!(deck.dest_rel_path, "Weeks/Week 04 — Probability/Biostatistics_Module4_Slides_class.pptx");
        assert!(by_name["_Inbox/Week 4 Notes.pdf"].alternative.is_none(), "a sort card offers none");

        // A scan that records a numbered module turns the module reading off;
        // the week word still reads.
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, source)
             VALUES (3, 2, 'module', 'Module 1', 1, 'syllabus')",
            [],
        )
        .expect("module");
        let state = sort_state(&conn, 3).expect("state");
        let alternatives: Vec<Option<i64>> = state
            .proposals
            .iter()
            .map(|p| p.alternative.as_ref().map(|a| a.week))
            .collect();
        assert_eq!(alternatives, vec![Some(4), None, None]);

        // A by-name card from a Materials row already heading for the week
        // destination: the Canvas card is not offered the same path, which
        // only the second approval could refuse.
        fs::create_dir_all(class_dir.join("Slides")).expect("slides");
        fs::write(class_dir.join("Slides/Week 4 Sampling.pdf"), "%PDF").expect("tree twin");
        upsert_proposal(
            &conn,
            3,
            "by_name",
            "Slides/Week 4 Sampling.pdf",
            "Weeks/Week 04 — Probability/Week 4 Sampling.pdf",
            "because",
            None,
        )
        .expect("by-name card");
        let state = sort_state(&conn, 3).expect("state");
        let reading = state
            .proposals
            .iter()
            .find(|p| p.source_rel_path == "_Inbox/Week 4 Sampling.pdf")
            .expect("the reading's card");
        assert!(reading.alternative.is_none(), "a destination another card claims is not offered");
        assert_eq!(state.proposals.len(), 4);
        let _ = fs::remove_dir_all(&root);
    }

    /// A Canvas alternative onto a name the week folder already holds — a
    /// re-upload, the Sept 8 shape — lands beside the earlier file at the next
    /// free name, said on the card, and approval would take it; the by-name
    /// row's filing of a tree file onto that name stays refused.
    #[test]
    fn a_canvas_alternative_lands_beside_an_earlier_file_of_its_name() {
        let root = std::env::temp_dir().join(format!("classhub-beside-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 3 — Data Quality', 3, '2026-09-03', 'syllabus')",
            [],
        )
        .expect("week");
        let class_dir = crate::scanner::class_dir(&conn, 3).expect("class dir");
        let week_dir = class_dir.join("Weeks/Week 03 — Data Quality/Week 3 Coding Material");
        fs::create_dir_all(&week_dir).expect("week folder");
        fs::write(week_dir.join("Intro.html"), "<html>earlier</html>").expect("earlier");
        fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
        fs::write(class_dir.join(INBOX_DIR).join("Intro.html"), "<html>later</html>").expect("later");
        let source = format!("{INBOX_DIR}/Intro.html");
        upsert_proposal(
            &conn,
            3,
            "canvas",
            &source,
            "Coding Material/Week 3 Coding Material/Intro.html",
            "because",
            None,
        )
        .expect("canvas card");

        let state = sort_state(&conn, 3).expect("state");
        let alt = state.proposals[0].alternative.as_ref().expect("an alternative");
        assert_eq!(
            alt.dest_rel_path,
            "Weeks/Week 03 — Data Quality/Week 3 Coding Material/Intro (2).html"
        );
        assert!(
            alt.reasoning.ends_with(
                "Weeks/Week 03 — Data Quality/Week 3 Coding Material already holds Intro.html, \
                 which stays; this one lands beside it as Intro (2).html, and the week's guide \
                 reads both until one is removed."
            ),
            "{}",
            alt.reasoning
        );
        validate_dest(&class_dir, &source, &alt.dest_rel_path).expect("approval would take it");

        // The landing runs before the claim filter, so the name that would be
        // taken is the suffixed one: a pending card already heading there
        // withholds the alternative altogether.
        fs::create_dir_all(class_dir.join("Coding Material")).expect("tree folder");
        fs::write(class_dir.join("Coding Material/Intro.html"), "z").expect("tree twin");
        upsert_proposal(
            &conn,
            3,
            "by_name",
            "Coding Material/Intro.html",
            "Weeks/Week 03 — Data Quality/Week 3 Coding Material/Intro (2).html",
            "because",
            None,
        )
        .expect("claimant");
        let state = sort_state(&conn, 3).expect("state");
        let canvas = state
            .proposals
            .iter()
            .find(|p| p.source_rel_path == source)
            .expect("the canvas card");
        assert!(canvas.alternative.is_none(), "a claimed suffixed destination is not offered");

        // The same name from a Materials row: refused, since two copies in the
        // tree are the reader's to reconcile.
        fs::write(class_dir.join("Coding Material/Week 3 Intro.html"), "x").expect("tree twin");
        fs::write(class_dir.join("Weeks/Week 03 — Data Quality/Week 3 Intro.html"), "y").expect("held name");
        let refused = match week_filing(&conn, 3, &class_dir, "Coding Material/Week 3 Intro.html") {
            Ok(_) => panic!("a taken name on a row's filing"),
            Err(e) => e,
        };
        assert!(format!("{refused:#}").contains("already exists"), "{refused:#}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A file Canvas keeps loose gets the by-name card its row would offer
    /// when its name reads a week the course declares — the week word, or a
    /// module where the course reads modules as weeks — and stays loose for
    /// the sorter otherwise: a name reading none, a week the course lacks, a
    /// module on a Part course, or a destination already on disk.
    #[test]
    fn a_loose_canvas_file_named_for_its_week_is_proposed_by_name() {
        let root = std::env::temp_dir().join(format!("classhub-loose-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I: Deep Learning', 1, 1, 8, 'syllabus')",
            [],
        )
        .expect("part");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 4 — Probability', 4, '2026-09-10', 'syllabus')",
            [],
        )
        .expect("week");
        let propose = |class: i64, name: &str| {
            let class_dir = crate::scanner::class_dir(&conn, class).expect("class dir");
            fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
            fs::write(class_dir.join(INBOX_DIR).join(name), "x").expect("landed");
            let source = format!("{INBOX_DIR}/{name}");
            let written = propose_loose_by_name(&conn, class, &class_dir, &source, name).expect("proposed");
            let card: Option<(String, String, String)> = conn
                .query_row(
                    "SELECT dest_rel_path, reasoning, source FROM move_proposals
                     WHERE class_id = ?1 AND source_rel_path = ?2 AND status = 'pending'",
                    params![class, source],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .expect("query");
            (written, card)
        };

        let (written, card) = propose(4, "CAI6734_Week3_Colab.ipynb");
        let (dest, reasoning, source) = card.expect("a by-name card");
        assert_eq!(written.as_deref(), Some("Weeks/Week 03/CAI6734_Week3_Colab.ipynb"));
        assert_eq!((dest.as_str(), source.as_str()), ("Weeks/Week 03/CAI6734_Week3_Colab.ipynb", "by_name"));
        assert_eq!(
            reasoning,
            "Canvas keeps it in no folder. Its name carries Week 3. Under Weeks/Week 03, it counts \
             among the sources of Part I: Deep Learning."
        );
        let (written, card) = propose(3, "Biostatistics_Module4_Slides_class.pptx");
        assert!(written.is_some());
        let (dest, reasoning, _) = card.expect("the module reading's card");
        assert_eq!(dest, "Weeks/Week 04 — Probability/Biostatistics_Module4_Slides_class.pptx");
        assert!(reasoning.starts_with("Canvas keeps it in no folder. Its name carries Module 4,"), "{reasoning}");

        for (class, name) in [
            (4, "Loose notes.pdf"),
            (4, "CAI6734_Week9_Beyond.pdf"),
            (4, "Module2_Colab.ipynb"),
            (3, "Weekly plan.pdf"),
        ] {
            let (written, card) = propose(class, name);
            assert!(written.is_none() && card.is_none(), "{name} stays loose for the sorter");
        }
        // A name the week folder already holds stays loose too.
        let class_dir = crate::scanner::class_dir(&conn, 4).expect("class dir");
        fs::create_dir_all(class_dir.join("Weeks/Week 03")).expect("week folder");
        fs::write(class_dir.join("Weeks/Week 03/CAI6734_Week3_Deck.pdf"), "x").expect("earlier");
        let (written, card) = propose(4, "CAI6734_Week3_Deck.pdf");
        assert!(written.is_none() && card.is_none(), "a taken name stays loose");
        // A pending Canvas card holds the source: refused, and the Canvas card
        // survives with its own destination — the promise of SPEC §10 step 7.
        let held = format!("{INBOX_DIR}/CAI6734_Week4_Held.pdf");
        fs::write(class_dir.join(&held), "x").expect("landed");
        upsert_proposal(&conn, 4, "canvas", &held, "Slides/CAI6734_Week4_Held.pdf", "canvas", None)
            .expect("canvas card");
        let (written, card) = propose(4, "CAI6734_Week4_Held.pdf");
        let (dest, _, source) = card.expect("the canvas card");
        assert!(written.is_none(), "a held source is refused");
        assert_eq!((dest.as_str(), source.as_str()), ("Slides/CAI6734_Week4_Held.pdf", "canvas"));
        // Another pending card already claims the week destination: stays loose.
        upsert_proposal(
            &conn,
            4,
            "by_name",
            "Slides/CAI6734_Week5_Twin.pdf",
            "Weeks/Week 05/CAI6734_Week5_Twin.pdf",
            "because",
            None,
        )
        .expect("claimant");
        let (written, card) = propose(4, "CAI6734_Week5_Twin.pdf");
        assert!(written.is_none() && card.is_none(), "a claimed destination stays loose");
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod auto_filing_tests {
    use super::*;

    fn slot(week: i64, folder: &str) -> crate::units::WeekSlot {
        crate::units::WeekSlot {
            week,
            folder: folder.to_string(),
            unit_id: 10 + week,
            unit_name: format!("Week {week} — Topic"),
            unit_kind: "week".to_string(),
            meets_on: None,
        }
    }

    /// The week word files, Canvas's week folder files under its own name,
    /// Canvas's plain folder is the destination, a module reading keeps its
    /// card, and a loose file whose name reads nothing keeps its sort job.
    #[test]
    fn where_a_staged_file_goes() {
        let slots = [slot(3, "Week 03 — Data"), slot(4, "Week 04 — Probability")];
        let canvas = "Canvas files it under \"Reading Material\"";
        match auto_filing(&slots, true, "Week 4 Reinhold.pdf", Some("Reading Material/Week 4 Reinhold.pdf"), canvas) {
            AutoFiling::Filed { dest_rel, reasoning } => {
                assert_eq!(dest_rel, "Weeks/Week 04 — Probability/Week 4 Reinhold.pdf");
                assert!(reasoning.contains("its name carries Week 4"), "{reasoning}");
                assert!(reasoning.contains("Week 4 — Topic"), "{reasoning}");
            }
            other => panic!("{other:?}"),
        }
        let folder = "Canvas files it under \"Week 3 Coding Material\"";
        match auto_filing(&slots, true, "intro.html", Some("Week 3 Coding Material/intro.html"), folder) {
            AutoFiling::Filed { dest_rel, reasoning } => {
                assert_eq!(dest_rel, "Weeks/Week 03 — Data/Week 3 Coding Material/intro.html");
                assert!(reasoning.contains("a folder named for Week 3"), "{reasoning}");
            }
            other => panic!("{other:?}"),
        }
        let quizzes = "Canvas files it under \"Quizzes\"";
        assert_eq!(
            auto_filing(&slots, true, "Quiz 1.pdf", Some("Quizzes/Quiz 1.pdf"), quizzes),
            AutoFiling::Filed {
                dest_rel: "Quizzes/Quiz 1.pdf".to_string(),
                reasoning: "Canvas files it under \"Quizzes\".".to_string(),
            }
        );
        // A week the course does not declare yields to the folder, and then
        // to Canvas's placement.
        assert_eq!(
            auto_filing(&slots, true, "Week 9 notes.pdf", Some("Slides/Week 9 notes.pdf"), "Canvas files it under \"Slides\""),
            AutoFiling::Filed {
                dest_rel: "Slides/Week 9 notes.pdf".to_string(),
                reasoning: "Canvas files it under \"Slides\".".to_string(),
            }
        );
        assert_eq!(
            auto_filing(&slots, true, "Biostatistics_Module3_Slides.pptx", Some("Slides/Biostatistics_Module3_Slides.pptx"), "x"),
            AutoFiling::ModuleCard
        );
        assert_eq!(auto_filing(&slots, true, "parking.pdf", None, "x"), AutoFiling::Loose);
        match auto_filing(&slots, false, "Week3 fixture.csv", None, "x") {
            AutoFiling::Filed { dest_rel, reasoning } => {
                assert_eq!(dest_rel, "Weeks/Week 03 — Data/Week3 fixture.csv");
                assert!(reasoning.starts_with("Canvas keeps it in no folder."), "{reasoning}");
            }
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod filing_now_tests {
    use super::*;

    fn class_on_disk(name: &str) -> (Connection, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("classhub-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let class_dir = crate::scanner::class_dir(&conn, 3).expect("class dir");
        fs::create_dir_all(class_dir.join(INBOX_DIR)).expect("inbox");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (3, 1, 'week', 'Week 4 — Probability', 4, '2026-09-10', 'syllabus')",
            [],
        )
        .expect("week");
        (conn, root, class_dir)
    }

    fn pending(conn: &Connection, source: &str, source_rel: &str, dest_rel: &str) -> i64 {
        conn.execute(
            "INSERT INTO move_proposals (class_id, source_rel_path, dest_rel_path, reasoning, source, status, created_at)
             VALUES (3, ?1, ?2, 'a card', ?3, 'pending', 0)",
            params![source_rel, dest_rel, source],
        )
        .expect("card");
        conn.last_insert_rowid()
    }

    fn status(conn: &Connection, id: i64) -> (String, String) {
        conn.query_row(
            "SELECT status, dest_rel_path FROM move_proposals WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("row")
    }

    /// The sync's placement: refused by name while a card from another
    /// route holds the file or another card claims the destination, and its
    /// own waiting card becomes the record of the move rather than a second
    /// row, with the destination and the reason the rule read.
    #[test]
    fn a_placement_honours_the_queue_and_takes_over_its_own_card() {
        let (conn, root, class_dir) = class_on_disk("file-now");
        fs::write(class_dir.join("_Inbox/Quiz 1.pdf"), "%PDF").unwrap();
        let chat = pending(&conn, "chat", "_Inbox/Quiz 1.pdf", "Decks/Quiz 1.pdf");
        let err = file_now(&conn, 3, &class_dir, "_Inbox/Quiz 1.pdf", "Quizzes/Quiz 1.pdf", "Canvas files it.", "b")
            .expect_err("chat holds it");
        assert!(format!("{err:#}").contains("from chat"), "{err:#}");
        assert!(class_dir.join("_Inbox/Quiz 1.pdf").is_file(), "not moved");
        conn.execute("DELETE FROM move_proposals WHERE id = ?1", [chat]).unwrap();

        fs::write(class_dir.join("_Inbox/Week 4 reading.pdf"), "%PDF").unwrap();
        pending(&conn, "by_name", "Readings/Week 4 reading.pdf", "Quizzes/Quiz 1.pdf");
        let err = file_now(&conn, 3, &class_dir, "_Inbox/Quiz 1.pdf", "Quizzes/Quiz 1.pdf", "Canvas files it.", "b")
            .expect_err("destination claimed");
        assert!(format!("{err:#}").contains("already proposed for"), "{err:#}");
        conn.execute("DELETE FROM move_proposals", []).unwrap();

        let waiting = pending(&conn, "canvas", "_Inbox/Quiz 1.pdf", "Quizzes/Quiz 1.pdf");
        let (dest, audit_id) =
            file_now(&conn, 3, &class_dir, "_Inbox/Quiz 1.pdf", "Quizzes/Quiz 1.pdf", "Canvas files it under \"Quizzes\".", "b")
                .expect("filed");
        assert_eq!(dest, "Quizzes/Quiz 1.pdf");
        assert!(class_dir.join("Quizzes/Quiz 1.pdf").is_file());
        assert_eq!(status(&conn, waiting), ("approved".to_string(), "Quizzes/Quiz 1.pdf".to_string()));
        let rows: i64 = conn.query_row("SELECT COUNT(*) FROM move_proposals", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 1, "the waiting card is the record, not a second row");
        let (action, batch): (String, String) = conn
            .query_row(
                "SELECT action, json_extract(payload, '$.batch') FROM audit_log WHERE id = ?1",
                [audit_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((action.as_str(), batch.as_str()), ("canvas.filed", "b"));
        // A destination a file already occupies lands beside it.
        fs::write(class_dir.join("_Inbox/Quiz 1.pdf"), "%PDF v2").unwrap();
        let (dest, _) = file_now(&conn, 3, &class_dir, "_Inbox/Quiz 1.pdf", "Quizzes/Quiz 1.pdf", "Canvas files it.", "b")
            .expect("filed beside");
        assert_eq!(dest, "Quizzes/Quiz 1 (2).pdf");
        let _ = fs::remove_dir_all(&root);
    }

    /// Approve all: every pending card's ordinary move under one batch, a
    /// card whose file is gone left pending and named.
    #[test]
    fn approve_all_moves_every_card_it_can_and_names_the_rest() {
        let (conn, root, class_dir) = class_on_disk("approve-all");
        fs::write(class_dir.join("_Inbox/one.csv"), "a\n").unwrap();
        fs::write(class_dir.join("_Inbox/two.csv"), "b\n").unwrap();
        let one = pending(&conn, "sort_job", "_Inbox/one.csv", "Data/one.csv");
        let two = pending(&conn, "chat", "_Inbox/two.csv", "Data/two.csv");
        let gone = pending(&conn, "sort_job", "_Inbox/gone.csv", "Data/gone.csv");
        let (outcome, audit_ids) = approve_all_in_conn(&conn, 3, "batch-1").expect("approve all");
        assert_eq!(outcome.approved, [one, two]);
        assert_eq!(audit_ids.len(), 2);
        assert_eq!(outcome.skipped.len(), 1, "{outcome:?}");
        assert!(outcome.skipped[0].contains("no longer on disk"), "{outcome:?}");
        assert!(class_dir.join("Data/one.csv").is_file() && class_dir.join("Data/two.csv").is_file());
        assert_eq!(status(&conn, gone).0, "pending");
        let batches: Vec<String> = conn
            .prepare("SELECT json_extract(payload, '$.batch') FROM audit_log WHERE action = 'sort.move'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(batches, ["batch-1", "batch-1"]);
        let _ = fs::remove_dir_all(&root);
    }
}
