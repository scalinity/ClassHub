//! SPEC §8.6 — a note against the room. A note whose title opens with a
//! meeting's date is that meeting's note; once the session document exists
//! and the note carries no `## Against the room` heading, a light-tier,
//! read-only job reads both and answers with the section — what the note
//! has that the room did, what the room had that the note missed, where the
//! two disagree — and the app appends it through the note write path under
//! `review.write_note`, the previous content parked in the audit row, so it
//! is one `Undo` away like a save. The job never writes: the guard
//! fingerprints `Notes/`, and only the app can park the previous content.

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::db::{emit_hub_change, notify, with_conn, NOTES_DIR};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/notes_review.md");
pub const KIND: &str = "notes_review";
/// The heading the section opens with; a note that carries it is reviewed.
pub(crate) const HEADING: &str = "## Against the room";
/// A note in the prompt, and the section the answer may carry.
const MAX_NOTE_CHARS: usize = 40_000;
const MAX_SECTION_CHARS: usize = 12_000;

/// The meeting date a note's title opens with (`2026-09-10 — In class`), if
/// any. The title is the date alone, or the date, a separator and words of
/// the owner's own: the editor offers `<date> — ` on every new note, and a
/// title left at that is nothing the owner dated for a meeting.
pub(crate) fn note_date(title: &str) -> Option<NaiveDate> {
    let title = title.trim();
    let date: String = title.chars().take(10).collect();
    let parsed = NaiveDate::parse_from_str(&date, "%Y-%m-%d").ok()?;
    let rest = title.chars().skip(10).collect::<String>();
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '\u{2013}' | '\u{2014}' | ':' | '·'));
    if rest.is_empty() && !title.chars().skip(10).any(|c| !c.is_whitespace()) {
        return Some(parsed);
    }
    rest.chars().any(char::is_alphanumeric).then_some(parsed)
}

/// Whether the note already carries the section.
pub(crate) fn has_section(content: &str) -> bool {
    content.lines().any(|line| line.trim() == HEADING)
}

/// A note the review would read: dated for a distilled session of the class.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ReviewTarget {
    pub rel_path: String,
    pub name: String,
    pub date: String,
    /// The session document, as the digest wrote it.
    pub session_rel_path: String,
    pub transcript_rel_path: String,
    pub unit_name: String,
    /// The note already carries the section.
    pub reviewed: bool,
}

/// Every note of the class dated for a session that has a session document,
/// with whether it has been reviewed.
pub fn list_targets(conn: &Connection, class_id: i64) -> Result<Vec<ReviewTarget>> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let notes = crate::notes::list_notes(conn, class_id)?;
    if notes.is_empty() {
        return Ok(Vec::new());
    }
    let contributions = crate::lectures::list_contributions(conn, class_id)?;
    let sessions = crate::guides::guides_of_family(conn, class_id, "session")?;
    let mut out = Vec::new();
    for note in notes {
        let title = crate::notes::note_title(&note.rel_path);
        let Some(date) = note_date(title) else {
            continue;
        };
        let date = date.to_string();
        // The session of that date with a document: the first by path when
        // two lectures share a day.
        let session = contributions
            .iter()
            .filter(|c| crate::lectures::session_date(&c.rel_path) == date)
            .find_map(|c| {
                let scope = format!("{}{}", crate::db::SESSION_SCOPE_PREFIX, c.rel_path);
                sessions.iter().find(|g| g.scope == scope).map(|g| (c, g))
            });
        let Some((contribution, document)) = session else {
            continue;
        };
        let reviewed = std::fs::read_to_string(class_dir.join(&note.rel_path))
            .map(|text| has_section(&text))
            .unwrap_or(false);
        out.push(ReviewTarget {
            rel_path: note.rel_path.clone(),
            name: title.to_string(),
            date,
            session_rel_path: document.rel_path.clone(),
            transcript_rel_path: contribution.rel_path.clone(),
            unit_name: contribution.unit_name.clone(),
            reviewed,
        });
    }
    Ok(out)
}

/// Carried across the job: the note the answer is appended to.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewPayload {
    rel_path: String,
}

