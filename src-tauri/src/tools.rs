//! SPEC §9 — the chat agent's tools: four read tools (`get_overview`,
//! `list_material`, `search_material`, `read_material`) and, since M8, the
//! write tools (deadlines, grades, notes, synthesis triggers, practice exams,
//! file-move proposals).
//!
//! Retrieval is agentic, not vector-based (SPEC §1: no embeddings API): the
//! agent greps the pre-extracted markdown under `.classhub/extracts/` plus
//! notes and generated guides, then reads bounded windows of what it found.
//! Every path crossing the tool boundary is relative to the AIBHS root and
//! starts with a class folder name, so a path the agent reads back is a path it
//! can hand straight to `read_material` — and one the sidebar can open.
//!
//! Write policy: every write is immediate and its result text states exactly
//! what changed. Destructive writes (note overwrite, deadline delete) park the
//! prior state in `audit_log` first, so nothing a chat turn does is
//! unrecoverable; job triggers are recorded by the jobs table itself, and file
//! moves are only ever proposals in `move_proposals` — nothing moves without
//! approval. After a successful write the backend emits `hub-changed`, which
//! the frontend turns into query invalidation, so the UI reflects the change
//! without a manual refresh.
//!
//! Tools execute WITHOUT holding the DB lock across the call: write tools
//! that enqueue jobs re-enter the connection through the job runner, and a
//! held guard there would deadlock. Each tool takes the lock for exactly the
//! window it needs.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::db::{
    CORPUS_DIR, EXTRACTS_DIR, GUIDES_DIR, NOTES_DIR, audit, emit_hub_change, now, truncate,
    with_conn,
};
use crate::deadlines::{valid_due_at, DEADLINE_KINDS, MAX_NOTES_CHARS, MAX_TITLE_CHARS};
use crate::grades::{grades_line, trim_num, weighted_grade, weights_line};

/// Per-turn context for the write tools: today as display text (job prompt
/// stamps) and as YYYY-MM-DD (practice file names). Both are formatted
/// client-side — std Rust cannot format a local date.
pub struct ToolCtx<'a> {
    pub today: &'a str,
    pub today_iso: &'a str,
}


/// `read_material` reads text only; binaries are read through their extract.
const READABLE_EXTS: &[&str] = &[
    "md", "txt", "r", "rmd", "html", "htm", "csv", "tsv", "json", "tex", "bib",
    "yml", "yaml", "sql", "py", "js", "ts", "css", "log", "srt", "vtt",
];

const DEFAULT_READ_LINES: usize = 200;
const MAX_READ_LINES: usize = 400;
const MAX_LIST_LINES: usize = 400;
const MAX_READ_BYTES: usize = 8 * 1024 * 1024;
const MAX_LINE_CHARS: usize = 600;
const MAX_SEARCH_LINES: usize = 80;
const MAX_MATCH_CHARS: usize = 300;

/// One tool call's result: what the model receives, plus the one-line label the
/// sidebar shows on its chip.
pub struct Outcome {
    pub text: String,
    pub summary: String,
    pub is_error: bool,
}

impl Outcome {
    /// Every tool's first output line is written to work as its own summary, so
    /// the chip label is derived rather than duplicated — and a chip rebuilt
    /// from persisted history reads exactly like the live one did.
    fn ok(text: impl Into<String>) -> Self {
        let text = text.into();
        let summary = truncate(text.lines().next().unwrap_or_default().trim(), 140);
        Self {
            text,
            summary,
            is_error: false,
        }
    }

    fn err(message: String) -> Self {
        Self {
            summary: truncate(&message, 140),
            text: message,
            is_error: true,
        }
    }
}

/// Tools that change something. The sidebar marks their chips differently, and
/// deriving that here rather than from a hand-kept copy in the frontend means a
/// new write tool cannot quietly render as a read.
pub fn is_write(name: &str) -> bool {
    matches!(
        name,
        "upsert_deadline"
            | "complete_deadline"
            | "delete_deadline"
            | "upsert_grade_category"
            | "add_grade_item"
            | "write_note"
            | "trigger_synthesis"
            | "generate_practice"
            | "propose_file_moves"
    )
}

