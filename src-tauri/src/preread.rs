//! SPEC §8.6 — the pre-read: one page before a lecture whose deck or reading
//! posted early. A candidate is a week the course dates within the coming
//! days whose folder holds a file and no transcript; the document draws on
//! that folder, the previous division's note and hints, and the objectives.
//! Written from the Lectures section or by the shift, one per course a
//! night, and removed when the session document lands, since it is
//! superseded. Scoped `preread:<unit id>` in `guides`.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::AppHandle;

use crate::db::{with_conn, PREREAD_SCOPE_PREFIX, SESSIONS_DIR};
use crate::guides::{md_twin, synthesis_context, DocumentPayload};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/pre_read.md");
pub const KIND: &str = "pre_read";
/// How far ahead a week counts as coming (SPEC §8.6): its meeting within
/// this many days of today, today included.
pub(crate) const DAYS_AHEAD: i64 = 7;

pub(crate) fn preread_scope(unit_id: i64) -> String {
    format!("{PREREAD_SCOPE_PREFIX}{unit_id}")
}

/// Where a pre-read lands: `Study Guides/Sessions/<meeting date> — Before class.html`.
pub(crate) fn output_rel(meets_on: &str) -> String {
    format!("{SESSIONS_DIR}/{meets_on} \u{2014} Before class.html")
}

/// The weeks a pre-read may be written for (SPEC §8.6): those the course
/// dates from today through `DAYS_AHEAD` days on. Pure, since a wrong
/// answer is a page for last week, or none for Thursday.
pub(crate) fn coming_weeks<'a>(slots: &'a [crate::units::WeekSlot], today: NaiveDate) -> Vec<&'a crate::units::WeekSlot> {
    let last = today + chrono::Days::new(DAYS_AHEAD as u64);
    slots
        .iter()
        .filter(|slot| {
            slot.meets_on
                .as_deref()
                .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
                .is_some_and(|meets| meets >= today && meets <= last)
        })
        .collect()
}

/// One pre-read as the Lectures section lists it: a coming week whose folder
/// holds material and no transcript, or a pre-read already written.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrereadInfo {
    pub unit_id: i64,
    pub unit_name: String,
    pub week: i64,
    /// The meeting the page is for, `YYYY-MM-DD`.
    pub meets_on: String,
    pub scope: String,
    /// Files under the week folder — what the page would read.
    pub files: i64,
    /// Whether the week still qualifies: material filed, no transcript, the
    /// meeting still coming. A written pre-read is listed either way.
    pub candidate: bool,
    pub rel_path: Option<String>,
    pub generated_at: Option<i64>,
    pub stale: bool,
}

fn files_under(conn: &Connection, class_id: i64, folder: &str) -> Result<i64> {
    let prefix = format!(
        "{}/{}/%",
        crate::db::WEEKS_DIR,
        folder.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM files WHERE class_id = ?1 AND rel_path LIKE ?2 ESCAPE '\\'
           AND duplicate_of IS NULL",
        params![class_id, prefix],
        |row| row.get(0),
    )?)
}

/// The candidates and the written pre-reads of a class, by meeting date.
pub fn list(conn: &Connection, class_id: i64, today: &str) -> Result<Vec<PrereadInfo>> {
    let today = NaiveDate::parse_from_str(today, "%Y-%m-%d").context("today is not a date")?;
    let slots = crate::units::week_slots(conn, class_id)?;
    let written = crate::guides::guides_of_family(conn, class_id, "preread")?;
    let mut out: Vec<PrereadInfo> = Vec::new();
    for slot in coming_weeks(&slots, today) {
        let files = files_under(conn, class_id, &slot.folder)?;
        let transcript = !crate::lectures::contributing_paths(conn, class_id, slot.unit_id)?.is_empty();
        if files == 0 || transcript {
            continue;
        }
        let scope = preread_scope(slot.unit_id);
        let row = written.iter().find(|g| g.scope == scope);
        out.push(PrereadInfo {
            unit_id: slot.unit_id,
            unit_name: slot.unit_name.clone(),
            week: slot.week,
            meets_on: slot.meets_on.clone().unwrap_or_default(),
            scope,
            files,
            candidate: true,
            rel_path: row.map(|g| g.rel_path.clone()),
            generated_at: row.map(|g| g.generated_at),
            stale: row.is_some_and(|g| g.stale),
        });
    }
    // A pre-read written for a week that no longer qualifies — its meeting
    // passed with no transcript filed — stays readable until a session
    // document supersedes it.
    for row in &written {
        if out.iter().any(|p| p.scope == row.scope) {
            continue;
        }
        let Some(unit_id) = crate::db::preread_scope_id(&row.scope) else {
            continue;
        };
        let slot = slots.iter().find(|s| s.unit_id == unit_id);
        out.push(PrereadInfo {
            unit_id,
            unit_name: slot.map(|s| s.unit_name.clone()).unwrap_or_else(|| row.label.clone()),
            week: slot.map_or(0, |s| s.week),
            meets_on: slot.and_then(|s| s.meets_on.clone()).unwrap_or_default(),
            scope: row.scope.clone(),
            files: slot.map(|s| files_under(conn, class_id, &s.folder)).transpose()?.unwrap_or(0),
            candidate: false,
            rel_path: Some(row.rel_path.clone()),
            generated_at: Some(row.generated_at),
            stale: row.stale,
        });
    }
    out.sort_by(|a, b| a.meets_on.cmp(&b.meets_on).then(a.unit_id.cmp(&b.unit_id)));
    Ok(out)
}

