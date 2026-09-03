//! SPEC §7.1 — lecture ingestion, and the `lecture_digest` job that distills a
//! session.
//!
//! A recording reaches the app as one of three things: a caption track Zoom
//! produced (`.vtt`/`.srt`/the in-meeting `.txt` save), a media file with no
//! captions at all, or a Zoom recording link. They converge here: whatever came
//! in becomes cues (`transcripts.rs`), the cues become markdown, and the
//! markdown is written into the class tree as ordinary source material.
//!
//! Filing it as source rather than as an app-managed artifact is the whole
//! trick. From that point nothing else needed changing: the scanner indexes it,
//! `extract::route` sends it down the zero-token text path, and chat searches
//! it. The transcript joins the pipeline instead of sitting beside it.
//!
//! Where it is filed is the second half. A lecture goes under
//! `Weeks/Week NN — <topic>/`, and because each of these courses meets once a
//! week and divides itself no finer, the week it was filed under *is* the
//! division it covers (SPEC §8.5). `lecture_contributions` records that join,
//! which is how a lecture stored by date reaches a guide scoped by topic.
//!
//! The digest is the separate, token-spending half — a `claude -p` job over the
//! transcript that writes the session document, names it, and distils the
//! session into the corpus note that guide is built from.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::{
    audit, emit_hub_change, lock, now, with_conn, CORPUS_DIR, INBOX_DIR, SESSIONS_DIR,
    SESSION_SCOPE_PREFIX, WEEKS_DIR,
};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/lecture_digest.md");

/// What the UI hands over to add a lecture.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRequest {
    pub class_id: i64,
    /// An absolute path to a caption track or a recording, or a Zoom link.
    pub source: String,
    /// The week this session belongs to, from the course's own schedule
    /// (SPEC §8.5) — the form resolves and shows it, and it is correctable
    /// there. `None` routes through `_Inbox/` so the sorter proposes a week
    /// instead, which is what a course publishing no schedule gets.
    pub week: Option<i64>,
    /// ISO `YYYY-MM-DD`. The UI always sends one, so this module never has to
    /// guess a session date from a file's mtime.
    pub date: String,
    /// Overrides the transcript's file name; defaults to "Lecture".
    pub title: Option<String>,
    /// Whether to spend tokens distilling it once it is filed.
    pub digest: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub rel_path: String,
    /// True when no week was resolved and the sorter is proposing one.
    pub routed_to_inbox: bool,
    /// The course's own name for the division this lecture now feeds, when it
    /// declares one for that week.
    pub unit_name: Option<String>,
    pub speakers: Vec<String>,
    pub digest_job_id: Option<i64>,
    /// Why no digest started, when one was asked for. The transcript is filed
    /// either way, so this is a note rather than a failure — but silence here
    /// reads as "no digest was wanted".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest_error: Option<String>,
}

// ---------------------------------------------------------------------------
// Ingestion

/// Turns a recording or caption track into a filed transcript. Long-running
/// when it has to transcribe, so callers run it off the UI thread.
pub fn add(app: &AppHandle, req: &AddRequest, on_stage: &dyn Fn(&str)) -> Result<AddResult> {
    if !crate::deadlines::valid_due_at(&req.date) || req.date.len() != 10 {
        bail!("the session date must be YYYY-MM-DD");
    }

    let title = req.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let file_name = transcript_file_name(&req.date, title.unwrap_or("Lecture"))?;

    // Only the lookups need the connection. Writing a multi-megabyte markdown
    // with it held blocks every other command, the chat tools and the job
    // runner for the duration — the arrangement `scan_class` and
    // `search_material` were both restructured away from.
    let Filing { class_dir, slot, dir_rel } =
        with_conn(app, |conn| resolve_filing(conn, req.class_id, req.week, &file_name))?;
    let routed_to_inbox = slot.is_none();

    let (caption, source_name) = fetch(app, &req.source, on_stage)?;
    let cues = crate::transcripts::parse(&caption);
    if cues.is_empty() {
        bail!("{source_name} holds no readable speech");
    }

    let markdown = crate::transcripts::to_markdown(
        &cues,
        &crate::transcripts::Meta {
            title: file_name.trim_end_matches(".md"),
            date: &req.date,
            source_name: &source_name,
        },
    );

    let dir = class_dir.join(&dir_rel);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    // Never overwrite: two lectures on one date, or a re-add of the same
    // one, are both ordinary and neither should silently replace a file.
    let rel_path = unique_rel_path(&class_dir, &dir_rel, &file_name);
    crate::db::write_atomic(&class_dir.join(&rel_path), &markdown)?;

    // Logged rather than propagated: the transcript is on disk by now, so
    // failing the run here would report "could not add the lecture" over a
    // filed file and earn a duplicate on the retry.
    let audited = with_conn(app, |conn| {
        audit(
            conn,
            "lecture.added",
            serde_json::json!({
                "classId": req.class_id,
                "relPath": rel_path,
                "source": source_name,
                "date": req.date,
                "cues": cues.len(),
            }),
        )
    });
    if let Err(e) = audited {
        eprintln!("lecture.added audit row failed for {rel_path}: {e:#}");
    }

    // SPEC §8.5: the filing decision is the join, so it is recorded here rather
    // than inferred later. A lecture that silently feeds no guide is the
    // failure this milestone exists to prevent, so a refusal here fails the
    // run — and takes the file with it: the name was free when the form asked
    // (`resolve_filing`) and a row took it during the capture, and a file
    // left in the week folder would be indexed by the next scan as a lecture
    // no division reads, beside the retry under a title.
    if let Some(slot) = &slot {
        let recorded = with_conn(app, |conn| {
            record_contribution(conn, req.class_id, &class_dir, slot, &rel_path, &markdown)
        });
        if let Err(e) = recorded {
            if let Err(rm) = fs::remove_file(class_dir.join(&rel_path)) {
                eprintln!("could not remove the refused transcript {rel_path}: {rm}");
            }
            return Err(e).with_context(|| {
                format!("{rel_path} was not filed — mapping it to {} failed", slot.unit_name)
            });
        }
    }

    // Index it before anything downstream looks for it: the digest job's
    // manifest reads the `files` row, and the sorter lists the inbox. A failure
    // here is not cosmetic: with no row the digest's manifest is empty, and the
    // session then reads as permanently fresh, or as permanently stale once the
    // row does appear.
    on_stage("Indexing…");
    {
        let db = app.state::<crate::Db>();
        if let Err(e) = crate::scanner::scan_class(&db.0, req.class_id) {
            bail!("filed {rel_path}, but indexing it failed: {e:#}");
        }
    }
    crate::extract::spawn_pipeline(app, req.class_id);

    let mut digest_error = None;
    let digest_job_id = if req.digest && !routed_to_inbox {
        match enqueue_digest(app, req.class_id, &rel_path, &req.date) {
            Ok(id) => Some(id),
            // The transcript is filed and that is the durable half; a digest
            // that failed to enqueue is a button away, not a reason to unwind.
            // Said out loud, though — the outcome panel otherwise shows the
            // same line as for a lecture no digest was ever asked for.
            Err(e) => {
                eprintln!("lecture digest failed to enqueue: {e:#}");
                digest_error = Some(format!("{e:#}"));
                None
            }
        }
    } else {
        None
    };

    if routed_to_inbox {
        crate::sorter::enqueue_followup(app, req.class_id);
    }
    emit_hub_change(app, "files");

    Ok(AddResult {
        rel_path,
        routed_to_inbox,
        unit_name: slot.map(|slot| slot.unit_name),
        speakers: crate::transcripts::speakers(&cues),
        digest_job_id,
        digest_error,
    })
}

/// Resolves whatever the user pointed at into caption text plus a display name
/// for where it came from.
fn fetch(app: &AppHandle, source: &str, on_stage: &dyn Fn(&str)) -> Result<(String, String)> {
    let source = source.trim();
    if source.starts_with("http://") || source.starts_with("https://") {
        return crate::zoom::fetch_caption(app, source, on_stage);
    }

    let path = Path::new(source);
    if !path.is_file() {
        bail!("no file at {source}");
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.to_string());

    if crate::transcribe::is_media(path) {
        return Ok((crate::transcribe::to_vtt(app, path, on_stage)?, name));
    }
    // Anything that is not media is read whole, so it needs a ceiling: nothing
    // upstream checks the extension, and a caption track for a three-hour
    // lecture is a couple of megabytes. Past this it is not a transcript.
    const MAX_CAPTION_BYTES: u64 = 64 * 1024 * 1024;
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_CAPTION_BYTES {
        bail!(
            "{name} is {}MB — too large to be a caption track. Point at the \
             recording itself, or at the transcript Zoom saved.",
            size / (1024 * 1024)
        );
    }
    let text = fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).replace("\r\n", "\n"))
        .with_context(|| format!("reading {source}"))?;
    Ok((text, name))
}