/// Tool schemas sent with every request (SPEC §9: four read tools, nine write
/// tools). Thirteen schemas ride every round of the loop — roughly two
/// thousand tokens, a fine price for the model always seeing its full reach.
pub fn definitions() -> Value {
    json!([
        {
            "name": "get_overview",
            "description": "Snapshot of the hub: every class, when it meets, what material is indexed and extracted per module, which study guides exist and whether they are stale, and open deadlines. Use it for questions about the schedule, deadlines, what exists, or what has been synthesized — not to find content inside material.",
            "input_schema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "list_material",
            "description": "List one class's indexed files (optionally limited to a subpath), plus its study guides and notes. Returned paths are relative to the AIBHS root and start with the class folder name — hand them straight to read_material.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": {
                        "type": "string",
                        "description": "Class name as get_overview reports it, e.g. 'Biostatistics for AI'."
                    },
                    "subpath": {
                        "type": "string",
                        "description": "Optional path inside the class folder to narrow the listing, e.g. 'Module 1/Slides'."
                    }
                },
                "required": ["class"],
                "additionalProperties": false
            }
        },
        {
            "name": "search_material",
            "description": "Grep every extract, distilled lecture note, note and study guide, returning matching lines with file paths and line numbers. This is the primary way to find content: search before reading. The query is a regular expression, case-insensitive unless it contains an uppercase letter. Prefer short distinctive phrases; if a query comes back thin, try a synonym or a narrower term.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Regular expression, e.g. 'central tendency' or 'mean|median|mode'."
                    },
                    "class": {
                        "type": "string",
                        "description": "Optional class name to search only that class."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_material",
            "description": "Read a bounded window of a text file by its AIBHS-relative path: an extract, note, study guide, or text source such as .R, .Rmd or .md. Binary sources (.pptx, .pdf) cannot be read — read their extract at '<class folder>/.classhub/extracts/<source path>.md' instead. Returns numbered lines; page through long files with offset.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path relative to the AIBHS root, starting with the class folder name."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "First line to read, 1-based. Defaults to 1."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "How many lines to read. Defaults to 200, maximum 400."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }
        },
        {
            "name": "upsert_deadline",
            "description": "Record a deadline, or amend one by passing its id (get_overview lists each open deadline's [#id]). Takes effect immediately and shows up in the app.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name, e.g. 'Biostatistics for AI'." },
                    "title": { "type": "string", "description": "What is due, e.g. 'Problem Set 2'." },
                    "kind": { "type": "string", "enum": ["assignment", "exam", "quiz", "project", "other"], "description": "Defaults to 'other'." },
                    "due_at": { "type": "string", "description": "ISO date: YYYY-MM-DD, or YYYY-MM-DDTHH:MM when the time matters." },
                    "notes": { "type": "string", "description": "Optional detail worth keeping with the deadline." },
                    "id": { "type": "integer", "description": "Pass an existing deadline's id to amend it; omit to create. When amending, only the fields you pass change." }
                },
                "required": ["class", "title", "due_at"],
                "additionalProperties": false
            }
        },
        {
            "name": "complete_deadline",
            "description": "Mark a deadline done by id (get_overview lists ids).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The deadline's id." }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "delete_deadline",
            "description": "Delete a deadline by id. The deleted row is kept in the audit log, but prefer complete_deadline for anything that was real — delete is for mistakes and duplicates.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The deadline's id." }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "upsert_grade_category",
            "description": "Create or reweight a grade category (e.g. Homework at 30%). Weights are percentages that should sum to 100 across the class; the result reports the current sum.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "name": { "type": "string", "description": "Category name, e.g. 'Homework'. Matching an existing name (case-insensitive) updates it." },
                    "weight": { "type": "number", "description": "Percentage weight, 0–100." }
                },
                "required": ["class", "name", "weight"],
                "additionalProperties": false
            }
        },
        {
            "name": "add_grade_item",
            "description": "Record a graded item (score out of max) in an existing grade category. The result includes the recomputed current weighted grade.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "category": { "type": "string", "description": "An existing category's name — create it with upsert_grade_category first if needed." },
                    "name": { "type": "string", "description": "The item, e.g. 'Quiz 1'." },
                    "score": { "type": "number", "description": "Points earned." },
                    "max_score": { "type": "number", "description": "Points possible." },
                    "graded_at": { "type": "string", "description": "Optional ISO date the grade was received." }
                },
                "required": ["class", "category", "name", "score", "max_score"],
                "additionalProperties": false
            }
        },
        {
            "name": "write_note",
            "description": "Write a markdown note into the class's Notes folder as '<title>.md'. A title matching an existing note overwrites it — read the existing note first when amending (the replaced version is kept in the audit log). Cite the written path in your answer so it can be opened.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "title": { "type": "string", "description": "Note title; becomes the file name." },
                    "content_md": { "type": "string", "description": "The full markdown content of the note." }
                },
                "required": ["class", "title", "content_md"],
                "additionalProperties": false
            }
        },
        {
            "name": "trigger_synthesis",
            "description": "Queue a study-guide synthesis job — one module's guide, or 'master' for the semester master. It appears in the Job Center immediately and runs on the Claude subscription: long (10–30+ minutes) and token-heavy, so trigger only on a clear request, one job per ask, and never re-trigger a scope that is already queued or running. The master runs exclusively after the queue drains. Report the job as queued, never as done.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "scope": { "type": "string", "description": "A module folder name as get_overview lists it (e.g. 'Module 1'), or 'master' for the semester master." }
                },
                "required": ["class", "scope"],
                "additionalProperties": false
            }
        },
        {
            "name": "generate_practice",
            "description": "Queue a practice-exam job for a module or the whole semester, optionally focused on given topics. Same rules as trigger_synthesis: subscription job, visible in the Job Center, report it as queued. The exam lands in Study Guides/Practice/ and the workspace's practice list when it succeeds.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "scope": { "type": "string", "description": "A module folder name, or 'master' for semester-wide." },
                    "focus": { "type": "string", "description": "Optional topics to emphasize, e.g. 'hypothesis testing and p-values'." }
                },
                "required": ["class", "scope"],
                "additionalProperties": false
            }
        },
        {
            "name": "propose_file_moves",
            "description": "Propose file reorganizations. This NEVER moves anything: each entry lands in the confirm queue as a proposal awaiting explicit approval. Paths are AIBHS-root-relative (starting with the class folder name), exactly as list_material returns them; destinations may name folders that don't exist yet.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "moves": {
                        "type": "array",
                        "description": "One entry per file to move.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "from": { "type": "string", "description": "Current path of an existing file, AIBHS-root-relative." },
                                "to": { "type": "string", "description": "Proposed new path including the file name, AIBHS-root-relative, same class." },
                                "reason": { "type": "string", "description": "One line on why this destination." }
                            },
                            "required": ["from", "to"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["moves"],
                "additionalProperties": false
            }
        }
    ])
}

/// Runs one tool call. Failures come back as tool results, not transport
/// errors — the model can correct a bad path or a thin query on its own.
pub fn execute(app: &AppHandle, name: &str, input: &Value, ctx: &ToolCtx) -> Outcome {
    let result = match name {
        "get_overview" => with_conn(app, |conn| overview_text(conn, true)).map(Outcome::ok),
        "list_material" => with_conn(app, |conn| list_material(conn, input)),
        "search_material" => search_material(app, input),
        "read_material" => with_conn(app, |conn| read_material(conn, input)),
        "upsert_deadline" => upsert_deadline(app, input),
        "complete_deadline" => complete_deadline(app, input),
        "delete_deadline" => delete_deadline(app, input),
        "upsert_grade_category" => upsert_grade_category(app, input),
        "add_grade_item" => add_grade_item(app, input),
        "write_note" => write_note(app, input),
        "trigger_synthesis" => trigger_synthesis(app, input, ctx),
        "generate_practice" => generate_practice(app, input, ctx),
        "propose_file_moves" => propose_file_moves(app, input),
        other => Err(anyhow::anyhow!(
            "unknown tool '{other}' — the available tools are listed in the tools parameter"
        )),
    };
    result.unwrap_or_else(|e| Outcome::err(format!("{e:#}")))
}

// ---------------------------------------------------------------------------
// Classes

#[derive(Clone)]
pub struct ClassRow {
    pub id: i64,
    pub display_name: String,
    pub folder_name: String,
}