/// The `{previous}` block: the division that met last before this week —
/// its notes and what was flagged — so the page can say how the week
/// follows from the last.
fn previous_block(conn: &Connection, class_id: i64, slot: &crate::units::WeekSlot) -> Result<String> {
    let slots = crate::units::week_slots(conn, class_id)?;
    let previous = slots
        .iter()
        .filter(|s| s.unit_id != slot.unit_id && s.meets_on.is_some() && s.meets_on < slot.meets_on)
        .max_by(|a, b| a.meets_on.cmp(&b.meets_on));
    let Some(previous) = previous else {
        return Ok("(none — this is the course's first dated week)".to_string());
    };
    let notes = crate::lectures::corpus_notes(conn, class_id, previous.unit_id)?;
    let hints = crate::guides::hints_for(conn, class_id, Some(&[previous.unit_id]))?;
    Ok(format!(
        "{} (met {})\n\nIts distilled notes, each with the transcript it came from:\n{}\n\nWhat the professor flagged there:\n{}",
        previous.unit_name,
        previous.meets_on.as_deref().unwrap_or("undated"),
        crate::lectures::corpus_block(&notes),
        hints
    ))
}

/// `Write the pre-read` (SPEC §8.6).
pub fn write_preread(app: &AppHandle, class_id: i64, unit_id: i64, generated_at_label: &str) -> Result<i64> {
    let scope = preread_scope(unit_id);
    let (class_dir, prompt, payload) = with_conn(app, |conn| {
        if crate::guides::has_active_job(conn, class_id, KIND, &scope)? {
            bail!("a pre-read for this week is already being written");
        }
        let slots = crate::units::week_slots(conn, class_id)?;
        let slot = slots
            .iter()
            .find(|s| s.unit_id == unit_id)
            .context("that division is no longer one of the course's weeks")?
            .clone();
        let meets_on = slot
            .meets_on
            .clone()
            .with_context(|| format!("{} is not dated, so there is no meeting to read before", slot.unit_name))?;
        if !crate::lectures::contributing_paths(conn, class_id, unit_id)?.is_empty() {
            bail!(
                "{} already has a lecture filed — its session document is the page to read",
                slot.unit_name
            );
        }
        let ctx = synthesis_context(
            conn,
            class_id,
            &scope,
            BTreeSet::new(),
            false,
            &format!("nothing is filed under {}'s folder yet", slot.unit_name),
        )?;
        let output = output_rel(&meets_on);
        let output_md = md_twin(&output);
        let prompt = PROMPT_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{unit}", &slot.unit_name)
            .replace("{meets_on}", &meets_on)
            .replace("{files}", &ctx.files_block)
            .replace("{previous}", &previous_block(conn, class_id, &slot)?)
            .replace(
                "{objectives}",
                &crate::guides::objectives_block(&crate::guides::unit_objectives(conn, unit_id)?),
            )
            .replace("{output}", &output)
            .replace("{output_md}", &output_md)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&DocumentPayload {
            scope: scope.clone(),
            rel_path: output,
            md_rel_path: Some(output_md),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        Ok((ctx.class_dir, prompt, payload))
    })?;
    fs::create_dir_all(class_dir.join(SESSIONS_DIR)).with_context(|| format!("creating {SESSIONS_DIR}"))?;
    crate::jobs::enqueue_document(app, KIND, class_id, &scope, &prompt, payload)
}

/// The shift's list (SPEC §6): per class, the first coming week that
/// qualifies and has no fresh pre-read — one per course a night.
pub(crate) fn candidates(conn: &Connection, today: NaiveDate, now_secs: i64) -> Result<Vec<(i64, i64, String)>> {
    let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
    let class_ids: Vec<i64> = stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for class_id in class_ids {
        let first = list(conn, class_id, &today.to_string())?
            .into_iter()
            .filter(|p| p.candidate && (p.rel_path.is_none() || p.stale))
            .find(|p| !crate::shift::recently_failed(conn, KIND, class_id, &p.scope, now_secs).unwrap_or(true));
        if let Some(p) = first {
            out.push((class_id, p.unit_id, p.unit_name));
        }
    }
    Ok(out)
}

/// The session document landed for a lecture of this division: its pre-read
/// is superseded (SPEC §8.6). The row goes inside the caller's transaction;
/// the files it names are the caller's to remove after the commit.
pub(crate) fn supersede(conn: &Connection, class_id: i64, unit_id: i64) -> Result<Vec<String>> {
    crate::guides::forget_document(conn, class_id, &preread_scope(unit_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::memory_db;

    fn slot(unit_id: i64, week: i64, meets_on: Option<&str>) -> crate::units::WeekSlot {
        crate::units::WeekSlot {
            week,
            folder: format!("Week {week:02}"),
            unit_id,
            unit_name: format!("Week {week}"),
            unit_kind: "week".into(),
            meets_on: meets_on.map(Into::into),
        }
    }

    /// The candidate rule (SPEC §8.6): a coming week — today through the week
    /// ahead — whose folder holds a file and no transcript; a past week, an
    /// undated one, an empty folder and a filed lecture each rule it out.
    #[test]
    fn a_pre_read_is_for_a_coming_week_with_material_and_no_transcript() {
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        let slots = vec![
            slot(1, 1, Some("2026-09-01")),
            slot(2, 2, Some("2026-09-08")),
            slot(3, 3, Some("2026-09-10")),
            slot(4, 4, Some("2026-09-15")),
            slot(5, 5, Some("2026-09-16")),
            slot(6, 6, None),
        ];
        let coming: Vec<i64> = coming_weeks(&slots, today).iter().map(|s| s.week).collect();
        assert_eq!(coming, vec![2, 3, 4], "today through seven days on; an undated week never");

        let conn = memory_db();
        crate::db::set_setting(&conn, "aibhs_root", "/nonexistent/classhub-preread").unwrap();
        // Biostatistics (class 3): Week 3 met with a transcript, Week 4 holds a reading.
        for (number, starts) in [(3, "2026-09-03"), (4, "2026-09-10"), (5, "2026-09-17"), (6, "2026-09-24")] {
            conn.execute(
                "INSERT INTO units (id, class_id, ordinal, kind, name, number, starts_on, source)
                 VALUES (?1, 3, ?1, 'week', ?2, ?1, ?3, 'syllabus')",
                params![number, format!("Week {number} \u{2014} Topic"), starts],
            )
            .unwrap();
        }
        for rel in [
            "Weeks/Week 03 \u{2014} Topic/2026-09-03 \u{2014} Lecture.md",
            "Weeks/Week 03 \u{2014} Topic/deck.pdf",
            "Weeks/Week 04 \u{2014} Topic/reading.pdf",
            "Weeks/Week 06 \u{2014} Topic/reading.pdf",
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (3, ?1, ?1, 1, 1, 'pdf')",
                [rel],
            )
            .unwrap();
        }
        conn.execute(
            "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
             VALUES (3, 3, 'Weeks/Week 03 \u{2014} Topic/2026-09-03 \u{2014} Lecture.md', 0, 1, 1, 2,
                     '.classhub/corpus/Week 3 \u{2014} Topic/2026-09-03 \u{2014} Lecture.md', '', 'high', 'applied', 1)",
            [],
        )
        .unwrap();
        let listed = list(&conn, 3, "2026-09-08").unwrap();
        assert_eq!(listed.iter().map(|p| (p.unit_id, p.candidate, p.files)).collect::<Vec<_>>(), vec![(4, true, 1)],
            "Week 3 has its transcript, Week 5 nothing filed, Week 6 is past the week ahead");
        assert_eq!(listed[0].meets_on, "2026-09-10");
        assert_eq!(output_rel("2026-09-10"), "Study Guides/Sessions/2026-09-10 \u{2014} Before class.html");

        // The shift takes the same candidate, once, and a fresh pre-read clears it.
        assert_eq!(candidates(&conn, NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(), 1_000_000).unwrap().len(), 1);
        crate::guides::upsert_guide(
            &conn, 3, &preread_scope(4), &output_rel("2026-09-10"),
            r#"[{"relPath":"Weeks/Week 04 — Topic/reading.pdf","sha256":"Weeks/Week 04 — Topic/reading.pdf"}]"#,
        ).unwrap();
        assert!(candidates(&conn, NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(), 1_000_000).unwrap().is_empty());
        let listed = list(&conn, 3, "2026-09-08").unwrap();
        assert_eq!((listed[0].rel_path.is_some(), listed[0].stale), (true, false));
        // The week passes with no transcript: still listed, no longer a candidate.
        let later = list(&conn, 3, "2026-09-20").unwrap();
        assert_eq!(later.iter().map(|p| (p.unit_id, p.candidate)).collect::<Vec<_>>(), vec![(4, false), (6, true)], "Week 6 is now within the week");
        // The session document lands: the pre-read is forgotten with its files named.
        let files = supersede(&conn, 3, 4).unwrap();
        assert_eq!(files, vec![
            "Study Guides/Sessions/2026-09-10 \u{2014} Before class.md".to_string(),
            "Study Guides/Sessions/2026-09-10 \u{2014} Before class.html".to_string(),
        ]);
        assert_eq!(list(&conn, 3, "2026-09-20").unwrap().iter().map(|p| p.unit_id).collect::<Vec<_>>(), vec![6]);
        assert!(supersede(&conn, 3, 4).unwrap().is_empty(), "nothing twice");
    }
}