/// `Against the room` on a note's row (SPEC §8.6).
pub fn review_note(app: &AppHandle, class_id: i64, rel_path: &str) -> Result<i64> {
    let prompt = with_conn(app, |conn| {
        if crate::guides::has_active_job(conn, class_id, KIND, rel_path)? {
            bail!("this note is already being read against the room");
        }
        let target = list_targets(conn, class_id)?
            .into_iter()
            .find(|t| t.rel_path == rel_path)
            .with_context(|| {
                format!(
                    "{} is not dated for a distilled session — a note's title opens with the \
                     meeting's date, and the session needs its document first",
                    crate::notes::note_title(rel_path)
                )
            })?;
        if target.reviewed {
            bail!("{} already carries {HEADING}", target.name);
        }
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let note = std::fs::read_to_string(class_dir.join(&target.rel_path))
            .with_context(|| format!("reading {}", target.rel_path))?;
        // Decidable before the run is spent: the note plus the largest
        // section has to fit what a note save accepts.
        if note.len() + MAX_SECTION_CHARS * 4 > crate::notes::MAX_NOTE_BYTES {
            bail!("{} is too long for a section to be appended — a note is 1 MB at most", target.name);
        }
        let class_name: String = conn.query_row(
            "SELECT display_name FROM classes WHERE id = ?1",
            [class_id],
            |row| row.get(0),
        )?;
        // The note's own text last, so a `{placeholder}` literal inside it
        // is never rewritten by a later substitution.
        Ok(PROMPT_TEMPLATE
            .replace("{class}", &class_name)
            .replace("{date}", &target.date)
            .replace("{unit}", &target.unit_name)
            .replace("{note_path}", &target.rel_path)
            .replace("{session}", &crate::guides::md_twin(&target.session_rel_path))
            .replace("{session_html}", &target.session_rel_path)
            .replace("{transcript}", &target.transcript_rel_path)
            .replace("{heading}", HEADING)
            .replace("{note}", &crate::db::truncate(note.trim(), MAX_NOTE_CHARS)))
    })?;
    let payload = serde_json::to_string(&ReviewPayload { rel_path: rel_path.to_string() })?;
    crate::jobs::enqueue_notes_review(app, class_id, rel_path, &prompt, payload)
}

/// The job's contracted answer.
#[derive(Deserialize)]
struct ReviewAnswer {
    section: String,
}

/// The section as the note takes it: the heading, then the body.
pub(crate) fn section_text(body: &str) -> Result<String> {
    let body = body.trim();
    if body.is_empty() {
        bail!("the review answered with an empty section");
    }
    if has_section(body) {
        bail!("the review's section carries the heading itself — the app adds it");
    }
    Ok(format!("{HEADING}\n\n{}\n", crate::db::truncate(body, MAX_SECTION_CHARS)))
}

/// Whether the section may be appended to this note at finalize time: a
/// note under `Notes/`, read again under the lock, that does not carry the
/// heading — a second review of a note that gained it meanwhile is refused.
pub(crate) fn appendable(rel_path: &str, note: &str) -> Result<()> {
    if !rel_path.starts_with(&format!("{NOTES_DIR}/")) {
        bail!("{rel_path} is not a note");
    }
    if has_section(note) {
        bail!(
            "{} already carries {HEADING} — not appended twice",
            crate::notes::note_title(rel_path)
        );
    }
    Ok(())
}

/// The note with the section appended — a blank line between, and the
/// note's own text kept to the byte.
pub(crate) fn appended(note: &str, section: &str) -> String {
    let mut out = note.trim_end_matches('\n').to_string();
    out.push_str("\n\n");
    out.push_str(section);
    out
}

/// Completion (job runner, before the row leaves `running`): the answer
/// parsed, the note checked again for the heading — a second review of a
/// note that gained it meanwhile is refused — and the section appended
/// through the note write path with its audit row and its notice.
pub fn finalize_job(app: &AppHandle, class_id: i64, payload: Option<&str>, result_text: &str) -> Result<String> {
    let payload: ReviewPayload =
        serde_json::from_str(payload.context("the review job carries no payload")?)
            .context("parsing the review payload")?;
    let value = crate::jobs::parse_object(result_text)?;
    let answer: ReviewAnswer =
        serde_json::from_value(value).context("the review's answer names no section")?;
    let section = section_text(&answer.section)?;
    let (written, name) = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        let rel = payload.rel_path.as_str();
        let note = std::fs::read_to_string(class_dir.join(rel))
            .with_context(|| format!("{rel} could not be read — it may have been removed"))?;
        appendable(rel, &note)?;
        let written = crate::notes::overwrite_note(conn, class_id, rel, &appended(&note, &section), "review.write_note")?;
        Ok((written, crate::notes::note_title(rel).to_string()))
    })?;
    emit_hub_change(app, "notes");
    notify(
        app,
        format!("Added Against the room to {name}"),
        vec![written.audit_id],
        Some(class_id),
    );
    Ok(format!("Against the room added to {name}"))
}