fn class_rows(conn: &Connection) -> Result<Vec<ClassRow>> {
    let mut stmt = conn
        .prepare("SELECT id, display_name, folder_name FROM classes ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ClassRow {
                id: row.get(0)?,
                display_name: row.get(1)?,
                folder_name: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Exact, then substring, then subsequence: the first tier with any match
/// decides, and a tier matching more than one is an error rather than a guess.
/// `keys` returns every string an item may be named by (a class answers to its
/// display name and its folder name; a module only to itself).
///
/// The needle is squashed by the caller, which is also where the empty-needle
/// guard belongs: an empty needle is `contains`-true against everything, so it
/// would match the whole first tier it reached.
fn best_match<'a, T>(
    items: &'a [T],
    needle: &str,
    keys: impl Fn(&T) -> Vec<String>,
) -> Option<Vec<&'a T>> {
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    let mut loose = Vec::new();
    for item in items {
        let candidates = keys(item);
        if candidates.iter().any(|c| c == needle) {
            exact.push(item);
        } else if candidates.iter().any(|c| c.contains(needle)) {
            partial.push(item);
        } else if candidates.iter().any(|c| is_subsequence(needle, c)) {
            loose.push(item);
        }
    }
    [exact, partial, loose].into_iter().find(|t| !t.is_empty())
}

