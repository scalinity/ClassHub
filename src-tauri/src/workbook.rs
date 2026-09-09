//! SPEC §8.6 — the project workbook, one per class: the project's items in
//! order with their states, the rubric as the guidelines state it, what the
//! next item needs and what the owner's own drafts under `Project/` already
//! have toward it. Written from the workspace's `Project` section, or by the
//! shift when an item is due within the week and the workbook is stale or
//! absent. Scoped `project` in `guides`. Beside it the presentation kit: a
//! manual run from a paper's row, scoped `kit:<pdf rel path>`.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use tauri::AppHandle;

use crate::db::{setting, with_conn, KIT_SCOPE_PREFIX, PRESENTATIONS_DIR, PROJECT_DIR, PROJECT_OUTPUT, PROJECT_SCOPE};
use crate::extract::ManifestEntry;
use crate::guides::{md_twin, synthesis_context, DocumentPayload};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/project_workbook.md");
const KIT_TEMPLATE: &str = include_str!("../prompts/presentation_kit.md");
pub const KIND: &str = "project_workbook";
pub const KIT_KIND: &str = "presentation_kit";
/// How far ahead the shift refreshes a workbook (SPEC §6).
pub(crate) const DAYS_AHEAD: i64 = 7;
/// The setting a class's `Project material` pick lives under: `project_material.<class id>`.
const MATERIAL_SETTING: &str = "project_material";
/// A description or a notice body in the prompt.
const MAX_TEXT_CHARS: usize = 4000;

/// Whether a deadline is one of the project's items (SPEC §8.6): a `project`
/// row, or an assignment whose title speaks the project's vocabulary with
/// no homework word in it — the syllabus scan typed Design Studio's `Draft:
/// Introduction` an assignment, and it is the project's. Pure, since a wrong
/// reading is a section that hides, or a homework listed as a milestone.
pub(crate) fn is_project_item(kind: &str, title: &str) -> bool {
    if kind == "project" {
        return true;
    }
    if kind != "assignment" {
        return false;
    }
    let key = crate::deadlines::assignment_key(title);
    let words: Vec<&str> = key.split(' ').collect();
    const PROJECT_WORDS: &[&str] = &[
        "project", "draft", "proposal", "milestone", "demo", "prototype", "presentation",
        "presentations", "sketch", "teaming", "scaling", "capstone", "poster",
    ];
    const HOMEWORK_WORDS: &[&str] = &["homework", "quiz", "exam", "test", "coding", "survey"];
    !words.iter().any(|w| HOMEWORK_WORDS.contains(w)) && words.iter().any(|w| PROJECT_WORDS.contains(w))
}

/// Whether a file's name reads as the project's guidelines (SPEC §8.6).
pub(crate) fn is_guideline_file(rel_path: &str) -> bool {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path).to_lowercase();
    ["project", "guideline", "rubric", "proposal"].iter().any(|word| name.contains(word))
}

/// One project item as the section and the prompt read it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectItem {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub due_at: String,
    pub status: String,
    pub notes: Option<String>,
    pub description: Option<String>,
    pub from_canvas: bool,
}

/// The class's project items in due order, open and done.
pub fn project_items(conn: &Connection, class_id: i64) -> Result<Vec<ProjectItem>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, title, kind, due_at, status, notes, description, canvas_assignment_id IS NOT NULL
         FROM deadlines WHERE class_id = ?1 ORDER BY {}, id",
        crate::deadlines::DUE_INSTANT_SQL
    ))?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(ProjectItem {
                id: row.get(0)?,
                title: row.get(1)?,
                kind: row.get(2)?,
                due_at: row.get(3)?,
                status: row.get(4)?,
                notes: row.get(5)?,
                description: row.get(6)?,
                from_canvas: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows.into_iter().filter(|item| is_project_item(&item.kind, &item.title)).collect())
}

/// The file the owner pointed the `Project material` picker at, if any.
pub fn material(conn: &Connection, class_id: i64) -> Option<String> {
    setting(conn, &format!("{MATERIAL_SETTING}.{class_id}"))
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
}