/// `2026-08-24 — Lecture.md`. Slashes and colons would repoint the write, so
/// they flatten to dashes rather than rejecting a natural title (the same
/// policy `notes::note_file_name` applies to note titles).
fn transcript_file_name(date: &str, title: &str) -> Result<String> {
    let cleaned: String = title
        .trim()
        .trim_end_matches(".md")
        .chars()
        .map(|c| if c == '/' || c == ':' { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        bail!("the lecture needs a title");
    }
    if cleaned.chars().count() > 80 {
        bail!("lecture title is too long — keep it under 80 characters");
    }
    Ok(format!("{date} — {cleaned}.md"))
}

/// Where a lecture is going, settled before anything is captured.
struct Filing {
    class_dir: PathBuf,
    /// The week and the division it feeds, or `None` for a lecture the
    /// sorter will place.
    slot: Option<crate::units::WeekSlot>,
    /// The folder it files into, class-relative: the week's, or `_Inbox/`.
    dir_rel: String,
}

/// Resolves the week, the folder and the name a lecture will take, and
/// refuses one whose note another transcript holds.
///
/// A lecture is filed by when it happened, and for these four courses that
/// also settles what it covers (SPEC §8.5). A week that resolved to none of
/// the course's own means the sorter gets to propose one, which it can only
/// do from `_Inbox/` (SPEC §10).
///
/// The name decides where the note goes, and a division spanning several
/// weeks holds one note per name (`refuse_held_note`). Settled here, before
/// the capture: refused now it costs nothing, refused after a three-hour
/// capture it costs the capture. The name is asked for again at the write,
/// since the folder may have filled in the meantime, and the row is checked
/// again there by `record_contribution`.
///
/// A note already on disk with no row behind it is refused here too. A
/// refile to a week the course has not declared leaves one (`refile_lecture`),
/// for the refile back that will claim it again — so this check belongs only
/// to the form, where a title is a field away; on the sorter's path the same
/// note is the lecture's own, coming home.
fn resolve_filing(
    conn: &Connection,
    class_id: i64,
    week: Option<i64>,
    file_name: &str,
) -> Result<Filing> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let slot = match week {
        Some(week) => crate::units::slot_for_week(conn, class_id, week)?,
        None => None,
    };
    let dir_rel = match &slot {
        Some(slot) => format!("{WEEKS_DIR}/{}", slot.folder),
        None => INBOX_DIR.to_string(),
    };
    if let Some(slot) = &slot {
        let rel_path = unique_rel_path(&class_dir, &dir_rel, file_name);
        let corpus_rel = corpus_rel_path(&slot.unit_name, &rel_path);
        refuse_held_note(conn, class_id, &class_dir, &slot.unit_name, &corpus_rel, &rel_path)?;
        if class_dir.join(&corpus_rel).is_file() {
            bail!(
                "a distilled note already exists at {corpus_rel}, and this lecture's would \
                 replace it — give it a title of its own"
            );
        }
    }
    Ok(Filing { class_dir, slot, dir_rel })
}

/// Appends ` (2)`, ` (3)`… until the name is free, the way `guides.rs` keeps
/// two practice exams on one date from colliding.
fn unique_rel_path(class_dir: &Path, dir_rel: &str, file_name: &str) -> String {
    let stem = file_name.trim_end_matches(".md");
    let mut rel = format!("{dir_rel}/{file_name}");
    let mut n = 2;
    while class_dir.join(&rel).exists() {
        rel = format!("{dir_rel}/{stem} ({n}).md");
        n += 1;
    }
    rel
}

// ---------------------------------------------------------------------------
// The lecture → unit join (SPEC §8.5)

/// One lecture's contribution, for the workspace listing and for the guide
/// that reads its corpus note.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contribution {
    /// The transcript, class-relative.
    pub rel_path: String,
    pub unit_id: i64,
    /// The course's own name for the division this lecture feeds.
    pub unit_name: String,
    pub corpus_rel_path: String,
    /// Whether the corpus note has actually been written yet — the distillation
    /// is a separate, token-spending pass, so the map exists before the note.
    pub distilled: bool,
    pub summary: String,
}

/// Where one lecture's distilled note goes: under the unit it feeds, named for
/// the transcript it was cut from.
///
/// Derived rather than chosen by the digest, the way an extract path mirrors
/// its source (SPEC §4). That keeps the path knowable at filing time — which is
/// when the contribution row is written — and makes "did the note get written"
/// a question about one known path rather than about a name a model reported.
pub(crate) fn corpus_rel_path(unit_name: &str, transcript_rel: &str) -> String {
    let file_name = Path::new(transcript_rel)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| transcript_rel.to_string());
    format!("{}/{file_name}", corpus_folder(unit_name))
}

/// The folder one division's notes live in, named for the division — which is
/// why a rename moves it (`units::upsert`).
pub(crate) fn corpus_folder(unit_name: &str) -> String {
    format!("{CORPUS_DIR}/{}", crate::units::folder_segment(unit_name))
}

/// What one filed transcript covers, end to end: `(end_ms, lines)`.
///
/// Read back off the filed markdown rather than carried from the cues, so a
/// transcript the sorter placed — which never passed through ingestion —
/// describes itself the same way one added through the form does. The
/// `## HH:MM` anchors are the only timing the artifact keeps; a transcript with
/// none was transcribed without timings and reports a zero-length span rather
/// than an invented one.
fn span_of(markdown: &str) -> (i64, i64) {
    let mut end_ms = 0i64;
    let mut lines = 0i64;
    for line in markdown.lines() {
        lines += 1;
        let Some((hours, minutes)) = line.strip_prefix("## ").and_then(|s| s.trim().split_once(':'))
        else {
            continue;
        };
        if let (Ok(hours), Ok(minutes)) = (hours.parse::<i64>(), minutes.parse::<i64>()) {
            end_ms = end_ms.max((hours * 60 + minutes) * 60_000);
        }
    }
    (end_ms, lines.max(1))
}

/// What a contribution row says about the session until the digest has read
/// it, followed by the unit's name.
const PLACEHOLDER_SUMMARY: &str = "Whole session — ";

/// Records the join between a lecture stored by date and the division it
/// covers, and returns where its corpus note goes.
///
/// One row per lecture, spanning its whole length, `applied` on sight: the
/// filing decision it follows is the user's own rather than a model's reading,
/// so there is nothing to confirm (SPEC §8.5). The delete before the insert is
/// what makes refiling a lecture *move* its contribution and a re-run *replace*
/// it, rather than either adding a second.
fn record_contribution(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    slot: &crate::units::WeekSlot,
    rel_path: &str,
    markdown: &str,
) -> Result<String> {
    let corpus_rel = corpus_rel_path(&slot.unit_name, rel_path);
    refuse_held_note(conn, class_id, class_dir, &slot.unit_name, &corpus_rel, rel_path)?;
    let (end_ms, lines) = span_of(markdown);
    conn.execute(
        "DELETE FROM lecture_contributions WHERE class_id = ?1 AND rel_path = ?2",
        rusqlite::params![class_id, rel_path],
    )?;
    conn.execute(
        "INSERT INTO lecture_contributions
         (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
          corpus_rel_path, summary, confidence, status, created_at)
         VALUES (?1, ?2, ?3, 0, ?4, 1, ?5, ?6, ?7, 'high', 'applied', ?8)",
        rusqlite::params![
            class_id,
            slot.unit_id,
            rel_path,
            end_ms,
            lines,
            corpus_rel,
            format!("{PLACEHOLDER_SUMMARY}{}", slot.unit_name),
            now(),
        ],
    )?;
    Ok(corpus_rel)
}

/// Refuses a transcript whose note path another transcript of the class
/// already holds.
///
/// A note is keyed by its division and its transcript's name (SPEC §8.5). For
/// a course that numbers its weeks the division is the week folder, where
/// `unique_rel_path` keeps two names apart; a division that groups weeks — a
/// Part spanning eight of them — holds one note per name across all of its
/// folders, so `2026-09-01 — Lecture.md` under Week 02 and under Week 03
/// derive one path, and the second digest would write over the first note
/// with both rows naming it. The refile already refuses that collision
/// (`refile_lecture`); this is the same line at the filing, and for the
/// sorter's path through `contribution_for`. A name of its own is the way
/// out — the form's title, or a rename of the file the sorter is moving —
/// and the message says so.
///
/// A holder whose transcript is no longer on disk is a row a delete in
/// Finder left behind, which no scan clears: it holds the name for a lecture
/// that is not there, so it is cleared here rather than refusing a real one
/// in its name. A note it may have left is the next digest's to replace.
fn refuse_held_note(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    unit_name: &str,
    corpus_rel: &str,
    rel_path: &str,
) -> Result<()> {
    let holder: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, rel_path FROM lecture_contributions
             WHERE class_id = ?1 AND corpus_rel_path = ?2 AND rel_path != ?3",
            rusqlite::params![class_id, corpus_rel, rel_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, holder)) = holder else {
        return Ok(());
    };
    if !class_dir.join(&holder).is_file() {
        conn.execute("DELETE FROM lecture_contributions WHERE id = ?1", [id])?;
        eprintln!("lectures: cleared the contribution of {holder}, whose transcript is gone");
        return Ok(());
    }
    bail!(
        "its note would replace the one for {holder}, which already feeds {unit_name} under the \
         same name — file it under a name of its own: the title on the Add lecture form, or a \
         rename of the file"
    )
}

/// A markdown file sitting in a week folder is taken for a lecture — the same
/// reading the Lectures listing applies to the tree. A slide deck the sorter
/// filed under a week is not, and must not earn a contribution row.
fn is_filed_transcript(rel_path: &str) -> bool {
    rel_path.to_lowercase().ends_with(".md")
        && crate::units::week_from_rel_path(rel_path).is_some()
}