/// Tolerant class lookup: exact name, then substring, then subsequence — so
/// "Biostats" finds "Biostatistics for AI" without the model having to echo the
/// full name. Ambiguity is an error listing the candidates.
fn resolve_class(conn: &Connection, query: &str) -> Result<ClassRow> {
    let rows = class_rows(conn)?;
    let names = || {
        rows.iter()
            .map(|r| r.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let needle = squash(query);
    if needle.is_empty() {
        bail!("which class? one of: {}", names());
    }

    let Some(tier) = best_match(&rows, &needle, |r| {
        vec![squash(&r.display_name), squash(&r.folder_name)]
    }) else {
        bail!("no class matches '{query}'. Classes: {}", names());
    };
    match tier.as_slice() {
        [only] => Ok((*only).clone()),
        many => bail!(
            "'{query}' matches {} classes ({}) — use the full name",
            many.len(),
            many.iter()
                .map(|r| r.display_name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// Lowercased alphanumerics only, so punctuation and spacing never decide a match.
fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_subsequence(needle: &str, haystack: &str) -> bool {
    let mut chars = haystack.chars();
    needle.chars().all(|n| chars.any(|h| h == n))
}

// ---------------------------------------------------------------------------
// get_overview (also the system prompt's injected context, SPEC §9)

const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/// The hub in text. `detailed` adds per-class inventories and guide dates; the
/// compact form is what rides in the system prompt.
pub fn overview_text(conn: &Connection, detailed: bool) -> Result<String> {
    let root = crate::db::aibhs_root(conn)?;
    let classes = class_rows(conn)?;
    let open_deadlines: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deadlines WHERE status = 'open'",
        [],
        |row| row.get(0),
    )?;
    // The first line doubles as the tool chip's label (see Outcome::ok).
    let mut out = format!(
        "Hub snapshot — {} classes, {open_deadlines} open deadline(s)\nAIBHS root: {}\n",
        classes.len(),
        root.display()
    );
    let pending_moves: i64 = conn.query_row(
        "SELECT COUNT(*) FROM move_proposals WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    let pending_deadlines: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deadline_proposals WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    // Both confirm queues on one line: a proposal the overview never mentions
    // is one chat cannot remind anyone about.
    let mut waiting = Vec::new();
    if pending_moves > 0 {
        waiting.push(format!("{pending_moves} file move proposal(s)"));
    }
    if pending_deadlines > 0 {
        waiting.push(format!("{pending_deadlines} deadline proposal(s)"));
    }
    if !waiting.is_empty() {
        out.push_str(&format!(
            "{} awaiting Daniel's approval\n",
            waiting.join(" and ")
        ));
    }

    for class in classes {
        let (color, room, instructors, credits, exam_start, exam_end): (
            String,
            String,
            String,
            i64,
            Option<String>,
            Option<String>,
        ) = conn.query_row(
            "SELECT color, room, instructors, credits, final_exam_start, final_exam_end
             FROM classes WHERE id = ?1",
            [class.id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;

        let mut meeting_stmt = conn.prepare(
            "SELECT weekday, start_time, end_time FROM meetings
             WHERE class_id = ?1 ORDER BY weekday, start_time",
        )?;
        let meetings = meeting_stmt
            .query_map([class.id], |row| {
                let weekday: i64 = row.get(0)?;
                let start: String = row.get(1)?;
                let end: String = row.get(2)?;
                Ok(format!(
                    "{} {start}–{end}",
                    WEEKDAYS
                        .get((weekday.max(1) - 1) as usize)
                        .unwrap_or(&"?")
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        out.push_str(&format!(
            "\n## {} (folder: {}, accent {color})\n",
            class.display_name, class.folder_name
        ));
        out.push_str(&format!(
            "Meets {} · {room} · {credits} credits · {instructors}\n",
            if meetings.is_empty() {
                "—".to_string()
            } else {
                meetings.join(", ")
            }
        ));
        if let (Some(start), Some(end)) = (&exam_start, &exam_end) {
            out.push_str(&format!("Final exam: {start} to {end}\n"));
        }

        let (indexed, extracted): (i64, i64) = conn.query_row(
            "SELECT COUNT(*),
                    SUM(CASE WHEN extracted_sha256 IS NOT NULL
                              AND extracted_sha256 = sha256 THEN 1 ELSE 0 END)
             FROM files WHERE class_id = ?1",
            [class.id],
            |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
        )?;
        let modules = module_counts(conn, class.id)?;
        let module_list = modules
            .iter()
            .map(|(name, count)| format!("{name} ({count})"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "Material: {indexed} files indexed, {extracted} with a current extract{}\n",
            if module_list.is_empty() {
                " · no modules yet".to_string()
            } else {
                format!(" · modules: {module_list}")
            }
        ));

        let guides = crate::guides::list_guides(conn, class.id)?;
        if guides.is_empty() {
            out.push_str("Guides: none generated yet\n");
        } else {
            let now_ts = now();
            let described = guides
                .iter()
                .map(|g| {
                    let scope = crate::guides::scope_label(&g.scope);
                    if detailed {
                        format!(
                            "{scope} — {} ({}, {})",
                            if g.stale { "STALE" } else { "fresh" },
                            g.rel_path,
                            days_ago(now_ts, g.generated_at)
                        )
                    } else {
                        format!("{scope} ({})", if g.stale { "STALE" } else { "fresh" })
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(&format!("Guides: {described}\n"));
        }
        if detailed {
            out.push_str(&grades_line(conn, class.id)?);
        }
    }

    let mut deadline_stmt = conn.prepare(
        "SELECT d.id, c.display_name, d.title, d.kind, d.due_at, d.notes
         FROM deadlines d JOIN classes c ON c.id = d.class_id
         WHERE d.status = 'open' ORDER BY d.due_at LIMIT 25",
    )?;
    let deadlines = deadline_stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let class: String = row.get(1)?;
            let title: String = row.get(2)?;
            let kind: String = row.get(3)?;
            let due: String = row.get(4)?;
            let notes: Option<String> = row.get(5)?;
            // The [#id] is what upsert/complete/delete_deadline address.
            Ok(format!(
                "- [#{id}] {due} · {class} · {title} ({kind}){}",
                notes.map(|n| format!(" — {n}")).unwrap_or_default()
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    out.push_str("\n## Open deadlines\n");
    out.push_str(&if deadlines.is_empty() {
        "None recorded.\n".to_string()
    } else {
        format!("{}\n", deadlines.join("\n"))
    });

    Ok(out)
}

/// Depth-0 folders holding indexed files, with their file counts.
fn module_counts(conn: &Connection, class_id: i64) -> Result<BTreeMap<String, usize>> {
    let mut stmt =
        conn.prepare("SELECT rel_path FROM files WHERE class_id = ?1 ORDER BY rel_path")?;
    let paths = stmt
        .query_map([class_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for path in paths {
        let module = match path.split_once('/') {
            Some((first, _)) => first.to_string(),
            None => "(class folder)".to_string(),
        };
        *counts.entry(module).or_default() += 1;
    }
    Ok(counts)
}

// ---------------------------------------------------------------------------
// list_material

fn list_material(conn: &Connection, input: &Value) -> Result<Outcome> {
    let class = resolve_class(conn, &str_arg(input, "class")?)?;
    let subpath = opt_str_arg(input, "subpath").map(|s| s.trim_matches('/').to_string());
    let root = crate::db::aibhs_root(conn)?;

    let mut stmt = conn.prepare(
        "SELECT rel_path, kind, size, extracted_sha256 = sha256 FROM files
         WHERE class_id = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt
        .query_map([class.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<bool>>(3)?.unwrap_or(false),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut lines = Vec::new();
    let mut shown = 0usize;
    let mut skipped = 0usize;
    let mut matched = 0usize;
    for (rel_path, kind, size, extracted) in &rows {
        if let Some(sub) = &subpath {
            let inside = rel_path == sub || rel_path.starts_with(&format!("{sub}/"));
            if !inside {
                continue;
            }
        }
        matched += 1;
        if shown >= MAX_LIST_LINES {
            skipped += 1;
            continue;
        }
        shown += 1;
        lines.push(format!(
            "{}/{rel_path} · {kind} · {} · {}",
            class.folder_name,
            format_size(*size),
            if *extracted { "extract ✓" } else { "no extract yet" }
        ));
    }

    let mut text = match subpath.as_deref() {
        Some(sub) => format!(
            // matched, not rows.len(): the denominator has to be the count
            // under the subpath, or "5 of 100 … under 'Module 1'" reports the
            // whole class to both the model and the sidebar chip.
            "{} — {shown} of {matched} indexed file(s) under '{sub}'\n",
            class.display_name,
        ),
        None => format!(
            "{} — {} indexed file(s)\n",
            class.display_name,
            rows.len()
        ),
    };
    text.push_str(&format!(
        "Extract path rule: {}/{EXTRACTS_DIR}/<source path>.md\n\n",
        class.folder_name
    ));
    if lines.is_empty() {
        text.push_str("No indexed files here.\n");
    } else {
        text.push_str(&lines.join("\n"));
        text.push('\n');
        if skipped > 0 {
            text.push_str(&format!("({skipped} more — narrow with subpath)\n"));
        }
    }

    // Study guides and notes live outside the file index (they are app-managed,
    // SPEC §4) but are both searchable and readable.
    let guides = crate::guides::list_guides(conn, class.id)?;
    text.push_str("\nStudy guides:\n");
    if guides.is_empty() {
        text.push_str("none generated yet\n");
    } else {
        for guide in &guides {
            text.push_str(&format!(
                "{}/{} · {}\n",
                class.folder_name,
                guide.rel_path,
                if guide.stale { "STALE" } else { "fresh" }
            ));
        }
    }

    // notes::list_dir_files already applies the read-dir/hidden/sort policy;
    // only the names are needed here.
    let notes: Vec<String> = crate::notes::list_dir_files(
        &root.join(&class.folder_name).join(NOTES_DIR),
        NOTES_DIR,
    )
    .into_iter()
    .map(|f| f.name)
    .collect();
    text.push_str("\nNotes:\n");
    if notes.is_empty() {
        text.push_str("none yet\n");
    } else {
        for note in &notes {
            text.push_str(&format!("{}/{NOTES_DIR}/{note}\n", class.folder_name));
        }
    }

    Ok(Outcome::ok(text))
}

// ---------------------------------------------------------------------------
// search_material (ripgrep over extracts, corpus notes, notes and guides — SPEC §9)

/// The lock is held only long enough to resolve the class folders; the grep
/// itself runs without it. Spawning ripgrep and waiting for it under the app's
/// single connection blocked every other command, every chat tool, and the job
/// runner for the whole search.
fn search_material(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let query = str_arg(input, "query")?;
    let (root, classes) = with_conn(app, |conn| {
        let root = crate::db::aibhs_root(conn)?;
        let classes = match opt_str_arg(input, "class") {
            Some(name) => vec![resolve_class(conn, &name)?],
            None => class_rows(conn)?,
        };
        Ok((root, classes))
    })?;

    let mut dirs = Vec::new();
    for class in &classes {
        let base = root.join(&class.folder_name);
        // The corpus joins the extract cache here (SPEC §8.5): a lecture's
        // distilled content is the only searchable form of what was said out
        // loud, and it is what a question about a session should find.
        for sub in [EXTRACTS_DIR, CORPUS_DIR, NOTES_DIR, GUIDES_DIR] {
            let dir = base.join(sub);
            if dir.is_dir() {
                dirs.push(dir);
            }
        }
    }
    if dirs.is_empty() {
        return Ok(Outcome::ok(
            "Nothing to search — no extracts, notes or guides exist yet for that \
             scope. The material may not have been scanned and extracted.",
        ));
    }

    let output = match run_rg(&query, &dirs, false) {
        Ok(output) if output.status.code() == Some(2) => {
            // Exit 2 is usually an invalid regex; the model often means a
            // literal phrase, so retry the query as fixed strings.
            run_rg(&query, &dirs, true)?
        }
        other => other?,
    };
    if output.status.code() == Some(2) {
        bail!(
            "search failed: {}",
            truncate(String::from_utf8_lossy(&output.stderr).trim(), 300)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files = Vec::new();
    let mut hits = 0usize;
    let mut lines = Vec::new();
    for raw in stdout.lines() {
        // --null puts a NUL after the path, so paths containing ':' are safe.
        let Some((path, rest)) = raw.split_once('\0') else {
            continue;
        };
        let Some((line_no, text)) = rest.split_once(':') else {
            continue;
        };
        hits += 1;
        let rel = Path::new(path)
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        if !files.contains(&rel) {
            files.push(rel.clone());
        }
        if lines.len() < MAX_SEARCH_LINES {
            lines.push(format!("{rel}:{line_no}: {}", truncate(text.trim(), MAX_MATCH_CHARS)));
        }
    }

    let scope = if classes.len() == 1 {
        format!(" in {}", classes[0].display_name)
    } else {
        String::new()
    };
    let mut text = format!(
        "{hits} matching line(s) in {} file(s) for /{query}/{scope}\n\n",
        files.len()
    );
    if lines.is_empty() {
        text.push_str(
            "No matches. Try a shorter phrase, a synonym, or a single distinctive term.\n",
        );
    } else {
        text.push_str(&lines.join("\n"));
        text.push('\n');
        if hits > lines.len() {
            text.push_str(&format!(
                "\n(showing the first {} of {hits} matching lines — narrow the query to see more)\n",
                lines.len()
            ));
        }
    }

    Ok(Outcome::ok(text))
}

fn run_rg(query: &str, dirs: &[PathBuf], fixed: bool) -> Result<std::process::Output> {
    let mut cmd = Command::new(rg_bin());
    cmd.args([
        "--line-number",
        "--with-filename",
        "--no-heading",
        "--null",
        "--color",
        "never",
        "--smart-case",
        // Extracts live under the hidden .classhub directory.
        "--hidden",
        "--no-ignore",
        "--max-columns",
        "400",
        "--max-columns-preview",
        "--max-count",
        "6",
        // A session digest is written twice, as HTML for reading and markdown
        // for exactly this — so searching both returns every hit twice, at
        // twice the tokens, with the HTML copy carrying its own markup through
        // the match. Module and master guides are HTML-only and unaffected.
        "--glob",
        "!Sessions/*.html",
    ]);
    if fixed {
        cmd.arg("--fixed-strings");
    }
    cmd.arg("--regexp").arg(query);
    for dir in dirs {
        cmd.arg(dir);
    }
    cmd.output()
        .context("running ripgrep (install it with `brew install ripgrep`)")
}

fn rg_bin() -> PathBuf {
    // A bundled app's PATH does not include Homebrew, so look there first.
    for candidate in ["/opt/homebrew/bin/rg", "/usr/local/bin/rg"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("rg")
}

// ---------------------------------------------------------------------------
// read_material (bounded reads, never binaries — SPEC §9)

fn read_material(conn: &Connection, input: &Value) -> Result<Outcome> {
    let rel_path = str_arg(input, "path")?;
    let offset = input
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_READ_LINES as u64)
        .clamp(1, MAX_READ_LINES as u64) as usize;

    let root = crate::db::aibhs_root(conn)?;
    let abs = safe_join(&root, &rel_path)?;
    let meta = fs::symlink_metadata(&abs)
        .with_context(|| format!("no such file: {rel_path}"))?;
    if !meta.is_file() {
        bail!("not a file: {rel_path}");
    }

    let ext = abs
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if !READABLE_EXTS.contains(&ext.as_str()) {
        match rel_path.split_once('/') {
            Some((class_folder, inside)) => bail!(
                "{ext} files are binary — read the extract instead: \
                 {class_folder}/{EXTRACTS_DIR}/{inside}.md"
            ),
            None => bail!("{ext} files cannot be read as text"),
        }
    }
    if meta.len() as usize > MAX_READ_BYTES {
        bail!("file is too large to read ({})", format_size(meta.len() as i64));
    }

    let bytes = fs::read(&abs).with_context(|| format!("reading {rel_path}"))?;
    let content = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{rel_path} is not UTF-8 text"))?;
    let all: Vec<&str> = content.lines().collect();
    let total = all.len();
    // Stated as a fact rather than as the contradictory "lines 1–0 of 0" the
    // window arithmetic would otherwise produce.
    if total == 0 {
        return Ok(Outcome::ok(format!("{rel_path} is empty (0 lines).")));
    }
    if offset > total.max(1) {
        bail!("offset {offset} is past the end of {rel_path} ({total} lines)");
    }

    let start = offset - 1;
    let end = (start + limit).min(total);
    let mut text = format!(
        "{rel_path} — lines {}–{end} of {total}\n\n",
        start + 1
    );
    for (index, line) in all[start..end].iter().enumerate() {
        text.push_str(&format!(
            "{:>5}| {}\n",
            start + index + 1,
            truncate(line, MAX_LINE_CHARS)
        ));
    }
    if end < total {
        text.push_str(&format!(
            "\n({} more line(s) — call again with offset {})\n",
            total - end,
            end + 1
        ));
    }

    Ok(Outcome::ok(text))
}

/// Joins an AIBHS-root-relative path, rejecting anything that could escape the
/// root (`..`, absolute paths, `~`).
fn safe_join(root: &Path, rel_path: &str) -> Result<PathBuf> {
    let rel = Path::new(rel_path);
    if rel_path.trim().is_empty() {
        bail!("path is required");
    }
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("path must be relative to the AIBHS root: {rel_path}");
    }
    Ok(root.join(rel))
}

// ---------------------------------------------------------------------------
// Write tools — deadlines (SPEC §9/§11; rows carry source='agent').
// Validation and caps come from the deadlines module — one rule set for the
// chat tool, the UI form, and the syllabus scan.

fn upsert_deadline(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        match input.get("id").and_then(Value::as_i64) {
            Some(id) => amend_deadline(conn, &class, id, input),
            None => create_deadline(conn, &class, input),
        }
    })?;
    emit_hub_change(app, "deadlines");
    Ok(outcome)
}

fn create_deadline(conn: &Connection, class: &ClassRow, input: &Value) -> Result<Outcome> {
    let title = truncate(&str_arg(input, "title")?, MAX_TITLE_CHARS);
    let kind = opt_str_arg(input, "kind").unwrap_or_else(|| "other".to_string());
    if !DEADLINE_KINDS.contains(&kind.as_str()) {
        bail!("kind must be one of: {}", DEADLINE_KINDS.join(", "));
    }
    let due_at = str_arg(input, "due_at")?;
    if !valid_due_at(&due_at) {
        bail!("due_at must be ISO — YYYY-MM-DD or YYYY-MM-DDTHH:MM, got '{due_at}'");
    }
    let notes = opt_str_arg(input, "notes").map(|n| truncate(&n, MAX_NOTES_CHARS));
    conn.execute(
        "INSERT INTO deadlines (class_id, title, kind, due_at, notes, status, source)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', 'agent')",
        params![class.id, title, kind, due_at, notes],
    )?;
    let id = conn.last_insert_rowid();
    audit(
        conn,
        "chat.upsert_deadline",
        json!({ "id": id, "classId": class.id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes, "created": true }),
    )?;
    Ok(Outcome::ok(format!(
        "Deadline recorded — {title} ({kind}) due {due_at} · {} [#{id}]\n\
         Amend with upsert_deadline(id: {id}); close with complete_deadline when it's done.",
        class.display_name
    )))
}

fn amend_deadline(conn: &Connection, class: &ClassRow, id: i64, input: &Value) -> Result<Outcome> {
    let before = conn
        .query_row(
            "SELECT class_id, title, kind, due_at, notes, status FROM deadlines WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, String>(5)?,
                ))
            },
        )
        .optional()?
        .with_context(|| format!("no deadline #{id} — get_overview lists the ids"))?;
    let (class_id, old_title, old_kind, old_due, old_notes, status) = before;
    if class_id != class.id {
        bail!("deadline #{id} belongs to a different class");
    }

    let title = truncate(
        &opt_str_arg(input, "title").unwrap_or_else(|| old_title.clone()),
        MAX_TITLE_CHARS,
    );
    let kind = opt_str_arg(input, "kind").unwrap_or_else(|| old_kind.clone());
    if !DEADLINE_KINDS.contains(&kind.as_str()) {
        bail!("kind must be one of: {}", DEADLINE_KINDS.join(", "));
    }
    let due_at = opt_str_arg(input, "due_at").unwrap_or_else(|| old_due.clone());
    if !valid_due_at(&due_at) {
        bail!("due_at must be ISO — YYYY-MM-DD or YYYY-MM-DDTHH:MM, got '{due_at}'");
    }
    let notes = opt_str_arg(input, "notes")
        .or_else(|| old_notes.clone())
        .map(|n| truncate(&n, MAX_NOTES_CHARS));
    conn.execute(
        "UPDATE deadlines SET title = ?1, kind = ?2, due_at = ?3, notes = ?4 WHERE id = ?5",
        params![title, kind, due_at, notes, id],
    )?;
    audit(
        conn,
        "chat.upsert_deadline",
        json!({ "id": id, "classId": class.id,
                "before": { "title": old_title, "kind": old_kind, "dueAt": old_due, "notes": old_notes },
                "after": { "title": title, "kind": kind, "dueAt": due_at, "notes": notes } }),
    )?;
    Ok(Outcome::ok(format!(
        "Deadline amended — {title} ({kind}) due {due_at} · {} [#{id}]{}",
        class.display_name,
        if status == "done" { "\n(It is marked done.)" } else { "" }
    )))
}

fn complete_deadline(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let id = int_arg(input, "id")?;
        let (title, class_name, status) = deadline_brief(conn, id)?;
        if status == "done" {
            return Ok(Outcome::ok(format!(
                "Deadline was already done — {title} · {class_name} [#{id}]"
            )));
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE deadlines SET status = 'done' WHERE id = ?1", [id])?;
        audit(&tx, "chat.complete_deadline", json!({ "id": id }))?;
        tx.commit()?;
        Ok(Outcome::ok(format!(
            "Deadline done — {title} · {class_name} [#{id}]"
        )))
    })?;
    emit_hub_change(app, "deadlines");
    Ok(outcome)
}

fn delete_deadline(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let id = int_arg(input, "id")?;
        let row = conn
            .query_row(
                "SELECT class_id, title, kind, due_at, notes, status, source
                 FROM deadlines WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("no deadline #{id} — get_overview lists the ids"))?;
        let (class_id, title, kind, due_at, notes, status, source) = row;
        // One transaction, because the promise below — that the row survives
        // in the audit log — is only true if both statements land together.
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        // The full row rides the audit entry, so a deletion is recoverable.
        audit(
            &tx,
            "chat.delete_deadline",
            json!({ "id": id, "classId": class_id, "title": title, "kind": kind,
                    "dueAt": due_at, "notes": notes, "status": status, "source": source }),
        )?;
        tx.commit()?;
        Ok(Outcome::ok(format!(
            "Deadline deleted — {title} (was due {due_at}) [#{id}]\nThe full row is kept in the audit log."
        )))
    })?;
    emit_hub_change(app, "deadlines");
    Ok(outcome)
}

fn deadline_brief(conn: &Connection, id: i64) -> Result<(String, String, String)> {
    conn.query_row(
        "SELECT d.title, c.display_name, d.status
         FROM deadlines d JOIN classes c ON c.id = d.class_id WHERE d.id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()?
    .with_context(|| format!("no deadline #{id} — get_overview lists the ids"))
}

// ---------------------------------------------------------------------------
// Write tools — grades (SPEC §11 math: weighted over graded categories only)

fn upsert_grade_category(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let name = str_arg(input, "name")?;
        let weight = float_arg(input, "weight")?;
        if !(0.0..=100.0).contains(&weight) {
            bail!("weight is a percentage between 0 and 100");
        }
        let existing: Option<(i64, f64)> = conn
            .query_row(
                "SELECT id, weight FROM grade_categories
                 WHERE class_id = ?1 AND LOWER(name) = LOWER(?2)",
                params![class.id, name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let verb = match existing {
            Some((id, old_weight)) => {
                conn.execute(
                    "UPDATE grade_categories SET name = ?1, weight = ?2 WHERE id = ?3",
                    params![name, weight, id],
                )?;
                audit(
                    conn,
                    "chat.upsert_grade_category",
                    json!({ "id": id, "classId": class.id, "name": name,
                            "weight": weight, "previousWeight": old_weight }),
                )?;
                "updated"
            }
            None => {
                conn.execute(
                    "INSERT INTO grade_categories (class_id, name, weight) VALUES (?1, ?2, ?3)",
                    params![class.id, name, weight],
                )?;
                audit(
                    conn,
                    "chat.upsert_grade_category",
                    json!({ "id": conn.last_insert_rowid(), "classId": class.id,
                            "name": name, "weight": weight }),
                )?;
                "created"
            }
        };
        Ok(Outcome::ok(format!(
            "Grade category {verb} — {name} at {}% · {}\n{}",
            trim_num(weight),
            class.display_name,
            weights_line(conn, class.id)?
        )))
    })?;
    emit_hub_change(app, "grades");
    Ok(outcome)
}

fn add_grade_item(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let category = str_arg(input, "category")?;
        let found: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, name FROM grade_categories
                 WHERE class_id = ?1 AND LOWER(name) = LOWER(?2)",
                params![class.id, category],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((category_id, category_name)) = found else {
            let names: Vec<String> = conn
                .prepare("SELECT name FROM grade_categories WHERE class_id = ?1 ORDER BY id")?
                .query_map([class.id], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            bail!(
                "no grade category '{category}' in {} — {}",
                class.display_name,
                if names.is_empty() {
                    "create one with upsert_grade_category first".to_string()
                } else {
                    format!("existing: {}", names.join(", "))
                }
            );
        };
        let name = str_arg(input, "name")?;
        let score = float_arg(input, "score")?;
        let max_score = float_arg(input, "max_score")?;
        if max_score <= 0.0 {
            bail!("max_score must be positive");
        }
        if score < 0.0 {
            bail!("score cannot be negative");
        }
        let graded_at = opt_str_arg(input, "graded_at");
        conn.execute(
            "INSERT INTO grade_items (category_id, name, score, max_score, graded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![category_id, name, score, max_score, graded_at],
        )?;
        audit(
            conn,
            "chat.add_grade_item",
            json!({ "id": conn.last_insert_rowid(), "categoryId": category_id, "name": name,
                    "score": score, "maxScore": max_score, "gradedAt": graded_at }),
        )?;
        let grade = weighted_grade(conn, class.id)?
            .map(|pct| format!("\nCurrent weighted grade over graded items: {pct:.1}%."))
            .unwrap_or_default();
        Ok(Outcome::ok(format!(
            "Grade recorded — {name}: {}/{} in {category_name} · {}{grade}",
            trim_num(score),
            trim_num(max_score),
            class.display_name
        )))
    })?;
    emit_hub_change(app, "grades");
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// Write tools — notes, synthesis triggers, practice, move proposals

fn write_note(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let title = str_arg(input, "title")?;
        let content = str_arg(input, "content_md")?;
        // notes::write_note commits the audit row (with the replaced content)
        // before the file write — an overwrite can never silently destroy a note.
        let written =
            crate::notes::write_note(conn, class.id, &title, &content, "chat.write_note")?;
        let lines = content.lines().count();
        Ok(Outcome::ok(format!(
            "Note {} — {}/{} ({lines} line{})\n{}",
            if written.created { "written" } else { "updated" },
            class.folder_name,
            written.rel_path,
            if lines == 1 { "" } else { "s" },
            if written.created {
                "Cite it by that full path so it can be opened from the answer."
            } else {
                "The previous version is kept in the audit log."
            }
        )))
    })?;
    emit_hub_change(app, "notes");
    Ok(outcome)
}

/// `master` (and natural synonyms) selects the semester master; anything else
/// must match one of the class's module folders — same tolerant matching as
/// classes, so "module 1" or "m1" finds "Module 1".
fn resolve_scope(conn: &Connection, class: &ClassRow, scope: &str) -> Result<Option<String>> {
    let needle = squash(scope);
    if ["master", "semester", "semestermaster", "wholesemester", "all"]
        .contains(&needle.as_str())
    {
        return Ok(None);
    }
    let modules: Vec<String> = module_counts(conn, class.id)?
        .into_keys()
        .filter(|m| m != "(class folder)")
        .collect();
    if modules.is_empty() {
        bail!("{} has no module folders yet", class.display_name);
    }
    // Same guard as `resolve_class`, and for a sharper reason: an empty needle
    // is `contains`-true against every candidate, so a scope of "?" or "—"
    // squashes to nothing, matches every module, and — in a class with exactly
    // one — resolves silently. That would enqueue a full synthesis run against
    // a scope the model never actually named.
    if needle.is_empty() {
        bail!(
            "which module? one of: {}",
            modules.join(", ")
        );
    }
    let Some(tier) = best_match(&modules, &needle, |m| vec![squash(m)]) else {
        bail!(
            "no module matches '{scope}' in {}. Modules: {} — or 'master' for the semester master",
            class.display_name,
            modules.join(", ")
        );
    };
    match tier.as_slice() {
        [only] => Ok(Some((*only).clone())),
        many => bail!(
            "'{scope}' matches {} modules ({}) — be specific",
            many.len(),
            many.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn trigger_synthesis(app: &AppHandle, input: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let (class, scope) = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let scope = resolve_scope(conn, &class, &str_arg(input, "scope")?)?;
        Ok((class, scope))
    })?;
    // Enqueued outside the DB lock — the job runner takes the lock itself.
    match scope {
        None => {
            let job_id = crate::guides::synthesize_master(app, class.id, ctx.today)?;
            Ok(Outcome::ok(format!(
                "Semester master synthesis queued — {} (job #{job_id})\n\
                 Exclusive job: it waits for running jobs to drain, then runs alone — often \
                 30+ minutes. Progress is live in the Job Center; the guide appears in the \
                 workspace when it succeeds.",
                class.display_name
            )))
        }
        Some(module_rel) => {
            let job_id =
                crate::guides::synthesize_module(app, class.id, &module_rel, ctx.today)?;
            Ok(Outcome::ok(format!(
                "Module guide synthesis queued — {module_rel} · {} (job #{job_id})\n\
                 Progress is live in the Job Center; the guide appears on the module row when \
                 it succeeds (typically 10–30 minutes).",
                class.display_name
            )))
        }
    }
}

fn generate_practice(app: &AppHandle, input: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let focus = opt_str_arg(input, "focus");
    let (class, scope) = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let scope = resolve_scope(conn, &class, &str_arg(input, "scope")?)?;
        Ok((class, scope))
    })?;
    let scope_rel = scope.as_deref().unwrap_or("master");
    let (job_id, output_rel) = crate::guides::generate_practice(
        app,
        class.id,
        scope_rel,
        focus.as_deref(),
        ctx.today,
        ctx.today_iso,
    )?;
    Ok(Outcome::ok(format!(
        "Practice exam queued — {} · {} (job #{job_id})\n\
         It will land at {}/{output_rel} and show in the workspace's practice list when \
         the job succeeds (live in the Job Center now).{}",
        if scope_rel == "master" { "semester scope" } else { scope_rel },
        class.display_name,
        class.folder_name,
        focus
            .map(|f| format!("\nFocus: {f}"))
            .unwrap_or_default()
    )))
}

fn propose_file_moves(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let outcome = with_conn(app, |conn| {
        let moves = input
            .get("moves")
            .and_then(Value::as_array)
            .filter(|m| !m.is_empty())
            .context("missing required argument 'moves' (a non-empty array)")?;
        let root = crate::db::aibhs_root(conn)?;

        // Everything validates before anything inserts, so one bad entry can
        // be corrected without half a batch landing in the queue.
        let validated = moves
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                validate_move(conn, &root, entry)
                    .with_context(|| format!("move {} of {}", index + 1, moves.len()))
            })
            .collect::<Result<Vec<_>>>()?;

        // The batch was validated as a unit, so it lands as one: a failure
        // partway through would otherwise leave the queue holding half of a
        // proposal set whose result text describes all of it.
        let tx = conn.unchecked_transaction()?;
        for mv in &validated {
            crate::sorter::upsert_proposal(
                &tx,
                mv.class_id,
                "chat",
                &mv.source_rel,
                &mv.dest_rel,
                &mv.reason,
                None,
            )?;
        }
        tx.commit()?;

        let mut text = format!(
            "{} file move(s) proposed — nothing has moved; each waits for Daniel's approval\n",
            validated.len()
        );
        for mv in &validated {
            text.push_str(&format!("- {} → {}\n", mv.from_display, mv.to_display));
        }
        text.push_str(
            "Each waits in the class workspace's inbox queue, where Daniel can approve, \
             redirect, or decline it.",
        );
        Ok(Outcome::ok(text))
    })?;
    emit_hub_change(app, "proposals");
    Ok(outcome)
}

struct ValidatedMove {
    class_id: i64,
    /// Class-relative, matching the files-table convention.
    source_rel: String,
    dest_rel: String,
    reason: String,
    from_display: String,
    to_display: String,
}

fn validate_move(conn: &Connection, root: &Path, entry: &Value) -> Result<ValidatedMove> {
    let from = str_arg(entry, "from")?;
    let to = str_arg(entry, "to")?;
    let reason =
        opt_str_arg(entry, "reason").unwrap_or_else(|| "proposed in chat".to_string());
    let (from_class, source_rel) = split_class_path(conn, &from)?;
    let (to_class, dest_rel) = split_class_path(conn, &to)?;
    if from_class.id != to_class.id {
        bail!("'{from}' and '{to}' are in different classes — a move stays within one class");
    }
    if !root.join(&from).is_file() {
        bail!("'{from}' is not a file on disk — use paths exactly as list_material returns them");
    }
    if Path::new(&dest_rel).file_name().is_none() {
        bail!("'{to}' must include the destination file name");
    }
    // The destination policy itself belongs to the drop-to-sort validator, and
    // is called rather than restated: the copy that used to live here had
    // already drifted, missing the symlinked-ancestor check that stops a move
    // physically landing outside the class folder while the index records it
    // as inside. Approval re-runs the same function, so the two agree by
    // construction now instead of by comment.
    crate::sorter::validate_dest(&root.join(&from_class.folder_name), &source_rel, &dest_rel)
        .with_context(|| format!("'{to}' is not a valid destination"))?;
    Ok(ValidatedMove {
        class_id: from_class.id,
        source_rel,
        dest_rel,
        reason,
        from_display: from,
        to_display: to,
    })
}

/// Splits an AIBHS-root-relative path into its class and the class-relative
/// remainder, rejecting traversal and unknown class folders.
fn split_class_path(conn: &Connection, path: &str) -> Result<(ClassRow, String)> {
    if Path::new(path)
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("'{path}' must be relative to the AIBHS root");
    }
    let (folder, rest) = path
        .split_once('/')
        .with_context(|| format!("'{path}' must start with a class folder name"))?;
    if rest.trim().is_empty() {
        bail!("'{path}' names no file inside the class");
    }
    let class = class_rows(conn)?
        .into_iter()
        .find(|c| c.folder_name == folder)
        .with_context(|| {
            format!("'{folder}' is not a class folder — start paths with the class folder name")
        })?;
    Ok((class, rest.to_string()))
}

// ---------------------------------------------------------------------------
// Small helpers

fn str_arg(input: &Value, key: &str) -> Result<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .with_context(|| format!("missing required argument '{key}'"))
}

fn opt_str_arg(input: &Value, key: &str) -> Option<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int_arg(input: &Value, key: &str) -> Result<i64> {
    let Some(value) = input.get(key) else {
        bail!("missing required integer argument '{key}'");
    };
    // A model answering an "integer" field with 3.0 meant 3; telling it the
    // argument was missing invites it to retry the call unchanged.
    value
        .as_i64()
        .or_else(|| value.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        .with_context(|| format!("'{key}' must be an integer, got {value}"))
}

fn float_arg(input: &Value, key: &str) -> Result<f64> {
    input
        .get(key)
        .and_then(Value::as_f64)
        .with_context(|| format!("missing required numeric argument '{key}'"))
}

/// Relative age in whole days — std alone cannot format a local date, and the
/// agent only needs to know how current something is.
fn days_ago(now: i64, then: i64) -> String {
    let days = (now - then).max(0) / 86_400;
    match days {
        0 => "generated today".to_string(),
        1 => "generated yesterday".to_string(),
        n => format!("generated {n} days ago"),
    }
}

/// Shared with the drop-to-sort prompt builder — both surfaces feed the model
/// and should describe sizes identically. Mirrored by `formatSize` in
/// src/lib/materials.ts, which renders the same sizes in the inbox; change one,
/// change the other.
pub fn format_size(bytes: i64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in ["KB", "MB", "GB"] {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    if value >= 10.0 {
        format!("{} {unit}", value.round())
    } else {
        format!("{value:.1} {unit}")
    }
}