/// The shift's list (SPEC §6): every unreviewed note dated for a distilled
/// session, across the classes, the oldest date first; one a night. A note
/// whose review once succeeded is not read again by the shift — its section
/// undone, or edited out by hand, is the owner's decision, and the row's
/// action stays for a second reading asked for.
pub(crate) fn candidates(conn: &Connection, now_secs: i64) -> Result<Vec<(i64, ReviewTarget)>> {
    let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
    let class_ids: Vec<i64> = stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for class_id in class_ids {
        for target in list_targets(conn, class_id)? {
            if target.reviewed
                || crate::shift::recently_failed(conn, KIND, class_id, &target.rel_path, now_secs)?
                || crate::shift::last_job_status(conn, KIND, class_id, &target.rel_path)?.as_deref()
                    == Some("succeeded")
            {
                continue;
            }
            out.push((class_id, target));
        }
    }
    out.sort_by(|a, b| a.1.date.cmp(&b.1.date).then(a.1.rel_path.cmp(&b.1.rel_path)));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::memory_db;
    use rusqlite::params;

    /// The note-date convention (SPEC §8.6): a title that opens with the
    /// meeting's date is that meeting's note; anything else is not.
    #[test]
    fn a_note_is_a_meetings_when_its_title_opens_with_the_date() {
        assert_eq!(note_date("2026-09-10 \u{2014} Biostatistics in class"), NaiveDate::from_ymd_opt(2026, 9, 10));
        assert_eq!(note_date("2026-09-10"), NaiveDate::from_ymd_opt(2026, 9, 10));
        assert_eq!(note_date("2026-09-10: in class"), NaiveDate::from_ymd_opt(2026, 9, 10));
        assert_eq!(note_date("Central tendency \u{2014} quick reference"), None);
        assert_eq!(note_date("Notes from 2026-09-10"), None);
        // The editor's untouched default is nothing the owner dated.
        assert_eq!(note_date("2026-09-10 \u{2014}"), None);
        assert_eq!(note_date("2026-09-10 \u{2014} "), None);
        assert!(appendable("Notes/x.md", "# note").is_ok());
        assert!(appendable("Weeks/x.md", "# note").is_err());
        assert!(appendable("Notes/x.md", "# note\n\n## Against the room\n").is_err());
        assert!(has_section("# Note\n\n## Against the room\n\nbody"));
        assert!(has_section("## Against the room  "));
        assert!(!has_section("# Note\n\n### Against the room\n"));
        assert!(!has_section("Against the room is a section"));
    }

    /// The append (SPEC §8.6): the note's own text to the byte, a blank
    /// line, the heading and the body; a section that carries the heading
    /// itself, or nothing, is refused; a reviewed note is refused a second.
    #[test]
    fn the_section_is_appended_once() {
        let section = section_text("What the note has: x.\n\nWhat it missed: y (00:45).").unwrap();
        assert!(section.starts_with("## Against the room\n\n"));
        assert!(section.ends_with("(00:45).\n"));
        assert_eq!(appended("# In class\n\n- a point\n", &section), format!("# In class\n\n- a point\n\n{section}"));
        assert!(section_text("   ").is_err());
        assert!(section_text("## Against the room\n\nbody").is_err());
        assert!(has_section(&appended("note", &section)));

        // Targets: a note dated for a distilled session with a document.
        let conn = memory_db();
        let root = std::env::temp_dir().join(format!("classhub-review-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).unwrap();
        let class_dir = root.join("Biostatistics for AI");
        std::fs::create_dir_all(class_dir.join(NOTES_DIR)).unwrap();
        std::fs::write(class_dir.join("Notes/2026-09-03 \u{2014} In class.md"), "# In class\n").unwrap();
        std::fs::write(class_dir.join("Notes/2026-09-10 \u{2014} In class.md"), "# Next week\n").unwrap();
        std::fs::write(class_dir.join("Notes/Quick reference.md"), "# Ref\n").unwrap();
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, number, source)
             VALUES (8, 3, 3, 'week', 'Week 3', 3, 'syllabus')",
            [],
        )
        .unwrap();
        let transcript = "Weeks/Week 03/2026-09-03 \u{2014} Lecture.md";
        conn.execute(
            "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
             VALUES (3, 8, ?1, 0, 1, 1, 2, '.classhub/corpus/Week 3/2026-09-03 \u{2014} Lecture.md', 'Outliers', 'high', 'applied', 1)",
            [transcript],
        )
        .unwrap();
        assert!(list_targets(&conn, 3).unwrap().is_empty(), "no session document yet");
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (3, ?1, 'Study Guides/Sessions/2026-09-03 \u{2014} Outliers.html', 1, '[]')",
            params![format!("session:{transcript}")],
        )
        .unwrap();
        let targets = list_targets(&conn, 3).unwrap();
        assert_eq!(targets.len(), 1, "{targets:?}");
        assert_eq!((targets[0].date.as_str(), targets[0].reviewed), ("2026-09-03", false));
        assert_eq!(targets[0].session_rel_path, "Study Guides/Sessions/2026-09-03 \u{2014} Outliers.html");
        assert_eq!(candidates(&conn, 1_000_000).unwrap().len(), 1);
        std::fs::write(class_dir.join("Notes/2026-09-03 \u{2014} In class.md"), appended("# In class\n", &section)).unwrap();
        assert!(list_targets(&conn, 3).unwrap()[0].reviewed);
        assert!(candidates(&conn, 1_000_000).unwrap().is_empty(), "reviewed once");
        // The section undone: the row offers the review again, the shift
        // does not spend a second run on a note whose review succeeded.
        std::fs::write(class_dir.join("Notes/2026-09-03 \u{2014} In class.md"), "# In class\n").unwrap();
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, created_at, started_at, finished_at)
             VALUES ('notes_review', 3, 'Notes/2026-09-03 \u{2014} In class.md', 'succeeded', 5, 5, 6)",
            [],
        )
        .unwrap();
        assert!(!list_targets(&conn, 3).unwrap()[0].reviewed, "the row's action is back");
        assert!(candidates(&conn, 1_000_000).unwrap().is_empty(), "the shift leaves an undone review alone");
        let _ = std::fs::remove_dir_all(&root);
    }
}