/// The contribution for one transcript, recording it first when the path names
/// a week the course declares and no row exists yet.
///
/// The second case is the sorter's: a transcript it filed reached its week by
/// an approved move, which never passes through the Add lecture form, so the
/// path is where that decision is written down.
fn contribution_for(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    rel_path: &str,
) -> Result<Option<(i64, String, String)>> {
    let existing = conn
        .query_row(
            "SELECT lc.unit_id, u.name, lc.corpus_rel_path
             FROM lecture_contributions lc JOIN units u ON u.id = lc.unit_id
             WHERE lc.class_id = ?1 AND lc.rel_path = ?2",
            rusqlite::params![class_id, rel_path],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if existing.is_some() {
        return Ok(existing);
    }

    if !is_filed_transcript(rel_path) {
        return Ok(None);
    }
    let Some(week) = crate::units::week_from_rel_path(rel_path) else {
        return Ok(None);
    };
    let Some(slot) = crate::units::slot_for_week(conn, class_id, week)? else {
        return Ok(None);
    };
    let markdown = fs::read_to_string(class_dir.join(rel_path))
        .with_context(|| format!("reading {rel_path}"))?;
    let corpus_rel = record_contribution(conn, class_id, class_dir, &slot, rel_path, &markdown)?;
    Ok(Some((slot.unit_id, slot.unit_name, corpus_rel)))
}

/// Moves everything keyed by a transcript's path along with the transcript,
/// for one relocated through the §10 confirm queue. Called inside the move's
/// own transaction; what it hands back is applied after the commit.
///
/// Two things are keyed that way. The session document's row (`refile_session`)
/// follows the path so the digest neither drops out of the listing nor reads
/// stale over a rename. The contribution is re-resolved rather than only
/// rewritten, because refiling to a different week is the correction affordance
/// (SPEC §8.5), and the distilled note travels with it: it holds the
/// transcript's content, which the move did not change, and leaving it behind
/// would price every correction at a fresh digest of a three-hour lecture. Only
/// when the new home is not a week is the row cleared and the note removed,
/// because a contribution naming a transcript that has left `Weeks/` maps a
/// guide to something no longer there.
pub fn refile_lecture(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
) -> Result<RefileEffects> {
    let orphaned = refile_session(conn, class_id, class_dir, source_rel, dest_rel)?;
    let mut effects = RefileEffects { note: None, orphaned };

    let old_row: Option<(String, String)> = conn
        .query_row(
            "SELECT corpus_rel_path, summary FROM lecture_contributions
             WHERE class_id = ?1 AND rel_path = ?2",
            rusqlite::params![class_id, source_rel],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if old_row.is_none() && !is_filed_transcript(dest_rel) {
        return Ok(effects);
    }
    conn.execute(
        "DELETE FROM lecture_contributions WHERE class_id = ?1 AND rel_path = ?2",
        rusqlite::params![class_id, source_rel],
    )?;
    let moved = contribution_for(conn, class_id, class_dir, dest_rel)?;
    let (old_corpus, old_summary) = match old_row {
        Some((corpus, summary)) => (Some(corpus), Some(summary)),
        None => (None, None),
    };
    // The re-recorded row opens with the filing placeholder again. The digest's
    // title was worth keeping and the note that carried it survives, so the
    // summary travels too; a placeholder names the old unit and is left behind.
    if let (Some(summary), Some(_)) = (&old_summary, &moved) {
        if !summary.starts_with(PLACEHOLDER_SUMMARY) {
            conn.execute(
                "UPDATE lecture_contributions SET summary = ?1 WHERE class_id = ?2 AND rel_path = ?3",
                rusqlite::params![summary, class_id, dest_rel],
            )?;
        }
    }

    let Some(old) = old_corpus else {
        return Ok(effects);
    };
    let from = class_dir.join(&old);
    if !from.is_file() {
        // Never distilled, or the note is already gone: nothing to carry.
        return Ok(effects);
    }
    effects.note = match moved {
        // A different unit's corpus: the note goes with the lecture, so that
        // unit's guide reads it and the old unit's guide stops reading it.
        Some((_, _, new)) if new != old => {
            let to = class_dir.join(&new);
            // Notes are keyed by unit and transcript name, and a unit spanning
            // several weeks can hold two transcripts with one name. Refused
            // here, inside the transaction, so the move rolls back cleanly
            // instead of one distillation silently replacing another.
            if to.exists() {
                bail!("a distilled note already exists at {new} — refiling would overwrite it");
            }
            Some(NoteMove::Relocate { from, to })
        }
        Some(_) => None,
        // Still under `Weeks/`, but in a week the course has not declared or a
        // folder that is not a week: no unit reads the note for now, and it
        // stays where it is. The next syllabus scan or Canvas sync may declare
        // the week, and a refile back to a declared one finds it again — while
        // deleting it would charge that correction a fresh digest.
        None if is_filed_transcript(dest_rel) => None,
        // Out of `Weeks/`: no unit reads it any more.
        None => Some(NoteMove::Remove(from)),
    };
    Ok(effects)
}

/// Everything a refile leaves for the filesystem, decided inside the move's
/// transaction and performed once it has committed. Nothing on disk changes
/// while the database can still roll back, so a failed commit leaves the note
/// and the documents exactly where the rows still say they are — the sorter's
/// undo path reverses the transcript's own rename and needs to know nothing
/// about either.
#[must_use = "nothing on disk changes until this is applied"]
#[derive(Debug)]
pub struct RefileEffects {
    /// The corpus note's move, when the lecture carries one.
    note: Option<NoteMove>,
    /// Session documents of a row cleared at the destination scope: chat's
    /// search walks `Study Guides/`, so a pair nothing points at would go on
    /// answering for a lecture that is not there.
    orphaned: Vec<PathBuf>,
}

impl RefileEffects {
    /// Applied after the commit, so a failure is logged rather than
    /// propagated: the database has already recorded the move, and undoing the
    /// transcript's rename against it would be worse than a note that stayed
    /// put — which the Lectures listing shows as undistilled, and a redistill
    /// repairs.
    pub fn apply(self) {
        for path in self.orphaned {
            if let Err(e) = fs::remove_file(&path) {
                eprintln!("orphaned session document removal failed ({}): {e}", path.display());
            }
        }
        if let Some(note) = self.note {
            note.apply();
        }
    }
}

/// The corpus note's part of a refile (see `RefileEffects`).
#[derive(Debug)]
pub enum NoteMove {
    /// The note follows the lecture into another unit's corpus folder.
    Relocate { from: PathBuf, to: PathBuf },
    /// The lecture left `Weeks/`, so no unit reads the note any more.
    Remove(PathBuf),
}

impl NoteMove {
    fn apply(self) {
        match self {
            NoteMove::Relocate { from, to } => {
                let moved = to
                    .parent()
                    .map_or(Ok(()), fs::create_dir_all)
                    .and_then(|()| fs::rename(&from, &to));
                if let Err(e) = moved {
                    eprintln!(
                        "corpus note move failed ({} → {}): {e}",
                        from.display(),
                        to.display()
                    );
                    return;
                }
                // `remove_dir` refuses a folder with anything left in it, which
                // is the whole check: a unit's corpus folder outlives its last
                // note only as clutter.
                if let Some(parent) = from.parent() {
                    let _ = fs::remove_dir(parent);
                }
            }
            NoteMove::Remove(path) => {
                if let Err(e) = fs::remove_file(&path) {
                    eprintln!("corpus note removal failed ({}): {e}", path.display());
                }
            }
        }
    }
}

/// The session document follows its transcript. Its `guides` row is keyed by
/// the transcript's path (`session:<path>`) and its manifest names that path,
/// so left alone a refiled lecture's digest would drop out of the Lectures
/// listing and read as stale over a move that changed no content — inviting a
/// redistill to buy back nothing.
///
/// Returns the documents of any row already at the destination scope, for the
/// caller to remove once the move has committed.
fn refile_session(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    source_rel: &str,
    dest_rel: &str,
) -> Result<Vec<PathBuf>> {
    let new_scope = session_scope(dest_rel);
    // A row already at the new scope can only be a leftover from a transcript
    // that vanished without a move — the destination itself was checked to be
    // free on disk. Cleared whether or not the incoming transcript brings a
    // row of its own: left standing it would read as the newcomer's session
    // document, and its pair would open the other lecture. The documents go
    // with it, as `record_session` does with a superseded pair, once the
    // caller has committed.
    let mut stmt = conn.prepare("SELECT rel_path FROM guides WHERE class_id = ?1 AND scope = ?2")?;
    let orphaned: Vec<PathBuf> = stmt
        .query_map(rusqlite::params![class_id, &new_scope], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|html| [html.clone(), format!("{}.md", html.trim_end_matches(".html"))])
        .filter_map(|rel| session_path(class_dir, &rel))
        .collect();
    conn.execute(
        "DELETE FROM guides WHERE class_id = ?1 AND scope = ?2",
        rusqlite::params![class_id, &new_scope],
    )?;

    let old_scope = session_scope(source_rel);
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, source_manifest FROM guides WHERE class_id = ?1 AND scope = ?2",
            rusqlite::params![class_id, &old_scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((id, manifest)) = row else {
        return Ok(orphaned);
    };
    // An unreadable manifest is read the way `manifest_is_stale` reads one: as
    // stale, never as an error. Failing here would roll back the whole move
    // over one corrupt row; the scope still follows, and the digest shows as
    // stale, which is the honest answer.
    let manifest = match serde_json::from_str::<Vec<crate::extract::ManifestEntry>>(&manifest) {
        Ok(mut entries) => {
            for entry in &mut entries {
                if entry.rel_path == source_rel {
                    entry.rel_path = dest_rel.to_string();
                }
            }
            serde_json::to_string(&entries)?
        }
        Err(_) => manifest,
    };
    conn.execute(
        "UPDATE guides SET scope = ?1, source_manifest = ?2 WHERE id = ?3",
        rusqlite::params![new_scope, manifest, id],
    )?;
    Ok(orphaned)
}

/// Every lecture that feeds one of this class's divisions.
pub fn list_contributions(conn: &Connection, class_id: i64) -> Result<Vec<Contribution>> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let mut stmt = conn.prepare(
        "SELECT lc.rel_path, lc.unit_id, u.name, lc.corpus_rel_path, lc.summary
         FROM lecture_contributions lc JOIN units u ON u.id = lc.unit_id
         WHERE lc.class_id = ?1 AND lc.status = 'applied'
         ORDER BY lc.rel_path",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            let corpus_rel_path: String = row.get(3)?;
            Ok(Contribution {
                rel_path: row.get(0)?,
                unit_id: row.get(1)?,
                unit_name: row.get(2)?,
                distilled: class_dir.join(&corpus_rel_path).is_file(),
                corpus_rel_path,
                summary: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The transcripts mapped to one unit.
pub fn contributing_paths(
    conn: &Connection,
    class_id: i64,
    unit_id: i64,
) -> Result<std::collections::BTreeSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path FROM lecture_contributions
         WHERE class_id = ?1 AND unit_id = ?2 AND status = 'applied'",
    )?;
    let paths = stmt
        .query_map(rusqlite::params![class_id, unit_id], |row| row.get(0))?
        .collect::<rusqlite::Result<std::collections::BTreeSet<String>>>()?;
    Ok(paths)
}

/// The corpus notes a unit's guide is built from, as `(note, transcript)`
/// pairs (SPEC §8.5).
///
/// Only notes that exist on disk: a lecture filed but not yet distilled has a
/// row and no note, and naming a file that is not there sends the job looking
/// for it.
pub fn corpus_notes(conn: &Connection, class_id: i64, unit_id: i64) -> Result<Vec<(String, String)>> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let mut stmt = conn.prepare(
        "SELECT corpus_rel_path, rel_path FROM lecture_contributions
         WHERE class_id = ?1 AND unit_id = ?2 AND status = 'applied'
         ORDER BY rel_path",
    )?;
    let notes = stmt
        .query_map(rusqlite::params![class_id, unit_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(corpus, _)| class_dir.join(corpus).is_file())
        .collect();
    Ok(notes)
}

/// The prompt block naming those notes, each with the transcript it came from
/// so the job can open the professor's exact words when the distillation is
/// not enough.
pub fn corpus_block(notes: &[(String, String)]) -> String {
    if notes.is_empty() {
        return "(none — no lecture for this division has been distilled yet)".into();
    }
    notes
        .iter()
        .map(|(corpus, transcript)| format!("- {corpus}\n  transcript: {transcript}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// Command entry point

/// Tauri event carrying ingestion progress and the final outcome.
pub const PROGRESS_EVENT: &str = "lecture://progress";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    class_id: i64,
    stage: &'a str,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a AddResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Runs `add` on its own thread and reports over `PROGRESS_EVENT`.
///
/// A plain `std::thread` rather than an async command, for the same reason
/// `chat::send` uses one: this path runs blocking HTTP and waits on child
/// processes for minutes at a time, neither of which belongs on the async
/// runtime's workers.
/// Classes with an ingestion in flight. The dialog is not the guard: it can be
/// closed and reopened mid-run, and two threads for one class race on the
/// destination name and on the Zoom capture window.
static INGESTING: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

pub fn spawn_add(app: &AppHandle, req: AddRequest) {
    let app = app.clone();
    std::thread::spawn(move || {
        let class_id = req.class_id;
        let emit = |stage: &str, done: bool, result: Option<&AddResult>, error: Option<String>| {
            let _ = tauri::Emitter::emit(
                &app,
                PROGRESS_EVENT,
                Progress { class_id, stage, done, result, error },
            );
        };
        let on_stage = |stage: &str| emit(stage, false, None, None);

        {
            let mut busy = crate::db::lock(&INGESTING);
            if busy.contains(&class_id) {
                emit(
                    "Failed",
                    true,
                    None,
                    Some("a lecture is already being added for this class".into()),
                );
                return;
            }
            busy.push(class_id);
        }
        // Released however the run ends, panic included.
        struct Claim(i64);
        impl Drop for Claim {
            fn drop(&mut self) {
                crate::db::lock(&INGESTING).retain(|id| *id != self.0);
            }
        }
        let _claim = Claim(class_id);

        // Caught, because the terminal event is the only thing that tells the
        // dialog the run is over. A panic here would otherwise leave the last
        // stage line standing forever with no outcome behind it.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            add(&app, &req, &on_stage)
        }));
        match outcome {
            Ok(Ok(result)) => emit("Done", true, Some(&result), None),
            Ok(Err(e)) => emit("Failed", true, None, Some(format!("{e:#}"))),
            Err(_) => emit(
                "Failed",
                true,
                None,
                Some("adding the lecture crashed — see the log for the panic".into()),
            ),
        }
    });
}

// ---------------------------------------------------------------------------
// Digest job (SPEC §8.4)

/// Carried across the job so `finalize_digest` knows what the digest was made
/// from without re-deriving it.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DigestPayload {
    transcript_rel_path: String,
    date: String,
    /// Captured at enqueue, not at finalize — SPEC §8.1's semantics, and what
    /// `guides.rs` does: a transcript edited while the job runs leaves the
    /// digest stale afterwards rather than recording as fresh.
    source_manifest: String,
    /// Where this lecture's distilled note goes (SPEC §8.5), when the lecture
    /// maps to one of the course's divisions. `None` when it maps to none — the
    /// session documents are still worth writing, they just feed no unit guide.
    corpus_rel_path: Option<String>,
}

/// Queues the distillation of one filed transcript.
pub fn enqueue_digest(
    app: &AppHandle,
    class_id: i64,
    transcript_rel_path: &str,
    date: &str,
) -> Result<i64> {
    let scope = session_scope(transcript_rel_path);
    let (class_dir, prompt, manifest, corpus_rel) = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let transcript = class_dir.join(transcript_rel_path);
        if !transcript.is_file() {
            bail!("no transcript at {transcript_rel_path}");
        }
        // The job row's scope is the transcript path, not the guide scope.
        if crate::guides::has_active_job(conn, class_id, "lecture_digest", transcript_rel_path)? {
            bail!("a session document for this lecture is already queued or running");
        }
        let (class_name, color): (String, String) = conn.query_row(
            "SELECT display_name, color FROM classes WHERE id = ?1",
            [class_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (accent_light, accent_dark) = crate::guides::accent_values(&color);
        // The job reads the transcript with `Read`, which pages. A three-hour
        // lecture runs past a single read, and a digest of the first fraction
        // would come back looking like a complete one.
        let lines = fs::read_to_string(&transcript).map(|t| t.lines().count()).unwrap_or(0);
        // Recorded at filing, but resolved here too: a transcript the sorter
        // placed reached its week through an approved move, and this is the
        // first moment anything asks what that move decided.
        let mapped = contribution_for(conn, class_id, &class_dir, transcript_rel_path)?;
        let prompt = render_prompt(&DigestPromptVars {
            class: &class_name,
            date,
            transcript: transcript_rel_path,
            transcript_lines: lines,
            accent_light,
            accent_dark,
            context: &lecture_context(
                conn,
                class_id,
                transcript_rel_path,
                mapped.as_ref().map(|(unit_id, ..)| *unit_id),
            )?,
            corpus: &corpus_instruction(mapped.as_ref()),
        });
        let manifest =
            serde_json::to_string(&crate::extract::current_manifest(conn, class_id, &scope)?)?;
        let corpus_rel = mapped.map(|(_, _, corpus_rel)| corpus_rel);
        Ok((class_dir, prompt, manifest, corpus_rel))
    })?;

    // Ahead of the run, as the guide and practice paths both do for their own
    // output folders. The job's write tool is scoped to these directories, so
    // leaving them to be created by the write itself puts the very first thing
    // the job does at the mercy of how the pattern treats a path that is not
    // there yet.
    fs::create_dir_all(class_dir.join(SESSIONS_DIR))
        .with_context(|| format!("creating {SESSIONS_DIR}"))?;
    if let Some(corpus_rel) = &corpus_rel {
        if let Some(parent) = class_dir.join(corpus_rel).parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
    }

    let payload = serde_json::to_string(&DigestPayload {
        transcript_rel_path: transcript_rel_path.to_string(),
        date: date.to_string(),
        source_manifest: manifest,
        corpus_rel_path: corpus_rel,
    })?;
    crate::jobs::enqueue_lecture_digest(app, class_id, transcript_rel_path, &prompt, payload)
}

/// The corpus half of the digest prompt: an exact path and what belongs in it,
/// or the reason there is none.
///
/// The path is stated rather than chosen because it is already recorded on the
/// contribution row — the guide reads it from there, so a differently-named
/// file would be one nothing points at.
fn corpus_instruction(mapped: Option<&(i64, String, String)>) -> String {
    match mapped {
        Some((_, unit_name, corpus_rel)) => format!(
            "This session belongs to **{unit_name}**, and its guide is built from the note you \
             write here rather than from the transcript itself — so this file is what that guide \
             will actually read.\n\n\
             Write it to exactly `{corpus_rel}` (that path, not one of your own choosing).\n\n\
             It holds the high-yield content of this session in plain markdown: the substance a \
             study guide would want, dense, with every point carrying its `HH:MM` anchor back to \
             the transcript. Open with one line naming what the session covered. This is not a \
             third copy of the session document — the session document is written for reading, \
             and this is written to be built from, so drop the logistics, the narrative of how \
             the class went, and anything an exam could not touch. Where the session covered a \
             topic only partially, say so: a guide reading this must not present a fragment as a \
             treatment."
        ),
        None => "None for this session. It is not mapped to one of the course's own divisions \
                 — a transcript filed outside `Weeks/`, or a course that publishes no schedule \
                 — so write only the two documents above."
            .into(),
    }
}

/// Everything `lecture_digest.md` expects to be given.
///
/// A struct rather than eight positional arguments so that adding a `{…}` to
/// the prompt without filling it in is a compile error here and a test failure
/// next door — which is how the accent hue reached the model as the literal
/// text `{accent_light}` for the whole of M12.
struct DigestPromptVars<'a> {
    class: &'a str,
    date: &'a str,
    transcript: &'a str,
    transcript_lines: usize,
    accent_light: &'a str,
    accent_dark: &'a str,
    context: &'a str,
    corpus: &'a str,
}

fn render_prompt(vars: &DigestPromptVars<'_>) -> String {
    PROMPT_TEMPLATE
        .replace("{class}", vars.class)
        .replace("{date}", vars.date)
        .replace("{transcript_lines}", &vars.transcript_lines.to_string())
        .replace("{transcript}", vars.transcript)
        .replace("{sessions_dir}", SESSIONS_DIR)
        .replace("{accent_light}", vars.accent_light)
        .replace("{accent_dark}", vars.accent_dark)
        .replace("{context}", vars.context)
        .replace("{corpus}", vars.corpus)
}

/// The material this session was about, so the digest can tie what was said to
/// the slides and readings it was said about.
///
/// Two folders, because a lecture is stored by when it happened and its
/// material by what it is about (SPEC §8.5): the week folder holds anything
/// filed specifically for that session, and the division's own folder — where
/// the course has one — holds the rest.
fn lecture_context(
    conn: &Connection,
    class_id: i64,
    transcript_rel: &str,
    unit_id: Option<i64>,
) -> Result<String> {
    let mut folders: Vec<String> = Vec::new();
    if let Some(week_folder) = transcript_rel.rsplit_once('/').map(|(dir, _)| dir.to_string()) {
        folders.push(week_folder);
    }
    if let Some(unit_id) = unit_id {
        let unit_folder: Option<String> = conn
            .query_row("SELECT rel_path FROM units WHERE id = ?1", [unit_id], |row| {
                row.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten();
        if let Some(folder) = unit_folder.filter(|f| !folders.contains(f)) {
            folders.push(folder);
        }
    }

    let mut stmt = conn.prepare(
        "SELECT rel_path, extract_rel_path FROM files
         WHERE class_id = ?1 AND (rel_path = ?2 OR rel_path LIKE ?3 ESCAPE '\\')
           AND rel_path != ?4
         ORDER BY rel_path",
    )?;
    let mut lines: Vec<String> = Vec::new();
    for folder in &folders {
        let prefix = format!(
            "{}/%",
            folder.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let found = stmt
            .query_map(
                rusqlite::params![class_id, folder, prefix, transcript_rel],
                |row| {
                    let rel: String = row.get(0)?;
                    let extract: Option<String> = row.get(1)?;
                    Ok(match extract {
                        Some(e) => format!("- {rel}\n  extract: {e}"),
                        None => format!("- {rel}"),
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for line in found {
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
    }

    Ok(if lines.is_empty() {
        "(none filed alongside this session yet)".into()
    } else {
        lines.join("\n")
    })
}

/// What the job prints on stdout once it has written both documents.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DigestResult {
    title: String,
    rel_path_html: String,
    rel_path_md: String,
}

pub fn finalize_digest(
    app: &AppHandle,
    class_id: i64,
    payload: &str,
    result_text: &str,
) -> Result<String> {
    let payload: DigestPayload =
        serde_json::from_str(payload).context("parsing digest job payload")?;
    let value = crate::jobs::parse_object(result_text)?;
    let result: DigestResult =
        serde_json::from_value(value).context("digest output is missing its title or paths")?;

    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let class_dir = crate::scanner::class_dir(&conn, class_id)?;

    let recorded = record_session(&conn, class_id, &class_dir, &payload, &result);
    if recorded.is_err() {
        // The job writes before it reports, so a rejected report leaves two
        // files nothing references. `Study Guides` is app-managed, so the
        // scanner never indexes them and the Sessions list is built from the
        // table — they would be invisible and permanent, while chat's search
        // walks that folder and would keep returning them.
        for rel in [&result.rel_path_html, &result.rel_path_md] {
            if let Some(abs) = session_path(&class_dir, rel) {
                let _ = fs::remove_file(abs);
            }
        }
    }
    drop(conn);

    // No hub-change push: this runs before the job row leaves `running`, and
    // the settle edge is what refetches guides — the same path module and
    // master guides take, and the reason `guides::finalize_job` emits nothing.
    recorded
}

/// The absolute path for a reported output, or `None` when the report does not
/// name a file inside the sessions folder. Compared by component rather than by
/// byte prefix, so neither `..` nor a sibling named `Sessions-old` passes.
fn session_path(class_dir: &Path, rel: &str) -> Option<PathBuf> {
    let path = Path::new(rel);
    if path
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return None;
    }
    path.starts_with(SESSIONS_DIR).then(|| class_dir.join(path))
}

fn session_scope(transcript_rel_path: &str) -> String {
    format!("{SESSION_SCOPE_PREFIX}{transcript_rel_path}")
}

/// Verifies the two contracted documents exist where the job says it put them,
/// then records the digest. Both halves are checked because "wrote the HTML,
/// skipped the markdown" would otherwise pass as success and quietly leave the
/// digest out of chat's search scope.
fn record_session(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    payload: &DigestPayload,
    result: &DigestResult,
) -> Result<String> {
    // One file reported twice passes an element-wise check on both counts, and
    // is exactly the half-written outcome checking both is meant to catch.
    if result.rel_path_html == result.rel_path_md {
        bail!(
            "the digest reported one file for both documents ({})",
            result.rel_path_html
        );
    }
    if !result.rel_path_html.ends_with(".html") || !result.rel_path_md.ends_with(".md") {
        bail!(
            "the digest reported {} and {}, not an .html and a .md",
            result.rel_path_html,
            result.rel_path_md
        );
    }
    for rel in [&result.rel_path_html, &result.rel_path_md] {
        let Some(abs) = session_path(class_dir, rel) else {
            bail!("the digest reported {rel}, outside {SESSIONS_DIR}/");
        };
        let written = fs::metadata(abs)
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !written {
            bail!("no session document written at {rel}");
        }
    }
    // The corpus note is checked on the same terms, and for the sharper reason:
    // a contribution row names it, so a missing note is a unit guide quietly
    // built without the lecture it was supposed to be built from. The job asks
    // for it by an exact path, so there is nothing to interpret here.
    if let Some(corpus_rel) = &payload.corpus_rel_path {
        let written = fs::metadata(class_dir.join(corpus_rel))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !written {
            bail!("no corpus note written at {corpus_rel}");
        }
    }

    let scope = session_scope(&payload.transcript_rel_path);
    let superseded: Option<String> = conn
        .query_row(
            "SELECT rel_path FROM guides WHERE class_id = ?1 AND scope = ?2",
            rusqlite::params![class_id, &scope],
            |row| row.get(0),
        )
        .optional()?;

    conn.execute(
        "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(class_id, scope) DO UPDATE SET
           rel_path = excluded.rel_path,
           generated_at = excluded.generated_at,
           source_manifest = excluded.source_manifest",
        rusqlite::params![
            class_id,
            scope,
            result.rel_path_html,
            now(),
            payload.source_manifest
        ],
    )?;

    // The job names its own file, so a re-run under a different topic leaves
    // the previous pair on disk with nothing pointing at it — and chat's search
    // walks `Study Guides/`, so it would go on answering from the old one.
    if let Some(old) = superseded.filter(|p| *p != result.rel_path_html) {
        for rel in [old.clone(), format!("{}.md", old.trim_end_matches(".html"))] {
            if let Some(abs) = session_path(class_dir, &rel) {
                let _ = fs::remove_file(abs);
            }
        }
    }

    // The row's summary was a placeholder from filing time. Now that the
    // session has been read, it carries what the session was about — which is
    // what the Lectures listing shows beside the division it feeds.
    if payload.corpus_rel_path.is_some() {
        conn.execute(
            "UPDATE lecture_contributions SET summary = ?1
             WHERE class_id = ?2 AND rel_path = ?3",
            rusqlite::params![
                crate::db::truncate(result.title.trim(), 120),
                class_id,
                payload.transcript_rel_path
            ],
        )?;
    }
    Ok(format!("{} · {}", result.title, payload.date))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder per test and per process, so two `cargo test` runs
    /// side by side cannot rename or delete each other's fixtures.
    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{name}-{}", std::process::id()))
    }

    #[test]
    fn builds_a_dated_transcript_name() {
        assert_eq!(
            transcript_file_name("2026-08-24", "Lecture").unwrap(),
            "2026-08-24 — Lecture.md"
        );
    }

    /// A title is free text from a dialog; a slash in it must not repoint the
    /// write into another folder.
    #[test]
    fn flattens_path_separators_in_a_title() {
        let name = transcript_file_name("2026-08-24", "Week 3/4 — recap").unwrap();
        assert!(!name.contains('/'), "{name}");
        assert_eq!(name, "2026-08-24 — Week 3-4 — recap.md");
    }

    #[test]
    fn rejects_an_empty_or_overlong_title() {
        assert!(transcript_file_name("2026-08-24", "   ").is_err());
        assert!(transcript_file_name("2026-08-24", &"x".repeat(81)).is_err());
    }

    /// The digest job writes through the ordinary job write-contract. If
    /// `SESSIONS_DIR` ever moved out from under `GUIDES_DIR`, every digest run
    /// would be demoted as an out-of-contract write — with nothing else in the
    /// codebase to say why. The corpus note is the second destination, and
    /// hidden rather than app-managed, which is what keeps it out of the
    /// fingerprint diff.
    #[test]
    fn keeps_the_digest_inside_the_contracted_write_scope() {
        assert!(SESSIONS_DIR.starts_with(crate::db::GUIDES_DIR), "{SESSIONS_DIR}");
        assert!(crate::db::JOB_WRITABLE.contains(&crate::db::GUIDES_DIR));
        assert!(crate::db::JOB_WRITABLE.contains(&CORPUS_DIR));
        assert!(CORPUS_DIR.starts_with('.'), "{CORPUS_DIR}");
    }

    /// A guide reads the corpus note by the path on the contribution row, so
    /// the two have to be derived the same way from the same transcript.
    #[test]
    fn keys_a_corpus_note_by_its_unit_and_its_transcript() {
        assert_eq!(
            corpus_rel_path(
                "Week 2 \u{2014} Study Designs",
                "Weeks/Week 02 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md"
            ),
            ".classhub/corpus/Week 2 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md"
        );
        // A unit name is the course's own words, and a course may well punctuate
        // one with a colon — which would repoint the write.
        let path = corpus_rel_path("Part I: LLMs", "Weeks/Week 01/2026-08-25 \u{2014} Lecture.md");
        assert_eq!(
            path,
            ".classhub/corpus/Part I- LLMs/2026-08-25 \u{2014} Lecture.md"
        );
        assert!(!Path::new(&path).is_absolute() && !path.contains(".."), "{path}");
    }

    /// The span columns describe the artifact on disk, so they read back off it.
    #[test]
    fn reads_a_lecture_s_span_off_the_filed_markdown() {
        let timed = "# Lecture\n\nRecorded 2026-08-27\n\n## 00:00\n\nHello.\n\n## 01:05\n\nBye.\n";
        assert_eq!(span_of(timed), (65 * 60_000, 11));
        // Transcribed without timings: a zero-length span, never an invented one.
        let untimed = "# Lecture\n\nsource: x.txt\n\nHello.\n";
        assert_eq!(span_of(untimed).0, 0);
        assert_eq!(span_of("").1, 1, "an empty file still spans one line");
    }

    /// The digest job has an output contract, and the prompt is the only place
    /// it is stated. An unfilled `{…}` reaches the model as literal text, which
    /// is what happened to the accent hue for the whole of M12 — the prompt
    /// asked for a colour nothing ever substituted, and no test could see it.
    #[test]
    fn fills_every_placeholder_in_the_digest_prompt() {
        let corpus = corpus_instruction(Some(&(
            7,
            "Week 2 — Study Designs".into(),
            ".classhub/corpus/Week 2 — Study Designs/2026-08-24 — Lecture.md".into(),
        )));
        let rendered = render_prompt(&DigestPromptVars {
            class: "Biostatistics for AI",
            date: "2026-08-24",
            transcript: "Weeks/Week 02 — Study Designs/2026-08-24 — Lecture.md",
            transcript_lines: 4_210,
            accent_light: "oklch(0.578 0.135 158)",
            accent_dark: "oklch(0.732 0.13 158)",
            context: "- Module 1/slides.pdf",
            corpus: &corpus,
        });

        // A leftover reads as `{lower_snake}`; the JSON contract's own braces
        // open with a quote, so they are not mistaken for one.
        let placeholders = |text: &str| {
            let mut found: Vec<String> = Vec::new();
            let mut rest = text;
            while let Some(open) = rest.find('{') {
                rest = &rest[open + 1..];
                if let Some(close) = rest.find('}') {
                    let inner = &rest[..close];
                    if !inner.is_empty()
                        && inner.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    {
                        found.push(inner.to_string());
                    }
                }
            }
            found
        };
        // The template really does carry them, so an empty result below means
        // they were filled rather than that nothing was ever looked for.
        assert!(
            placeholders(PROMPT_TEMPLATE).contains(&"accent_light".to_string()),
            "the placeholder scan finds nothing to miss"
        );
        assert!(
            placeholders(&rendered).is_empty(),
            "unsubstituted placeholders: {:?}",
            placeholders(&rendered)
        );

        // And the values actually landed, rather than the template being empty.
        for expected in [
            "Biostatistics for AI",
            "2026-08-24",
            "4210 lines",
            "oklch(0.578 0.135 158)",
            SESSIONS_DIR,
            ".classhub/corpus/Week 2 — Study Designs/2026-08-24 — Lecture.md",
        ] {
            assert!(rendered.contains(expected), "missing {expected:?}");
        }

        // A lecture that maps to no division still gets its session documents,
        // and the prompt has to say there is no note rather than leaving the
        // instruction half-filled.
        let unmapped = render_prompt(&DigestPromptVars {
            corpus: &corpus_instruction(None),
            ..DigestPromptVars {
                class: "AI in Health Design Studio I",
                date: "2026-08-26",
                transcript: "_Inbox/2026-08-26 — Lecture.md",
                transcript_lines: 12,
                accent_light: "oklch(0.646 0.175 45)",
                accent_dark: "oklch(0.748 0.145 50)",
                context: "(none filed alongside this session yet)",
                corpus: "",
            }
        });
        assert!(placeholders(&unmapped).is_empty(), "{:?}", placeholders(&unmapped));
        assert!(!unmapped.contains(CORPUS_DIR), "asked for a note it has no path for");
    }

    /// The one check standing between a model-chosen string and a write path.
    #[test]
    fn only_accepts_a_reported_path_inside_the_sessions_folder() {
        let root = Path::new("/tmp/classhub-test");
        let ok = session_path(root, "Study Guides/Sessions/2026-08-24 — Attention.html");
        assert_eq!(ok, Some(root.join("Study Guides/Sessions/2026-08-24 — Attention.html")));

        // Traversal, absolute, and a sibling that a byte-prefix check accepts.
        assert_eq!(session_path(root, "Study Guides/Sessions/../../../.zshrc"), None);
        assert_eq!(session_path(root, "/Users/danny/.zshrc"), None);
        assert_eq!(session_path(root, "Study Guides/Sessions-old/x.html"), None);
        assert_eq!(session_path(root, "Notes/x.html"), None);
        assert_eq!(session_path(root, ""), None);
    }

    /// A pair that is really one file passes an element-wise existence check on
    /// both counts, which is the failure checking both was added to prevent.
    #[test]
    fn rejects_a_digest_that_reported_one_file_twice() {
        let dir = scratch("classhub-digest-pair");
        let sessions = dir.join(SESSIONS_DIR);
        fs::create_dir_all(&sessions).expect("sessions dir");
        let html = "Study Guides/Sessions/2026-08-24 — Attention.html";
        fs::write(dir.join(html), "<html></html>").expect("html");

        let conn = Connection::open_in_memory().expect("conn");
        let payload = DigestPayload {
            transcript_rel_path: "Weeks/Week 02 — Study Designs/2026-08-24 — Lecture.md".into(),
            date: "2026-08-24".into(),
            source_manifest: "[]".into(),
            corpus_rel_path: None,
        };
        let same = DigestResult {
            title: "Attention".into(),
            rel_path_html: html.into(),
            rel_path_md: html.into(),
        };
        let err = record_session(&conn, 1, &dir, &payload, &same).expect_err("one file");
        assert!(format!("{err:#}").contains("one file for both"), "{err:#}");

        // The markdown genuinely missing is caught too, and named.
        let missing = DigestResult {
            rel_path_md: "Study Guides/Sessions/2026-08-24 — Attention.md".into(),
            ..same
        };
        let err = record_session(&conn, 1, &dir, &payload, &missing).expect_err("no md");
        assert!(format!("{err:#}").contains("no session document"), "{err:#}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// The quiet failure M14 exists to prevent: both session documents land, so
    /// the run looks complete, while the unit guide is left built from a
    /// contribution row whose note was never written.
    #[test]
    fn rejects_a_digest_that_skipped_the_corpus_note() {
        let dir = scratch("classhub-digest-corpus");
        fs::create_dir_all(dir.join(SESSIONS_DIR)).expect("sessions dir");
        let html = "Study Guides/Sessions/2026-08-24 — Study Designs.html";
        let md = "Study Guides/Sessions/2026-08-24 — Study Designs.md";
        fs::write(dir.join(html), "<html></html>").expect("html");
        fs::write(dir.join(md), "# Study Designs").expect("md");

        let corpus_rel = ".classhub/corpus/Week 2 — Study Designs/2026-08-24 — Lecture.md";
        let conn = crate::db::memory_db();
        let payload = DigestPayload {
            transcript_rel_path: "Weeks/Week 02 — Study Designs/2026-08-24 — Lecture.md".into(),
            date: "2026-08-24".into(),
            source_manifest: "[]".into(),
            corpus_rel_path: Some(corpus_rel.into()),
        };
        let result = DigestResult {
            title: "Study Designs".into(),
            rel_path_html: html.into(),
            rel_path_md: md.into(),
        };
        let err = record_session(&conn, 1, &dir, &payload, &result).expect_err("no note");
        assert!(format!("{err:#}").contains("no corpus note"), "{err:#}");

        // With the note on disk it records, and the row's placeholder summary
        // is replaced by what the session turned out to be about.
        fs::create_dir_all(dir.join(corpus_rel).parent().expect("parent")).expect("corpus dir");
        fs::write(dir.join(corpus_rel), "# Study Designs\n").expect("note");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, source)
             VALUES (1, 2, 'week', 'Week 2 — Study Designs', 'syllabus')",
            [],
        )
        .expect("unit");
        record_contribution(
            &conn,
            1,
            &dir,
            &crate::units::slot_for_week(&conn, 1, 2).expect("slots").expect("slot"),
            &payload.transcript_rel_path,
            "# Lecture\n\n## 00:00\n\nHello.\n",
        )
        .expect("contribution");
        record_session(&conn, 1, &dir, &payload, &result).expect("records");
        let summary: String = conn
            .query_row(
                "SELECT summary FROM lecture_contributions WHERE class_id = 1",
                [],
                |row| row.get(0),
            )
            .expect("summary");
        assert_eq!(summary, "Study Designs");

        let _ = fs::remove_dir_all(&dir);
    }

    /// Refiling is the correction affordance for a wrong week (SPEC §8.5), so
    /// the map has to move with the file — and move, not multiply.
    #[test]
    fn refiling_a_lecture_moves_its_contribution_rather_than_adding_one() {
        let root = scratch("classhub-refile");
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        // `list_contributions` resolves the class folder to test whether each
        // note is on disk, so the fixture has to be where the settings say.
        let folder: String = conn
            .query_row("SELECT folder_name FROM classes WHERE id = 1", [], |row| row.get(0))
            .expect("class");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let dir = root.join(folder);
        for (ordinal, name) in [(2, "Week 2 — Study Designs"), (3, "Week 3 — Data Exploration")] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, source)
                 VALUES (1, ?1, 'week', ?2, 'syllabus')",
                rusqlite::params![ordinal, name],
            )
            .expect("unit");
        }
        let markdown = "# Lecture\n\n## 00:00\n\nHello.\n";
        let file = |week: &str| format!("Weeks/{week}/2026-08-27 — Lecture.md");
        let from = file("Week 02 — Study Designs");
        let to = file("Week 03 — Data Exploration");
        for rel in [&from, &to] {
            fs::create_dir_all(dir.join(rel).parent().expect("parent")).expect("week dir");
        }
        fs::write(dir.join(&from), markdown).expect("transcript");

        let slot = crate::units::slot_for_week(&conn, 1, 2).expect("slots").expect("slot");
        let corpus = record_contribution(&conn, 1, &dir, &slot, &from, markdown).expect("record");
        fs::create_dir_all(dir.join(&corpus).parent().expect("parent")).expect("corpus dir");
        fs::write(dir.join(&corpus), "# note").expect("note");
        // As the digest leaves it: the row carries the session's own title.
        conn.execute(
            "UPDATE lecture_contributions SET summary = 'Study Designs' WHERE class_id = 1",
            [],
        )
        .expect("summary");

        // The move itself, then the map catching up with it. The note's own
        // move is decided now and performed after the commit: until it is
        // applied, the note is exactly where the rolled-back row would say.
        fs::rename(dir.join(&from), dir.join(&to)).expect("move");
        let effects = refile_lecture(&conn, 1, &dir, &from, &to).expect("refile");
        assert!(effects.note.is_some(), "a note to move");
        assert!(dir.join(&corpus).is_file(), "the note moved before the commit");
        effects.apply();

        let rows = list_contributions(&conn, 1).expect("list");
        assert_eq!(rows.len(), 1, "the refile duplicated the contribution");
        assert_eq!(rows[0].rel_path, to);
        assert_eq!(rows[0].unit_name, "Week 3 — Data Exploration");
        assert!(rows[0].corpus_rel_path.contains("Week 3 — Data Exploration"));
        assert!(
            !dir.join(&corpus).exists(),
            "the old unit kept a note for a lecture that left it"
        );
        // The distillation is the transcript's, not the unit's: the note moved
        // with the lecture rather than being spent again on a redistill.
        assert!(rows[0].distilled, "the note did not follow the lecture");
        assert_eq!(
            fs::read_to_string(dir.join(&rows[0].corpus_rel_path)).expect("moved note"),
            "# note"
        );
        assert_eq!(rows[0].summary, "Study Designs", "the digest's title was reset");
        let moved_note = rows[0].corpus_rel_path.clone();

        // And moving it out of Weeks/ entirely leaves nothing mapping a guide
        // to a transcript that is no longer there — the note included.
        let out = "Module 1/2026-08-27 — Lecture.md".to_string();
        fs::create_dir_all(dir.join("Module 1")).expect("module dir");
        fs::rename(dir.join(&to), dir.join(&out)).expect("move out");
        let effects = refile_lecture(&conn, 1, &dir, &to, &out).expect("refile out");
        assert!(matches!(effects.note, Some(NoteMove::Remove(_))), "a note to remove");
        effects.apply();
        assert!(list_contributions(&conn, 1).expect("list").is_empty());
        assert!(!dir.join(&moved_note).exists(), "a note survived with no unit reading it");

        let _ = fs::remove_dir_all(&root);
    }

    /// A week the course has not declared yet is not the same as leaving
    /// `Weeks/`: the row goes, since nothing maps, but the note stays for the
    /// sync that declares the week or the refile that comes back.
    #[test]
    fn keeps_the_note_when_the_destination_week_is_undeclared() {
        let root = scratch("classhub-refile-undeclared");
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        let folder: String = conn
            .query_row("SELECT folder_name FROM classes WHERE id = 1", [], |row| row.get(0))
            .expect("class");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let dir = root.join(folder);
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, source)
             VALUES (1, 2, 'week', 'Week 2 — Study Designs', 'syllabus')",
            [],
        )
        .expect("unit");
        let markdown = "# Lecture\n\n## 00:00\n\nHello.\n";
        let from = "Weeks/Week 02 — Study Designs/2026-08-27 — Lecture.md";
        let to = "Weeks/Week 07/2026-08-27 — Lecture.md";
        for rel in [from, to] {
            fs::create_dir_all(dir.join(rel).parent().expect("parent")).expect("week dir");
        }
        fs::write(dir.join(from), markdown).expect("transcript");
        let slot = crate::units::slot_for_week(&conn, 1, 2).expect("slots").expect("slot");
        let corpus = record_contribution(&conn, 1, &dir, &slot, from, markdown).expect("record");
        fs::create_dir_all(dir.join(&corpus).parent().expect("parent")).expect("corpus dir");
        fs::write(dir.join(&corpus), "# note").expect("note");

        fs::rename(dir.join(from), dir.join(to)).expect("move");
        let effects = refile_lecture(&conn, 1, &dir, from, to).expect("refile");
        assert!(effects.note.is_none(), "an undeclared week is not a reason to touch the note");
        effects.apply();
        assert!(list_contributions(&conn, 1).expect("list").is_empty(), "nothing maps yet");
        assert_eq!(fs::read_to_string(dir.join(&corpus)).expect("note"), "# note");

        // Back to the declared week: the row returns and finds its note.
        fs::rename(dir.join(to), dir.join(from)).expect("move back");
        let effects = refile_lecture(&conn, 1, &dir, to, from).expect("refile back");
        assert!(effects.note.is_none());
        effects.apply();
        let rows = list_contributions(&conn, 1).expect("list");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].distilled, "the note was there to be found");

        let _ = fs::remove_dir_all(&root);
    }

    /// Notes are keyed by unit and transcript name, so a unit spanning several
    /// weeks can already hold a note at the path a refiled lecture's would take.
    /// `fs::rename` replaces silently; the refile has to refuse instead, inside
    /// the transaction, so the whole move rolls back.
    #[test]
    fn refuses_to_move_a_note_onto_another_lecture_s() {
        let root = scratch("classhub-refile-collision");
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        let folder: String = conn
            .query_row("SELECT folder_name FROM classes WHERE id = 1", [], |row| row.get(0))
            .expect("class");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let dir = root.join(folder);
        // One Part covering weeks 1–8: two week folders, one corpus folder.
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, first_week, last_week, source)
             VALUES (1, 1, 'part', 'Part I: Foundations (Weeks 1-8)', 1, 8, 'syllabus')",
            [],
        )
        .expect("unit");
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, first_week, last_week, source)
             VALUES (1, 2, 'part', 'Part II: Models (Weeks 9-16)', 9, 16, 'syllabus')",
            [],
        )
        .expect("unit");
        let markdown = "# Lecture\n\n## 00:00\n\nHello.\n";
        let a = "Weeks/Week 08/2026-09-01 — Lecture.md";
        let a_moved = "Weeks/Week 09/2026-09-01 — Lecture.md";
        let b = "Weeks/Week 12/2026-09-01 — Lecture.md";
        for rel in [a, a_moved, b] {
            fs::create_dir_all(dir.join(rel).parent().expect("parent")).expect("week dir");
        }
        fs::write(dir.join(a), markdown).expect("a");
        fs::write(dir.join(b), markdown).expect("b");
        for (rel, week) in [(a, 8), (b, 12)] {
            let slot = crate::units::slot_for_week(&conn, 1, week).expect("slots").expect("slot");
            let corpus = record_contribution(&conn, 1, &dir, &slot, rel, markdown).expect("record");
            fs::create_dir_all(dir.join(&corpus).parent().expect("parent")).expect("corpus dir");
            fs::write(dir.join(&corpus), format!("# note for {rel}")).expect("note");
        }

        fs::rename(dir.join(a), dir.join(a_moved)).expect("move");
        let err = refile_lecture(&conn, 1, &dir, a, a_moved).expect_err("a collision");
        assert!(format!("{err:#}").contains("already feeds"), "the row guard: {err:#}");
        // B's distillation is untouched either way.
        let b_note = list_contributions(&conn, 1)
            .expect("list")
            .into_iter()
            .find(|row| row.rel_path == b)
            .expect("b's row")
            .corpus_rel_path;
        assert_eq!(
            fs::read_to_string(dir.join(&b_note)).expect("b's note"),
            format!("# note for {b}")
        );

        // B's row gone but its note still on disk — a transcript deleted in
        // Finder leaves exactly that — the row guard has nothing to hold, and
        // the note itself is what refuses the move.
        conn.execute("DELETE FROM lecture_contributions WHERE rel_path = ?1", [b]).expect("drop b");
        let slot = crate::units::slot_for_week(&conn, 1, 8).expect("slots").expect("slot");
        record_contribution(&conn, 1, &dir, &slot, a, markdown).expect("a's row back");
        let err = refile_lecture(&conn, 1, &dir, a, a_moved).expect_err("a note on disk");
        assert!(format!("{err:#}").contains("already exists"), "the disk guard: {err:#}");
        assert_eq!(
            fs::read_to_string(dir.join(&b_note)).expect("b's note"),
            format!("# note for {b}"),
            "the orphan note was touched"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A note is keyed by its division and its transcript's name, and a Part
    /// spans several week folders — so two lectures named alike under Week 02
    /// and Week 03 derive one note path, and the second digest would write
    /// over the first note with both rows naming it. Refused at the row, so
    /// the filing and the sorter's path both stop short of that; a name of
    /// its own is the way out, a re-run of the first is not a collision with
    /// itself, and a holder whose transcript is gone is a stale row that
    /// gives the name up.
    #[test]
    fn a_part_holds_one_note_per_transcript_name() {
        let dir = scratch("classhub-part-note-names");
        let _ = fs::remove_dir_all(&dir);
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I: Deep Learning to Large Language Models', 1, 8, 'syllabus')",
            [],
        )
        .expect("unit");
        let markdown = "# Lecture\n\n## 00:00\n\nHello.\n";
        let week2 = crate::units::slot_for_week(&conn, 4, 2).expect("slots").expect("week 2");
        let week3 = crate::units::slot_for_week(&conn, 4, 3).expect("slots").expect("week 3");
        assert_eq!(week2.unit_id, week3.unit_id, "both weeks are Part I's");
        assert_eq!((week2.folder.as_str(), week3.folder.as_str()), ("Week 02", "Week 03"));

        let first = "Weeks/Week 02/2026-09-01 — Lecture.md";
        fs::create_dir_all(dir.join(first).parent().expect("parent")).expect("week dir");
        fs::write(dir.join(first), markdown).expect("first on disk");
        let note = record_contribution(&conn, 4, &dir, &week2, first, markdown).expect("first");
        assert_eq!(
            note,
            ".classhub/corpus/Part I- Deep Learning to Large Language Models/2026-09-01 — Lecture.md"
        );
        let second = "Weeks/Week 03/2026-09-01 — Lecture.md";
        assert_eq!(corpus_rel_path(&week3.unit_name, second), note, "one path for two names");
        let err = record_contribution(&conn, 4, &dir, &week3, second, markdown).expect_err("held");
        let message = format!("{err:#}");
        assert!(message.contains(first) && message.contains("name of its own"), "{message}");

        let rows = || -> Vec<(String, String)> {
            let mut stmt = conn
                .prepare(
                    "SELECT rel_path, corpus_rel_path FROM lecture_contributions
                     WHERE class_id = 4 ORDER BY rel_path",
                )
                .expect("prepare");
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .expect("query")
                .collect::<rusqlite::Result<Vec<_>>>()
                .expect("rows")
        };
        assert_eq!(rows(), vec![(first.to_string(), note.clone())], "the first row was touched");

        // A title of its own derives a path of its own.
        let titled = "Weeks/Week 03/2026-09-01 — Guest lecture.md";
        let other = record_contribution(&conn, 4, &dir, &week3, titled, markdown).expect("titled");
        assert_ne!(other, note);
        assert_eq!(rows().len(), 2);

        // The first lecture recorded again — a re-run — is not a collision.
        record_contribution(&conn, 4, &dir, &week2, first, markdown).expect("re-record");
        assert_eq!(rows().len(), 2);

        // Deleted in Finder, the first transcript's row lingers — no scan
        // clears it — and must not refuse a real lecture in its name: the
        // stale row goes and the newcomer takes the name.
        fs::remove_file(dir.join(first)).expect("delete in Finder");
        let taken = record_contribution(&conn, 4, &dir, &week3, second, markdown).expect("stale row cleared");
        assert_eq!(taken, note);
        assert_eq!(
            rows().iter().map(|(rel, _)| rel.as_str()).collect::<Vec<_>>(),
            vec![titled, second],
            "the stale row survived"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// What the form settles before a capture: the week's folder or the
    /// inbox, and a refusal — for a name another of the Part's transcripts
    /// holds, or for a note already on disk — that costs nothing because
    /// nothing has been captured yet. The same name in the same week folder
    /// takes its suffix and is no collision at all.
    #[test]
    fn a_filing_is_settled_before_the_capture() {
        let root = scratch("classhub-resolve-filing");
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        let folder: String = conn
            .query_row("SELECT folder_name FROM classes WHERE id = 4", [], |row| row.get(0))
            .expect("class");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let dir = root.join(folder);
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I: Deep Learning to Large Language Models', 1, 8, 'syllabus')",
            [],
        )
        .expect("unit");
        let markdown = "# Lecture\n\n## 00:00\n\nHello.\n";
        let name = "2026-09-01 — Lecture.md";

        // No week: the inbox, and no division to refuse for.
        let inbox = resolve_filing(&conn, 4, None, name).expect("inbox");
        assert!(inbox.slot.is_none());
        assert_eq!(inbox.dir_rel, "_Inbox");

        // Week 2: the bare folder of a Part-numbered course, feeding Part I.
        let week2 = resolve_filing(&conn, 4, Some(2), name).expect("week 2");
        assert_eq!(week2.dir_rel, "Weeks/Week 02");
        assert_eq!(week2.slot.as_ref().map(|s| s.unit_name.as_str()), Some("Part I: Deep Learning to Large Language Models"));

        // Filed and recorded there, the same name for Week 3 is refused
        // before a capture; the same name for Week 2 takes ` (2)` instead.
        let first = format!("{}/{name}", week2.dir_rel);
        fs::create_dir_all(dir.join(&week2.dir_rel)).expect("week dir");
        fs::write(dir.join(&first), markdown).expect("first");
        let slot = week2.slot.as_ref().expect("slot");
        let note = record_contribution(&conn, 4, &dir, slot, &first, markdown).expect("record");
        let err = resolve_filing(&conn, 4, Some(3), name).err().expect("held name");
        assert!(format!("{err:#}").contains(&first), "{err:#}");
        resolve_filing(&conn, 4, Some(2), name).expect("a suffixed name in the same folder");
        resolve_filing(&conn, 4, Some(3), "2026-09-01 — Guest lecture.md").expect("a title of its own");

        // A note on disk with no row behind it is refused too: the form has
        // a title field, and the note is someone's distillation.
        conn.execute("DELETE FROM lecture_contributions WHERE class_id = 4", []).expect("clear");
        fs::create_dir_all(dir.join(&note).parent().expect("parent")).expect("corpus dir");
        fs::write(dir.join(&note), "# note").expect("orphan note");
        let err = resolve_filing(&conn, 4, Some(3), name).err().expect("a note on disk");
        assert!(format!("{err:#}").contains("already exists"), "{err:#}");
        fs::remove_file(dir.join(&note)).expect("remove note");
        resolve_filing(&conn, 4, Some(3), name).expect("free again");

        let _ = fs::remove_dir_all(&root);
    }

    /// Within one week folder the never-overwrite rule already keeps two
    /// transcripts apart, and the note path follows the suffixed name — so a
    /// week-numbered course never meets the Part's one-note-per-name rule.
    #[test]
    fn a_second_lecture_in_one_week_folder_takes_a_suffix() {
        let dir = scratch("classhub-unique-name");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Weeks/Week 02")).expect("week dir");
        let first = unique_rel_path(&dir, "Weeks/Week 02", "2026-09-01 — Lecture.md");
        assert_eq!(first, "Weeks/Week 02/2026-09-01 — Lecture.md");
        fs::write(dir.join(&first), "x").expect("first");
        let second = unique_rel_path(&dir, "Weeks/Week 02", "2026-09-01 — Lecture.md");
        assert_eq!(second, "Weeks/Week 02/2026-09-01 — Lecture (2).md");
        assert_ne!(corpus_rel_path("Part I", &first), corpus_rel_path("Part I", &second));
        let _ = fs::remove_dir_all(&dir);
    }

    /// A session document is keyed by its transcript's path, so a refile that
    /// left the row alone would drop the digest from the Lectures listing and
    /// mark it stale over a rename — a redistill of a three-hour lecture to
    /// buy back nothing.
    #[test]
    fn a_session_document_follows_its_refiled_transcript() {
        let conn = crate::db::memory_db();
        let dir = scratch("classhub-refile-session");
        let from = "Weeks/Week 02 — Study Designs/2026-08-27 — Lecture.md";
        let to = "Weeks/Week 03 — Data Exploration/2026-08-27 — Lecture.md";
        let manifest = serde_json::json!([{ "relPath": from, "sha256": "abc" }]).to_string();
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (1, ?1, 'Study Guides/Sessions/2026-08-27 — Designs.html', 1, ?2)",
            rusqlite::params![session_scope(from), manifest],
        )
        .expect("session row");

        // No units declared, so nothing maps — the session still follows.
        refile_lecture(&conn, 1, &dir, from, to).expect("refile").apply();
        let (scope, manifest): (String, String) = conn
            .query_row(
                "SELECT scope, source_manifest FROM guides WHERE class_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("one row");
        assert_eq!(scope, session_scope(to));
        let entries: Vec<crate::extract::ManifestEntry> =
            serde_json::from_str(&manifest).expect("manifest");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rel_path, to, "the manifest still names the old path");
        assert_eq!(entries[0].sha256, "abc");

        // A manifest that does not parse is left as it is — stale, the way
        // `manifest_is_stale` already reads it — rather than failing the move.
        let back = "Weeks/Week 02 — Study Designs/2026-08-27 — Lecture.md";
        conn.execute(
            "UPDATE guides SET source_manifest = 'not json' WHERE class_id = 1",
            [],
        )
        .expect("corrupt");
        refile_lecture(&conn, 1, &dir, to, back).expect("refile despite it").apply();
        let (scope, manifest): (String, String) = conn
            .query_row(
                "SELECT scope, source_manifest FROM guides WHERE class_id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("one row");
        assert_eq!(scope, session_scope(back));
        assert_eq!(manifest, "not json");
    }

    /// A digested transcript deleted in Finder with no rescan leaves its
    /// session row behind. A transcript later moved onto that exact path must
    /// not inherit it — the row would show the newcomer as digested and open
    /// the other lecture's document — and the pair goes with the row, after
    /// the commit, so chat's search stops finding it.
    #[test]
    fn a_stale_session_row_at_the_destination_is_cleared_with_its_documents() {
        let conn = crate::db::memory_db();
        let dir = scratch("classhub-refile-stale-session");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(SESSIONS_DIR)).expect("sessions dir");
        let from = "Weeks/Week 02 — Study Designs/2026-08-27 — Lecture.md";
        let to = "Weeks/Week 03 — Data Exploration/2026-08-27 — Lecture.md";
        let html = "Study Guides/Sessions/2026-08-27 — Vanished.html";
        let md = "Study Guides/Sessions/2026-08-27 — Vanished.md";
        fs::write(dir.join(html), "<html></html>").expect("html");
        fs::write(dir.join(md), "# Vanished").expect("md");
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (1, ?1, ?2, 1, '[]')",
            rusqlite::params![session_scope(to), html],
        )
        .expect("stale row");

        // The incoming transcript has no session row of its own.
        let effects = refile_lecture(&conn, 1, &dir, from, to).expect("refile");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM guides WHERE class_id = 1", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 0, "the stale row survived the refile");
        assert!(dir.join(html).is_file(), "documents removed before the commit");
        assert_eq!(effects.orphaned.len(), 2);
        effects.apply();
        assert!(!dir.join(html).exists() && !dir.join(md).exists(), "the pair outlived its row");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A slide deck the sorter files under a week is not a lecture, and a
    /// contribution row for one would put a PDF in a guide's corpus listing.
    #[test]
    fn only_markdown_in_a_week_folder_counts_as_a_lecture() {
        assert!(is_filed_transcript("Weeks/Week 03 — Transformers/2026-09-10 — Lecture.md"));
        assert!(!is_filed_transcript("Weeks/Week 03 — Transformers/Slides/deck.pdf"));
        assert!(!is_filed_transcript("Module 1/2026-09-10 — Lecture.md"));
        assert!(!is_filed_transcript("Weeks/Loose/2026-09-10 — Lecture.md"));
    }

    #[test]
    fn recognises_a_session_scope() {
        assert!(crate::db::is_session_scope(
            "session:Weeks/Week 02 — Study Designs/2026-08-24 — Lecture.md"
        ));
        assert!(!crate::db::is_session_scope("Module 1"));
        assert!(!crate::db::is_session_scope("master"));
    }
}