/// Points the picker at a class file, or clears it; audited like every
/// setting write.
pub fn set_material(app: &AppHandle, class_id: i64, rel_path: Option<&str>) -> Result<()> {
    let key = format!("{MATERIAL_SETTING}.{class_id}");
    match rel_path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(rel) => {
            let indexed: bool = with_conn(app, |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM files WHERE class_id = ?1 AND rel_path = ?2)",
                    params![class_id, rel],
                    |row| row.get(0),
                )?)
            })?;
            if !indexed {
                bail!("{rel} is not an indexed file of this class — rescan, or pick another");
            }
            crate::settings::set_audited(app, &key, rel)?;
        }
        None => crate::settings::clear_audited(app, &key)?,
    }
    crate::db::emit_hub_change(app, "project");
    Ok(())
}

/// The workbook's manifest (SPEC §8.6): every guideline file by name, the
/// picked file, and everything under `Project/` — what `current_manifest`
/// answers for `project`.
pub fn project_manifest(conn: &Connection, class_id: i64) -> Result<Vec<ManifestEntry>> {
    let picked = material(conn, class_id);
    let mut stmt = conn.prepare(
        "SELECT rel_path, sha256 FROM files
         WHERE class_id = ?1 AND duplicate_of IS NULL ORDER BY rel_path",
    )?;
    let entries = stmt
        .query_map([class_id], |row| {
            Ok(ManifestEntry { rel_path: row.get(0)?, sha256: row.get(1)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|entry| {
            is_guideline_file(&entry.rel_path)
                || entry.rel_path.starts_with(&format!("{PROJECT_DIR}/"))
                || picked.as_deref() == Some(entry.rel_path.as_str())
        })
        .collect();
    Ok(entries)
}

/// What the `Project` section shows (SPEC §12): the items, the next open
/// one, the pick and the project's weight; `None` for a class with no item.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStatus {
    pub items: Vec<ProjectItem>,
    pub next: Option<ProjectItem>,
    pub material: Option<String>,
    /// The project category's weight, summed over the categories named for it.
    pub weight: f64,
}

pub fn status(conn: &Connection, class_id: i64, today: &str) -> Result<Option<ProjectStatus>> {
    let items = project_items(conn, class_id)?;
    if items.is_empty() {
        return Ok(None);
    }
    let next = items
        .iter()
        .find(|item| item.status == "open" && item.due_at.get(..10).unwrap_or("") >= today)
        .cloned();
    let mut stmt = conn.prepare("SELECT name, weight FROM grade_categories WHERE class_id = ?1")?;
    let weight: f64 = stmt
        .query_map([class_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(name, _)| name.to_lowercase().contains("project"))
        .map(|(_, w)| w)
        .sum();
    Ok(Some(ProjectStatus { items, next, material: material(conn, class_id), weight }))
}

/// The `{items}` block: every project item in order with its state.
fn items_block(items: &[ProjectItem]) -> String {
    items
        .iter()
        .map(|item| {
            let mut line = format!(
                "- {} · {} ({}, {}{})",
                item.due_at,
                item.title,
                item.kind,
                item.status,
                if item.from_canvas { ", on Canvas" } else { "" }
            );
            if let Some(notes) = item.notes.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
                line.push_str(&format!("\n  notes: {notes}"));
            }
            if let Some(text) = item.description.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                line.push_str(&format!("\n  description: {}", crate::db::truncate(text, MAX_TEXT_CHARS)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The `{notices}` block: the class's announcements that mention the project.
fn notices_block(conn: &Connection, class_id: i64) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT posted_at, title, body FROM announcements
         WHERE class_id = ?1 AND (lower(title) LIKE '%project%' OR lower(body) LIKE '%project%')
         ORDER BY posted_at",
    )?;
    let lines = stmt
        .query_map([class_id], |row| {
            let posted: String = row.get(0)?;
            let title: String = row.get(1)?;
            let body: String = row.get(2)?;
            Ok(format!(
                "--- {} · \"{}\"\n{}",
                posted.get(..10).unwrap_or(&posted),
                title.split_whitespace().collect::<Vec<_>>().join(" "),
                crate::db::truncate(body.trim(), MAX_TEXT_CHARS)
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if lines.is_empty() {
        "(none — no announcement of this class mentions the project)".to_string()
    } else {
        lines.join("\n\n")
    })
}

/// The `{actions}` block: what was assigned in the room about the project.
fn actions_block(conn: &Connection, class_id: i64) -> Result<String> {
    let hints = crate::lectures::list_hints(conn, class_id)?;
    let lines: Vec<String> = hints
        .iter()
        .filter(|h| h.kind == "action" && h.text.to_lowercase().contains("project"))
        .map(|h| {
            format!(
                "- {}{} — {} (transcript: {})",
                h.date,
                h.anchor.as_deref().map_or(String::new(), |a| format!(" · {a}")),
                h.text,
                h.rel_path
            )
        })
        .collect();
    Ok(if lines.is_empty() {
        "(none — no distilled session flagged an action about the project)".to_string()
    } else {
        lines.join("\n")
    })
}

/// `Write the workbook` (SPEC §8.6).
pub fn write_workbook(app: &AppHandle, class_id: i64, generated_at_label: &str) -> Result<i64> {
    let (class_dir, prompt, payload) = with_conn(app, |conn| {
        if crate::guides::has_active_job(conn, class_id, KIND, PROJECT_SCOPE)? {
            bail!("the workbook is already being written");
        }
        let items = project_items(conn, class_id)?;
        if items.is_empty() {
            bail!("this class has no project item on its list — add the project's deadlines first");
        }
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let next = items
            .iter()
            .find(|item| item.status == "open" && item.due_at.get(..10).unwrap_or("") >= today.as_str());
        // The workbook builds from the guidelines and the drafts; with neither
        // on disk it still has the items, the notices and the room.
        let ctx = match synthesis_context(conn, class_id, PROJECT_SCOPE, BTreeSet::new(), false, "") {
            Ok(ctx) => ctx,
            Err(_) => {
                let (class_name, color): (String, String) = conn.query_row(
                    "SELECT display_name, color FROM classes WHERE id = ?1",
                    [class_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let (accent_light, accent_dark) = crate::guides::accent_values(&color);
                crate::guides::SynthesisContext {
                    class_name,
                    accent_light,
                    accent_dark,
                    manifest: Vec::new(),
                    manifest_block: "(no guideline file or draft is indexed yet)".into(),
                    files_block: "(none — no file named for the project, no guidelines and no \
                                  draft under Project/ is indexed; work from the items, the \
                                  notices and the room, and say what is missing)"
                        .into(),
                    changes_block: String::new(),
                    class_dir: crate::scanner::class_dir(conn, class_id)?,
                }
            }
        };
        let next_block = match next {
            Some(item) => format!(
                "{} · {} ({})\n{}",
                item.due_at,
                item.title,
                item.kind,
                item.description
                    .as_deref()
                    .or(item.notes.as_deref())
                    .map(|t| crate::db::truncate(t.trim(), MAX_TEXT_CHARS))
                    .unwrap_or_else(|| "(no description on record)".into())
            ),
            None => "(none open — every item on the list is done or past)".to_string(),
        };
        let prompt = PROMPT_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{today}", &today)
            .replace("{items}", &items_block(&items))
            .replace("{next}", &next_block)
            .replace("{files}", &ctx.files_block)
            .replace("{notices}", &notices_block(conn, class_id)?)
            .replace("{actions}", &actions_block(conn, class_id)?)
            .replace("{weights}", &crate::guides::assessment_block(conn, class_id, &today)?)
            .replace("{output}", PROJECT_OUTPUT)
            .replace("{output_md}", &md_twin(PROJECT_OUTPUT))
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&DocumentPayload {
            scope: PROJECT_SCOPE.to_string(),
            rel_path: PROJECT_OUTPUT.to_string(),
            md_rel_path: Some(md_twin(PROJECT_OUTPUT)),
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        Ok((ctx.class_dir, prompt, payload))
    })?;
    fs::create_dir_all(class_dir.join(crate::db::WORKBOOK_DIR))
        .with_context(|| format!("creating {}", crate::db::WORKBOOK_DIR))?;
    crate::jobs::enqueue_document(app, KIND, class_id, PROJECT_SCOPE, &prompt, payload)
}

/// The classes whose workbook the shift would refresh (SPEC §6): an open
/// project item due within `DAYS_AHEAD`, and a workbook absent or stale.
pub(crate) fn candidates(conn: &Connection, today: NaiveDate, now_secs: i64) -> Result<Vec<(i64, String)>> {
    let last = (today + chrono::Days::new(DAYS_AHEAD as u64)).to_string();
    let today = today.to_string();
    let mut stmt = conn.prepare("SELECT id, display_name FROM classes ORDER BY id")?;
    let classes = stmt
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (class_id, name) in classes {
        let due_soon = project_items(conn, class_id)?.into_iter().any(|item| {
            let day = item.due_at.get(..10).unwrap_or("");
            item.status == "open" && day >= today.as_str() && day <= last.as_str()
        });
        if !due_soon || crate::shift::recently_failed(conn, KIND, class_id, PROJECT_SCOPE, now_secs)? {
            continue;
        }
        let fresh = crate::guides::guides_of_family(conn, class_id, "project")?
            .iter()
            .any(|g| g.scope == PROJECT_SCOPE && !g.stale);
        if !fresh {
            out.push((class_id, name));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// The presentation kit (SPEC §8.6): manual only, from a paper's row

pub(crate) fn kit_scope(rel_path: &str) -> String {
    format!("{KIT_SCOPE_PREFIX}{rel_path}")
}

/// Where a kit lands: `Study Guides/Presentations/<paper>.html`.
pub(crate) fn kit_output_rel(rel_path: &str) -> String {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let stem = name.strip_suffix(".pdf").or_else(|| name.strip_suffix(".PDF")).unwrap_or(name);
    format!("{PRESENTATIONS_DIR}/{}.html", crate::units::folder_segment(stem))
}

/// `Presentation kit` on a PDF's row (SPEC §8.6).
pub fn write_kit(app: &AppHandle, class_id: i64, rel_path: &str, generated_at_label: &str) -> Result<i64> {
    let scope = kit_scope(rel_path);
    let (class_dir, prompt, payload, output) = with_conn(app, |conn| {
        let kind: Option<String> = conn
            .query_row(
                "SELECT kind FROM files WHERE class_id = ?1 AND rel_path = ?2",
                params![class_id, rel_path],
                |row| row.get(0),
            )
            .optional()?;
        match kind.as_deref() {
            Some("pdf") => {}
            Some(other) => bail!("a presentation kit reads a paper as a PDF; {rel_path} is a {other}"),
            None => bail!("{rel_path} is not an indexed file of this class — rescan first"),
        }
        if crate::guides::has_active_job(conn, class_id, KIT_KIND, &scope)? {
            bail!("a kit for this paper is already being written");
        }
        let ctx = synthesis_context(
            conn,
            class_id,
            &scope,
            BTreeSet::new(),
            false,
            &format!("{rel_path} is not indexed — rescan the class first"),
        )?;
        let output = kit_output_rel(rel_path);
        let prompt = KIT_TEMPLATE
            .replace("{class}", &ctx.class_name)
            .replace("{paper}", rel_path)
            .replace("{files}", &ctx.files_block)
            .replace("{output}", &output)
            .replace("{accent_light}", ctx.accent_light)
            .replace("{accent_dark}", ctx.accent_dark)
            .replace("{generated_at}", generated_at_label)
            .replace("{manifest}", &ctx.manifest_block);
        let payload = serde_json::to_string(&DocumentPayload {
            scope: scope.clone(),
            rel_path: output.clone(),
            md_rel_path: None,
            source_manifest: serde_json::to_string(&ctx.manifest)?,
        })?;
        Ok((ctx.class_dir, prompt, payload, output))
    })?;
    fs::create_dir_all(class_dir.join(PRESENTATIONS_DIR))
        .with_context(|| format!("creating {PRESENTATIONS_DIR}"))?;
    let _ = output;
    crate::jobs::enqueue_document(app, KIT_KIND, class_id, &scope, &prompt, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::memory_db;

    /// A project item (SPEC §8.6) is a `project` row, or an assignment whose
    /// title speaks the project's vocabulary with no homework word in it.
    #[test]
    fn a_project_item_is_read_off_its_kind_or_its_title() {
        assert!(is_project_item("project", "Final Capstone Project"));
        assert!(is_project_item("assignment", "Problem Statement and AI Sketch"));
        assert!(is_project_item("assignment", "Draft: Introduction"));
        assert!(is_project_item("assignment", "AI Teaming Log"));
        assert!(is_project_item("assignment", "Scaling Plan & Cost Estimates"));
        assert!(!is_project_item("assignment", "Homework 2"));
        assert!(!is_project_item("assignment", "Homework 4: draft analysis"));
        assert!(!is_project_item("assignment", "Introduction to Python and Version Control"));
        assert!(!is_project_item("assignment", "Live coding session 09/01"));
        assert!(!is_project_item("quiz", "Project quiz"));
        assert!(is_guideline_file("AI Design Project/AI Design Project Guidelines.pdf"));
        assert!(is_guideline_file("Syllabus/rubric_v2.pdf"));
        assert!(!is_guideline_file("Weeks/Week 03/deck.pdf"));
        assert_eq!(kit_output_rel("Reading Material/Week 3 2022 Martijn.pdf"), "Study Guides/Presentations/Week 3 2022 Martijn.html");
    }

    /// The manifest and the status: the guideline files, the drafts under
    /// `Project/` and the pick are the sources; the next item is the first
    /// open one from today; a class with no item has no status.
    #[test]
    fn the_workbook_reads_the_guidelines_the_drafts_and_the_items() {
        let conn = memory_db();
        crate::db::set_setting(&conn, "aibhs_root", "/nonexistent/classhub-workbook").unwrap();
        for rel in [
            "AI Design Project/AI Design Project Guidelines.pdf",
            "Project/draft_v1.docx",
            "Weeks/Week 01/deck.pdf",
            "Syllabus/CAI5724.pdf",
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (2, ?1, ?1, 1, 1, 'pdf')",
                [rel],
            )
            .unwrap();
        }
        let listed: Vec<String> = project_manifest(&conn, 2).unwrap().into_iter().map(|e| e.rel_path).collect();
        assert_eq!(listed, vec!["AI Design Project/AI Design Project Guidelines.pdf", "Project/draft_v1.docx"]);
        crate::db::set_setting(&conn, "project_material.2", "Syllabus/CAI5724.pdf").unwrap();
        assert_eq!(project_manifest(&conn, 2).unwrap().len(), 3);

        assert!(status(&conn, 2, "2026-09-08").unwrap().is_none());
        for (title, kind, due, status_) in [
            ("Introduction to Python and Version Control", "assignment", "2026-09-02", "done"),
            ("Problem Statement and AI Sketch", "assignment", "2026-09-09T23:59", "open"),
            ("AI Solution Architecture", "project", "2026-09-16", "open"),
        ] {
            conn.execute(
                "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
                 VALUES (2, ?1, ?2, ?3, ?4, 'syllabus')",
                params![title, kind, due, status_],
            )
            .unwrap();
        }
        conn.execute("INSERT INTO grade_categories (class_id, name, weight) VALUES (2, 'AI Design Project', 60)", []).unwrap();
        let status = status(&conn, 2, "2026-09-08").unwrap().expect("a project class");
        assert_eq!(status.items.len(), 2, "the Python intro is not the project's");
        assert_eq!(status.next.as_ref().map(|i| i.title.as_str()), Some("Problem Statement and AI Sketch"));
        assert_eq!(status.weight, 60.0);
        assert_eq!(status.material.as_deref(), Some("Syllabus/CAI5724.pdf"));

        // The shift's list: an item due within the week and no workbook.
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).unwrap();
        assert_eq!(candidates(&conn, today, 1_000_000).unwrap(), vec![(2, "AI in Health Design Studio I".to_string())]);
        crate::guides::upsert_guide(&conn, 2, PROJECT_SCOPE, PROJECT_OUTPUT, &serde_json::to_string(&project_manifest(&conn, 2).unwrap()).unwrap()).unwrap();
        assert!(candidates(&conn, today, 1_000_000).unwrap().is_empty(), "a fresh workbook");
        conn.execute("UPDATE files SET sha256 = 'changed' WHERE rel_path = 'Project/draft_v1.docx'", []).unwrap();
        assert_eq!(candidates(&conn, today, 1_000_000).unwrap().len(), 1, "a draft changed");
    }
}
