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
    CORPUS_DIR, EXTRACTS_DIR, GUIDES_DIR, NOTES_DIR, audit, emit_hub_change, notify, now,
    truncate, with_conn,
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
/// How long the pattern fallback may run. Ripgrep over the whole tree answers
/// in well under a second; anything past this is a pattern backtracking, and
/// the chat thread has no watchdog of its own.
const RG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

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
/// this is the one place the list lives: the frontend reads it through
/// `chat_tool_names` rather than keeping a copy that a new write tool could
/// quietly fall out of.
pub const WRITE_TOOLS: &[&str] = &[
    "upsert_deadline",
    "complete_deadline",
    "delete_deadline",
    "upsert_grade_category",
    "add_grade_item",
    "write_note",
    "trigger_synthesis",
    "generate_practice",
    "propose_file_moves",
    "approve_move",
    "dismiss_move",
    "approve_all_moves",
    "run_sort",
    "run_syllabus_scan",
    "approve_deadlines",
    "add_lecture",
    "run_shift",
    "undo_last",
];

pub fn is_write(name: &str) -> bool {
    WRITE_TOOLS.contains(&name)
}

/// Tool schemas sent with every request (SPEC §9: four read tools, nine write
/// tools). Thirteen schemas ride every round of the loop — roughly two
/// thousand tokens, a fine price for the model always seeing its full reach.
pub fn definitions() -> Value {
    json!([
        {
            "name": "get_overview",
            "description": "Snapshot of the hub, in full: every class, when it meets, how the course divides itself (its weeks, modules or parts, with dates and which one is current), the professor's latest Canvas announcements with their text, the lectures filed under each division and whether they have been distilled or have a session document, what material is indexed and extracted per folder, which study guides exist and whether they are stale, every deadline or file-move proposal waiting for approval WITH ITS ID — which is what approve_move and approve_deadlines need — and open deadlines. Use it for questions about the schedule, this week, what the professor announced, deadlines, what exists, what is waiting, or what has been synthesized, and before approving anything; not to find content inside material. Pass a class when the question is about one, which also brings back what the professor flagged in it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": {
                        "type": "string",
                        "description": "Optional class name to report on that class alone, in more detail."
                    }
                },
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
            "description": "Ranked full-text search over every extract, distilled lecture note, note and study guide. This is the primary way to find content: search before reading. Returns the twenty most relevant DOCUMENTS, best first, each with the stretch of text that matched and the line it is on — hand the path and line straight to read_material. Write the query as words: a phrase ('central tendency') ranks documents holding the phrase above ones holding the words apart, and '|' separates alternatives ('mean|median|mode'). If a query comes back thin, try a synonym or a narrower term.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Words or a phrase, e.g. 'central tendency' or 'mean|median|mode'."
                    },
                    "class": {
                        "type": "string",
                        "description": "Optional class name to search only that class."
                    },
                    "regex": {
                        "type": "boolean",
                        "description": "Set true to match the query as a regular expression instead — for a shape rather than words (a date, a character class). Slower, unranked, and returns matching lines rather than ranked documents."
                    }
                },
                "required": ["query"],
                "additionalProperties": false
            }
        },
        {
            "name": "read_material",
            "description": "Read a bounded window of a text file by its AIBHS-relative path: an extract, note, study guide, or text source such as .R, .Rmd, .py, .csv or .md. Binary or bulky sources (.pptx, .pdf, .docx, .ipynb) cannot be read — read their extract at '<class folder>/.classhub/extracts/<source path>.md' instead. Returns numbered lines; page through long files with offset.",
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
            "description": "Queue a study-guide synthesis job — for one of the course's own divisions (a week, module or part as the overview lists it), for one folder of material, or 'master' for the semester master. A division's guide is built from its folder, if it has one, and its distilled lectures. It appears in the Job Center immediately and runs on the Claude subscription: long (10–30+ minutes) and token-heavy, so trigger only on a clear request, one job per ask, and never re-trigger a scope that is already queued or running. The master runs exclusively after the queue drains. Report the job as queued, never as done.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "scope": { "type": "string", "description": "A division as the overview names it (e.g. 'Week 3', or its topic), a folder name (e.g. 'Module 1'), or 'master' for the semester master." }
                },
                "required": ["class", "scope"],
                "additionalProperties": false
            }
        },
        {
            "name": "generate_practice",
            "description": "Queue a practice-exam job for one of the course's divisions (a week, module or part), for one folder of material, or the whole semester, optionally focused on given topics. A division's exam draws on the same sources as its guide: its folder, if it has one, and its distilled lectures — a division with neither is refused. Same rules as trigger_synthesis: subscription job, visible in the Job Center, report it as queued. The exam lands in Study Guides/Practice/ and the workspace's practice list when it succeeds.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "scope": { "type": "string", "description": "A division as the overview names it (e.g. 'Week 3'), a folder name (e.g. 'Module 1'), or 'master' for semester-wide." },
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
        },
        {
            "name": "approve_move",
            "description": "Approve one waiting file-move proposal by its id, which moves the file (get_overview lists each waiting proposal with its id). Only when Daniel has asked for this one — an approval is his to give, and a proposal this conversation itself made is no exception. The move is audit-logged and a notice with Undo follows it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The proposal's id." },
                    "destination": { "type": "string", "description": "Optional class-relative path to file it at instead of the proposed one, including the file name." }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "dismiss_move",
            "description": "Decline one waiting file-move proposal by its id: the file stays where it is and the card leaves the queue. Nothing moves and nothing is written, so this cannot be undone — say so if Daniel might want it back.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id": { "type": "integer", "description": "The proposal's id." }
                },
                "required": ["id"],
                "additionalProperties": false
            }
        },
        {
            "name": "approve_all_moves",
            "description": "Approve every waiting file-move proposal for one class, as the Inbox's 'Approve all' does: one batch, one notice, one Undo. A card whose move fails is left waiting and named. Only on a clear request covering all of them.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." }
                },
                "required": ["class"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_sort",
            "description": "Queue a sort job over one class's inbox: a read-only run on the Claude subscription that proposes a destination per file. It proposes; nothing moves without approval. Refused when the inbox is empty or a sort is already running for the class. Report it as queued.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." }
                },
                "required": ["class"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_syllabus_scan",
            "description": "Queue a syllabus scan for one class: a read-only subscription job that reads the deadlines, the course's own divisions and the grade weights out of the syllabus. Deadlines land as proposals to approve; the divisions and weights are recorded directly. Report it as queued.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "target": { "type": "string", "description": "Optional class-relative path of the file to read, e.g. 'Syllabus/CAI5731 Syllabus.pdf'. Omit to let the job read the whole class folder." }
                },
                "required": ["class"],
                "additionalProperties": false
            }
        },
        {
            "name": "approve_deadlines",
            "description": "Approve waiting deadline proposals, which records them as real deadlines: pass ids for specific ones, or a class to take every proposal it has waiting (as 'Add all' does). One batch, one notice, one Undo. Only when Daniel has asked — a proposal is a reading of a syllabus or a notice, and approving it is his call.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "ids": { "type": "array", "items": { "type": "integer" }, "description": "The proposals to approve, as get_overview lists them." },
                    "class": { "type": "string", "description": "Approve every waiting proposal for this class instead. Ignored when ids are given." }
                },
                "additionalProperties": false
            }
        },
        {
            "name": "add_lecture",
            "description": "File a lecture into the class's Weeks folder: a caption track or recording at an absolute path, or a Zoom recording link. The transcript is normalized and indexed as source material. Takes a minute or two and, for a Zoom link, opens a window Daniel may have to sign in to. Set digest true only when he asks for the session document — it is a long subscription job. Leave week out on a course whose weeks carry dates, and the meeting date resolves it; on a course whose divisions name week ranges (Applied Generative AI) pass the week, or the file goes to the inbox for the sorter.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "source": { "type": "string", "description": "Absolute path to a .vtt/.srt/.txt caption or a media file, or a Zoom recording share link." },
                    "date": { "type": "string", "description": "The session's date, YYYY-MM-DD." },
                    "week": { "type": "integer", "description": "The course's own week number, when it needs saying." },
                    "title": { "type": "string", "description": "Optional title for the transcript file; defaults to 'Lecture'." },
                    "digest": { "type": "boolean", "description": "Distil the transcript into a session document afterwards. Defaults to false — it is a long, token-heavy subscription job." }
                },
                "required": ["class", "source", "date"],
                "additionalProperties": false
            }
        },
        {
            "name": "run_shift",
            "description": "Start the idle shift's run for tonight now, as the tray's 'Run the shift now' does: sync, file, capture, extract, distil, rebuild stale guides, write the exams and the small documents, all under tonight's caps. Refused when one is already running or tonight's has already run. Long and token-heavy on the subscription — only on a clear request.",
            "input_schema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "undo_last",
            "description": "Reverse the most recent action that can be reversed — the one the last notice's Undo would take, with every row of its batch. Use it when Daniel says to undo, take that back, or put it back. Say what was reversed.",
            "input_schema": { "type": "object", "properties": {}, "additionalProperties": false }
        },
        {
            "name": "list_hints",
            "description": "What the professor flagged, from the distilled lectures: emphasis, exam hints, corrections to the slides, where the room got stuck, things assigned, and what a session built on. Each item carries its session, its date and an HH:MM anchor into the transcript. Cite the transcript path and the anchor so it can be opened at that moment.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "class": { "type": "string", "description": "Class name." },
                    "since": { "type": "string", "description": "Optional YYYY-MM-DD; only items from sessions on or after this date." },
                    "kind": { "type": "string", "enum": ["emphasis", "exam_hint", "correction", "confusion", "action", "thread"], "description": "Optional single kind to list." }
                },
                "required": ["class"],
                "additionalProperties": false
            }
        }
    ])
}

/// Runs one tool call. Failures come back as tool results, not transport
/// errors — the model can correct a bad path or a thin query on its own.
pub fn execute(app: &AppHandle, name: &str, input: &Value, ctx: &ToolCtx) -> Outcome {
    let result = match name {
        "get_overview" => {
            with_conn(app, |conn| {
                let focus = match opt_str_arg(input, "class") {
                    Some(name) => Some(resolve_class(conn, &name)?.id),
                    None => None,
                };
                overview_text(conn, true, ctx.today_iso, focus)
            })
            .map(Outcome::ok)
        }
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
        "approve_move" => approve_move(app, input),
        "dismiss_move" => dismiss_move(app, input),
        "approve_all_moves" => approve_all_moves(app, input),
        "run_sort" => run_sort(app, input),
        "run_syllabus_scan" => run_syllabus_scan(app, input, ctx),
        "approve_deadlines" => approve_deadlines(app, input),
        "add_lecture" => add_lecture(app, input),
        "run_shift" => run_shift(app),
        "undo_last" => undo_last(app),
        "list_hints" => with_conn(app, |conn| list_hints(conn, input)),
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

/// Both confirm queues on one line, or nothing when nothing waits: a proposal
/// the overview never mentions is one chat cannot remind anyone about.
fn waiting_line(pending_moves: i64, pending_deadlines: i64) -> Option<String> {
    let mut waiting = Vec::new();
    if pending_moves > 0 {
        waiting.push(format!("{pending_moves} file move proposal(s)"));
    }
    if pending_deadlines > 0 {
        waiting.push(format!("{pending_deadlines} deadline proposal(s)"));
    }
    if waiting.is_empty() {
        return None;
    }
    Some(format!("{} awaiting Daniel's approval", waiting.join(" and ")))
}

/// The hub in text. `detailed` adds per-class inventories and guide dates; the
/// compact form is what rides in the system prompt. `today_iso` is what each
/// class's current division is resolved against (SPEC §8.5).
/// `focus` names one class to report in full. The whole hub in detail is one
/// tool result and `MAX_TOOL_RESULT_CHARS` cuts it — measured 2026-09-09, the
/// four classes ran to 37 KB against a 24 KB cap and the tail was lost — so
/// the unfocused detailed form leaves the flagged items to `list_hints`,
/// which reads them all with a date and a kind to filter by, and a focused
/// call carries them for the one class asked about.
pub fn overview_text(
    conn: &Connection,
    detailed: bool,
    today_iso: &str,
    focus: Option<i64>,
) -> Result<String> {
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
    // Counted after the vanish pass, so chat and the cards agree on what is
    // still waiting.
    let pending_moves = crate::sorter::pending_move_count(conn)?;
    let pending_deadlines: i64 = conn.query_row(
        "SELECT COUNT(*) FROM deadline_proposals WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    if let Some(line) = waiting_line(pending_moves, pending_deadlines) {
        out.push_str(&line);
        out.push('\n');
    }

    for class in classes {
        if focus.is_some_and(|id| id != class.id) {
            continue;
        }
        // The flagged items only where one class was asked for; otherwise the
        // count, and `list_hints` for the rest.
        let ledger = focus == Some(class.id);
        class_block(conn, &class, detailed, ledger, today_iso, &mut out)?;
    }

    // The compact form rides every turn, and this list was most of it —
    // measured 2026-09-09, 4,436 bytes of 7,987, twenty-five rows carrying
    // Canvas's descriptions. What is due inside a week is what a question
    // asked today is about; the rest are a count, and `get_overview` still
    // carries all twenty-five with their ids.
    let horizon = if detailed { None } else { Some(7) };
    let mut deadline_stmt = conn.prepare(&format!(
        "SELECT d.id, c.display_name, d.title, d.kind, d.due_at, d.notes
         FROM deadlines d JOIN classes c ON c.id = d.class_id
         WHERE d.status = 'open' ORDER BY {} LIMIT 25",
        crate::deadlines::DUE_INSTANT_SQL
    ))?;
    let rows = deadline_stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let class: String = row.get(1)?;
            let title: String = row.get(2)?;
            let kind: String = row.get(3)?;
            let due: String = row.get(4)?;
            let notes: Option<String> = row.get(5)?;
            // The [#id] is what upsert/complete/delete_deadline address.
            Ok((
                due.clone(),
                format!(
                    "- [#{id}] {due} · {class} · {title} ({kind}){}",
                    notes.map(|n| format!(" — {n}")).unwrap_or_default()
                ),
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (deadlines, rest) = within_horizon(&rows, today_iso, horizon);
    out.push_str("\n## Open deadlines\n");
    if deadlines.is_empty() && rest == 0 {
        out.push_str("None recorded.\n");
    } else {
        if !deadlines.is_empty() {
            out.push_str(&format!("{}\n", deadlines.join("\n")));
        }
        if rest > 0 {
            out.push_str(&format!(
                "{} more open after {}{} — get_overview lists them all with their ids.\n",
                rest,
                horizon.map(|d| format!("the next {d} days")).unwrap_or_else(|| "these".into()),
                if deadlines.is_empty() { ", the soonest still ahead" } else { "" }
            ));
        }
    }

    Ok(out)
}

/// The rows due within `days` of today, and how many were left out. `None`
/// keeps them all — the detailed form's answer. A row with no valid date
/// counts as inside, since dropping it would hide it entirely.
fn within_horizon<'a>(
    rows: &'a [(String, String)],
    today_iso: &str,
    days: Option<i64>,
) -> (Vec<&'a str>, usize) {
    let Some(days) = days else {
        return (rows.iter().map(|(_, line)| line.as_str()).collect(), 0);
    };
    let cutoff = chrono::NaiveDate::parse_from_str(today_iso, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.checked_add_days(chrono::Days::new(days as u64)))
        .map(|d| d.format("%Y-%m-%d").to_string());
    let mut kept = Vec::new();
    let mut rest = 0usize;
    for (due, line) in rows {
        // A date-only value compares as its own day; a timed one by its date
        // part, so a row due at 11:59 pm on the last day is still inside.
        match cutoff.as_deref() {
            Some(cutoff) if due.get(..10).is_some_and(|d| d > cutoff) => rest += 1,
            _ => kept.push(line.as_str()),
        }
    }
    (kept, rest)
}

/// One class of the overview: how it meets, how the course divides itself and
/// where it is today, which lectures are filed and distilled, what material
/// and guides exist, and what waits for approval. The compact form is one
/// line per topic, since it rides every turn as system context; `detailed`
/// is where the lists go — every division with its date, every lecture with
/// its note and session document, every pending proposal with its id.
fn class_block(
    conn: &Connection,
    class: &ClassRow,
    detailed: bool,
    ledger: bool,
    today_iso: &str,
    out: &mut String,
) -> Result<()> {
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

    // The course's own divisions (SPEC §5), and where it is today from its
    // own schedule; a course that published no dates gets no `Now:` rather
    // than a computed week. Every name below is text a model read out of a
    // syllabus PDF, placed in the system prompt on purpose: it is the same
    // trust tier as the guide and folder lines, capped by
    // `units::MAX_UNIT_NAME`, and a chat that does not know which week it is
    // would be the worse trade.
    let units = crate::units::list_units(conn, class.id)?;
    let position = crate::units::current_position(conn, class.id, today_iso)?;
    let current = position.as_ref().map(|p| p.unit.clone());
    let contributions = crate::lectures::list_contributions(conn, class.id)?;
    let guides = crate::guides::list_guides(conn, class.id)?;
    out.push_str(&divisions_line(&units));
    if let Some(position) = &position {
        // An undated course's week is read off its latest filed lecture
        // (SPEC §8.5), and the line says so rather than passing it off as a
        // published date.
        match position.week {
            Some(week) => out.push_str(&format!(
                "Now: {} · week {week}, read from the latest filed lecture\n",
                position.unit.name
            )),
            None => out.push_str(&format!("Now: {}\n", position.unit.name)),
        }
    }
    if detailed {
        out.push_str(&division_rows(&units, current.as_ref(), &contributions, &guides));
    }
    out.push_str(&notices_block(conn, class.id, detailed)?);
    if let (Some(start), Some(end)) = (&exam_start, &exam_end) {
        out.push_str(&format!("Final exam: {start} to {end}\n"));
    }
    out.push_str(&lectures_block(conn, class.id, &contributions, &guides, current.as_ref(), detailed)?);
    out.push_str(&material_line(conn, class.id)?);
    out.push_str(&guides_line(&guides, detailed));
    // The queue before the ledger: these are the ids the approval tools act
    // on, and if the result is ever cut they are the last thing that should
    // go.
    out.push_str(&waiting_block(conn, class.id, detailed)?);
    out.push_str(&flagged_block(conn, class.id, ledger)?);
    if detailed {
        out.push_str(&grades_line(conn, class.id)?);
    }
    Ok(())
}

/// How many of the professor's announcements the overview carries per class,
/// and how much of each body the detailed form quotes.
const NOTICES_SHOWN: usize = 3;
const NOTICE_BODY_CHARS: usize = 400;

/// `Notices:` — the professor's latest announcements (SPEC §7.2), newest
/// first and three at most: titles alone in the compact form, since it rides
/// every turn; the detailed form quotes each body, capped. Nothing when the
/// course has none.
fn notices_block(conn: &Connection, class_id: i64, detailed: bool) -> Result<String> {
    let (latest, total) =
        crate::canvas_sync::latest_announcements(conn, class_id, NOTICES_SHOWN)?;
    if latest.is_empty() {
        return Ok(String::new());
    }
    // The date half of a stored `YYYY-MM-DDTHH:MM`, taken by characters —
    // another build shares the table, so the column's shape is not a promise.
    let day = |posted_at: &str| posted_at.chars().take(10).collect::<String>();
    // Title and body are the professor's text, folded onto one line each so
    // neither can open a line of its own in the system prompt.
    let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    if !detailed {
        let titles = latest
            .iter()
            .map(|a| format!("\"{}\" ({})", one_line(&a.title), day(&a.posted_at)))
            .collect::<Vec<_>>()
            .join("; ");
        return Ok(format!("Notices: {titles}\n"));
    }
    let mut out = format!(
        "Notices (latest {} of {}):\n",
        latest.len(),
        plural(total, "announcement")
    );
    for a in &latest {
        out.push_str(&format!(
            "- {} · {}\n  {}\n",
            day(&a.posted_at),
            one_line(&a.title),
            truncate(&one_line(&a.body), NOTICE_BODY_CHARS)
        ));
    }
    Ok(out)
}

/// The detailed form's row per division: its date, `NOW`, its folder, its
/// lecture counts and its guide's freshness.
fn division_rows(
    units: &[crate::units::UnitInfo],
    current: Option<&crate::units::UnitInfo>,
    contributions: &[crate::lectures::Contribution],
    guides: &[crate::guides::GuideInfo],
) -> String {
    let mut out = String::new();
    for unit in units {
        let mapped = contributions.iter().filter(|c| c.unit_id == unit.id);
        let filed = mapped.clone().count();
        let distilled = mapped.filter(|c| c.distilled).count();
        let mut line = format!("- {}. {}", unit.ordinal, unit.name);
        if let Some(starts) = &unit.starts_on {
            line.push_str(&format!(" · from {starts}"));
        }
        if current.is_some_and(|c| c.id == unit.id) {
            line.push_str(" · NOW");
        }
        if let Some(folder) = &unit.rel_path {
            line.push_str(&format!(" · folder {folder}"));
        }
        if filed > 0 {
            line.push_str(&format!(" · {} ({distilled} distilled)", plural(filed, "lecture")));
        }
        let scope = crate::db::unit_scope(unit.id);
        if let Some(guide) = guides.iter().find(|g| g.scope == scope) {
            line.push_str(&format!(" · guide {}", freshness(guide.stale)));
        }
        line.push('\n');
        out.push_str(&line);
    }
    out
}

/// `Lectures:` — what is filed under `Weeks/` (SPEC §4), which of it the
/// calendar mapped to a division and distilled into a corpus note (§8.5), and
/// which session documents exist (§8.4). The compact form counts, plus the
/// current division's share — naming every division with a lecture would grow
/// the line that rides every turn by the semester; the detailed form lists
/// each lecture with its note and its session document's markdown twin.
fn lectures_block(
    conn: &Connection,
    class_id: i64,
    contributions: &[crate::lectures::Contribution],
    guides: &[crate::guides::GuideInfo],
    current: Option<&crate::units::UnitInfo>,
    detailed: bool,
) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT rel_path FROM files WHERE class_id = ?1 AND rel_path LIKE 'Weeks/%.md'
         ORDER BY rel_path",
    )?;
    let filed = stmt
        .query_map([class_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let sessions = guides.iter().filter(|g| g.family == "session").count();
    let mapped = |rel_path: &str| contributions.iter().any(|c| c.rel_path == rel_path);
    if filed.is_empty() && contributions.is_empty() && sessions == 0 {
        return Ok("Lectures: none filed\n".to_string());
    }

    let mut out = String::new();
    if detailed {
        out.push_str("Lectures:\n");
        for c in contributions {
            let mut line = format!("- {} · feeds {}", c.rel_path, c.unit_name);
            line.push_str(&if c.distilled {
                format!(" · distilled to {}", c.corpus_rel_path)
            } else {
                " · not distilled yet".to_string()
            });
            let session_scope = format!("{}{}", crate::db::SESSION_SCOPE_PREFIX, c.rel_path);
            if let Some(session) = guides.iter().find(|g| g.scope == session_scope) {
                line.push_str(&format!(
                    " · session document {} ({})",
                    markdown_twin(&session.rel_path),
                    freshness(session.stale)
                ));
            }
            line.push('\n');
            out.push_str(&line);
        }
        for rel_path in filed.iter().filter(|p| !mapped(p)) {
            out.push_str(&format!("- {rel_path} · mapped to no division\n"));
        }
        return Ok(out);
    }

    let distilled = contributions.iter().filter(|c| c.distilled).count();
    out.push_str(&format!(
        "Lectures: {} filed, {distilled} distilled, {}",
        filed.len().max(contributions.len()),
        plural(sessions, "session document")
    ));
    if let Some(unit) = current {
        let here: Vec<_> = contributions.iter().filter(|c| c.unit_id == unit.id).collect();
        if !here.is_empty() {
            out.push_str(&format!(
                " · {} in the current division ({} distilled)",
                here.len(),
                here.iter().filter(|c| c.distilled).count()
            ));
        }
    }
    let unmapped = filed.iter().filter(|p| !mapped(p)).count();
    if unmapped > 0 {
        out.push_str(&format!(" · {unmapped} mapped to no division"));
    }
    out.push('\n');
    Ok(out)
}

/// `Material:` — how much is indexed and extracted, and the depth-0 folders
/// holding it, named as folders: a folder is where material sits, not one of
/// the course's divisions (SPEC §7.2).
fn material_line(conn: &Connection, class_id: i64) -> Result<String> {
    let (indexed, extracted): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN extracted_sha256 IS NOT NULL
                          AND extracted_sha256 = sha256 THEN 1 ELSE 0 END)
         FROM files WHERE class_id = ?1",
        [class_id],
        |row| Ok((row.get(0)?, row.get::<_, Option<i64>>(1)?.unwrap_or(0))),
    )?;
    let folders = folder_counts(conn, class_id)?
        .iter()
        .map(|(name, count)| format!("{name} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "Material: {indexed} files indexed, {extracted} with a current extract{}\n",
        if folders.is_empty() {
            " · no folders yet".to_string()
        } else {
            format!(" · folders: {folders}")
        }
    ))
}

/// `Guides:` — every scope with its staleness; the detailed form adds the
/// path and the age. A session document is named for what the session was
/// about, which its scope (the transcript's path) is not.
fn guides_line(guides: &[crate::guides::GuideInfo], detailed: bool) -> String {
    // An exam's row is not a guide (SPEC §8.3); the exams are listed nowhere here.
    let guides: Vec<&crate::guides::GuideInfo> = guides.iter().filter(|g| g.family != "practice").collect();
    if guides.is_empty() {
        return "Guides: none generated yet\n".to_string();
    }
    let now_ts = now();
    let described = guides
        .iter()
        .map(|g| {
            let scope = if g.family == "session" {
                format!("session {}", document_stem(&g.rel_path))
            } else {
                g.label.clone()
            };
            if detailed {
                format!(
                    "{scope} — {} ({}, {})",
                    freshness(g.stale),
                    g.rel_path,
                    days_ago(now_ts, g.generated_at)
                )
            } else {
                format!("{scope} ({})", freshness(g.stale))
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("Guides: {described}\n")
}

/// How many flagged items the detailed form lists per class, and how much of
/// each. The overview is one tool result and `MAX_TOOL_RESULT_CHARS` cuts it:
/// measured 2026-09-09 on the real hub, twenty full items a class ran the
/// detailed form to 37 KB against a 24 KB cap, so Biostatistics' waiting
/// queue and the whole of Applied Generative AI's block never reached the
/// model — and `approve_move` reads its id from exactly there.
const FLAGGED_SHOWN: usize = 20;
const FLAGGED_CHARS: usize = 200;

/// `Flagged:` — what the professor flagged across the class's distilled
/// sessions (SPEC §8.4): a count and the earliest session's date in the
/// compact form, the items newest first and capped in the detailed one.
/// Nothing when no session has been read for them.
fn flagged_block(conn: &Connection, class_id: i64, detailed: bool) -> Result<String> {
    let hints = crate::lectures::list_hints(conn, class_id)?;
    if hints.is_empty() {
        return Ok(String::new());
    }
    let since = hints
        .iter()
        .map(|h| h.date.as_str())
        .filter(|d| !d.is_empty())
        .min()
        .unwrap_or("the first distilled session");
    if !detailed {
        return Ok(format!("Flagged: {} since {since}\n", plural(hints.len(), "item")));
    }
    // The detailed form names the newest few and points at the tool, rather
    // than carrying twenty items a class. `list_hints` (SPEC §9) reaches the
    // whole ledger with a date and a kind to filter by, so the same text here
    // was a second copy — and one that pushed the proposal ids past the
    // tool-result cap, which is where the write tools read them.
    let mut out = format!("Flagged ({} since {since}, newest first):\n", plural(hints.len(), "item"));
    for h in hints.iter().take(FLAGGED_SHOWN) {
        let anchor = h.anchor.as_deref().map_or(String::new(), |a| format!(" {a}"));
        out.push_str(&format!(
            "- {}{anchor} · {} · {}\n",
            h.date,
            crate::lectures::hint_kind_label(&h.kind).to_lowercase(),
            truncate(&h.text.split_whitespace().collect::<Vec<_>>().join(" "), FLAGGED_CHARS)
        ));
    }
    if hints.len() > FLAGGED_SHOWN {
        out.push_str(&format!(
            "- … {} more · list_hints reads them all, by date or by kind\n",
            hints.len() - FLAGGED_SHOWN
        ));
    }
    Ok(out)
}

/// `Waiting:` — both confirm queues (SPEC §10, §11), so the model can say
/// what waits and where; nothing when nothing does. Ids in the detailed form
/// only: chat has no tool that acts on a proposal, so an id is something to
/// name, not something to press. The move rows are read directly rather than
/// through `sort_state`: the vanish pass already ran for every class at the
/// top of the overview, and the inbox listing it would also produce is not
/// shown here.
fn waiting_block(conn: &Connection, class_id: i64, detailed: bool) -> Result<String> {
    let deadlines = crate::deadlines::pending_proposals(conn, class_id)?;
    let mut stmt = conn.prepare(
        "SELECT id, source_rel_path, dest_rel_path, source FROM move_proposals
         WHERE class_id = ?1 AND status = 'pending' ORDER BY id",
    )?;
    let moves = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if deadlines.is_empty() && moves.is_empty() {
        return Ok(String::new());
    }
    if !detailed {
        let mut waiting = Vec::new();
        if !moves.is_empty() {
            waiting.push(plural(moves.len(), "file move proposal"));
        }
        if !deadlines.is_empty() {
            waiting.push(plural(deadlines.len(), "deadline proposal"));
        }
        return Ok(format!("Waiting: {}\n", waiting.join(", ")));
    }
    let mut out = String::from("Waiting for approval:\n");
    for p in &deadlines {
        out.push_str(&format!(
            "- deadline proposal #{} · {} ({}) due {} · from {}\n",
            p.id,
            p.title,
            p.kind,
            p.due_at,
            proposer(&p.source)
        ));
    }
    for (id, from, to, source) in &moves {
        out.push_str(&format!(
            "- move proposal #{id} · {from} → {to} · from {}\n",
            proposer(source)
        ));
    }
    Ok(out)
}

/// `Divisions: 14 weeks from the syllabus` — the course's own word where every
/// row agrees on one, the neutral noun where it declares two levels. Mirrors
/// `provenance` in src/components/Structure.tsx.
fn divisions_line(units: &[crate::units::UnitInfo]) -> String {
    if units.is_empty() {
        return "Divisions: none declared\n".to_string();
    }
    let kinds: std::collections::BTreeSet<&str> = units.iter().map(|u| u.kind.as_str()).collect();
    let noun = if kinds.len() == 1 {
        kinds.iter().next().copied().unwrap_or("division")
    } else {
        "division"
    };
    let sources: std::collections::BTreeSet<&str> =
        units.iter().map(|u| u.source.as_str()).collect();
    let from = if sources.len() > 1 {
        "Canvas and the syllabus"
    } else if sources.contains("canvas") {
        "Canvas"
    } else {
        "the syllabus"
    };
    format!("Divisions: {} from {from}\n", plural(units.len(), noun))
}

fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

fn freshness(stale: bool) -> &'static str {
    if stale {
        "STALE"
    } else {
        "fresh"
    }
}

/// Who proposed it, in the words the cards use.
fn proposer(source: &str) -> &'static str {
    match source {
        "canvas" => "Canvas",
        "syllabus" => "the syllabus scan",
        "sort_job" => "a sort job",
        "chat" => "chat",
        "by_name" => "its file name",
        _ => "an unknown reader",
    }
}

/// The session document's markdown twin (SPEC §8.4) — the copy chat can read
/// and search, so it is the one the overview names.
fn markdown_twin(html_rel_path: &str) -> String {
    format!("{}.md", html_rel_path.strip_suffix(".html").unwrap_or(html_rel_path))
}

/// `Study Guides/Sessions/2026-09-01 — Topic.html` → `2026-09-01 — Topic`.
fn document_stem(rel_path: &str) -> String {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    name.strip_suffix(".html").unwrap_or(name).to_string()
}

/// Depth-0 folders holding indexed files, with their file counts.
fn folder_counts(conn: &Connection, class_id: i64) -> Result<BTreeMap<String, usize>> {
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
        "SELECT rel_path, kind, size, extracted_sha256 = sha256, duplicate_of FROM files
         WHERE class_id = ?1 ORDER BY rel_path",
    )?;
    let rows = stmt
        .query_map([class.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<bool>>(3)?.unwrap_or(false),
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut lines = Vec::new();
    let mut shown = 0usize;
    let mut skipped = 0usize;
    let mut matched = 0usize;
    for (rel_path, kind, size, extracted, duplicate_of) in &rows {
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
            "{}/{rel_path} · {kind} · {} · {}{}",
            class.folder_name,
            format_size(*size),
            if *extracted { "extract ✓" } else { "no extract yet" },
            duplicate_of
                .as_ref()
                .map(|canonical| format!(" · duplicate of {canonical}, read once through it"))
                .unwrap_or_default()
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
/// The ranked half: reconcile, query, format. `None` means the query held no
/// words FTS could match on — a regular expression — and the caller should
/// take the ripgrep path instead.
fn ranked(
    app: &AppHandle,
    classes: &[ClassRow],
    query: &str,
    dropped: &std::collections::HashSet<String>,
) -> Result<Option<Outcome>> {
    // Read as a pattern before any word is pulled out of it: `^\d{4}` holds
    // the letter `d`, and searching for that would answer confidently and
    // wrongly rather than taking the path the model meant.
    if crate::search::looks_like_regex(query) || crate::search::fts_query(query).is_none() {
        return Ok(None);
    }
    let ids: Vec<i64> = classes.iter().map(|c| c.id).collect();
    // One lock per class rather than one across all four: the reconcile reads
    // whatever moved off disk, and a first build reads every document — no
    // reason to hold the connection through the next class's reads too.
    for class in classes {
        let synced = with_conn(app, |conn| {
            let dir = crate::scanner::class_dir(conn, class.id)?;
            crate::search::sync_class(conn, class.id, &dir)
        });
        if let Err(e) = synced {
            eprintln!(
                "search index: {} could not be reconciled: {e:#}",
                class.display_name
            );
        }
    }
    let (hits, total) = with_conn(app, |conn| crate::search::run(conn, &ids, query, dropped))?;

    let scope = if classes.len() == 1 {
        format!(" in {}", classes[0].display_name)
    } else {
        String::new()
    };
    if hits.is_empty() {
        return Ok(Some(Outcome::ok(format!(
            "No documents match '{query}'{scope}.\n\nTry a shorter phrase, a synonym, or a \
             single distinctive term. For a shape rather than words — a date, a character \
             class — call this tool again with regex: true.\n"
        ))));
    }
    let mut text = format!(
        "{total} document(s) match '{query}'{scope}, best first{}\n\n",
        if total > hits.len() {
            format!(" ({} shown)", hits.len())
        } else {
            String::new()
        }
    );
    for hit in &hits {
        let line = hit.line.map(|n| format!(":{n}")).unwrap_or_default();
        text.push_str(&format!(
            "{}{line} [{}]\n    {}\n",
            hit.rel_path,
            hit.kind,
            truncate(&hit.snippet, MAX_MATCH_CHARS)
        ));
    }
    text.push_str(
        "\nEach line is one document, ranked by relevance, with the stretch that matched. \
         Read the ones worth reading with read_material at the line given.\n",
    );
    Ok(Some(Outcome::ok(text)))
}

/// SPEC §9 — ranked search first, ripgrep behind it.
///
/// The index is reconciled against the disk before the query rather than
/// written through by the pipeline (`search.rs`), so what comes back is what
/// the tree holds now; an unchanged tree costs a stat of a few hundred files.
/// A reconcile that fails is logged and the query runs on what the index
/// already holds — a stale answer beats none, and the index is derived.
fn search_material(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let query = str_arg(input, "query")?;
    let as_regex = input["regex"].as_bool().unwrap_or(false);
    let (root, classes, duplicate_extracts) = with_conn(app, |conn| {
        let root = crate::db::aibhs_root(conn)?;
        let classes = match opt_str_arg(input, "class") {
            Some(name) => vec![resolve_class(conn, &name)?],
            None => class_rows(conn)?,
        };
        // A duplicate answers for a reading its canonical copy already
        // answers for (SPEC §7 step 1): a hit in its extract, or in the file
        // itself where the source is searchable text, is dropped.
        let mut duplicate_extracts = std::collections::HashSet::new();
        for class in &classes {
            let mut stmt = conn.prepare(
                "SELECT rel_path, extract_rel_path FROM files
                 WHERE class_id = ?1 AND duplicate_of IS NOT NULL",
            )?;
            let rows = stmt.query_map([class.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                let (rel_path, extract) = row?;
                duplicate_extracts.insert(format!("{}/{}", class.folder_name, rel_path));
                if let Some(extract) = extract {
                    duplicate_extracts.insert(format!("{}/{}", class.folder_name, extract));
                }
            }
        }
        Ok((root, classes, duplicate_extracts))
    })?;

    if !as_regex {
        if let Some(outcome) = ranked(app, &classes, &query, &duplicate_extracts)? {
            return Ok(outcome);
        }
    }

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
        let rel = Path::new(path)
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        if duplicate_extracts.contains(&rel) {
            continue;
        }
        hits += 1;
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
        "{hits} matching line(s) in {} file(s) for the pattern /{query}/{scope}\n\n",
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
        // A docx's conversion twin sits in the mirror beside its extract
        // (SPEC §4): the same prose through LibreOffice markup, plus every
        // figure as a base64 line.
        "--glob",
        "!*.docx.html",
    ]);
    if fixed {
        cmd.arg("--fixed-strings");
    }
    cmd.arg("--regexp").arg(query);
    for dir in dirs {
        cmd.arg(dir);
    }
    // Bounded like every other subprocess the app spawns (LibreOffice, the
    // idle read, Parakeet): a pattern that backtracks catastrophically over
    // two megabytes of markdown would otherwise park the chat thread with no
    // way back, and the chat loop has no watchdog of its own.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let child = cmd
        .spawn()
        .context("running ripgrep (install it with `brew install ripgrep`)")?;
    crate::jobs::wait_bounded(child, RG_TIMEOUT).with_context(|| {
        format!(
            "the pattern search did not finish within {} seconds — narrow the pattern, or \
             search for words instead",
            RG_TIMEOUT.as_secs()
        )
    })
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
    // The conversion twin is an intermediate, not a copy worth reading: the
    // extract beside it is the same document as prose.
    if let Some(stem) = rel_path.strip_suffix(".docx.html") {
        if rel_path.contains(&format!("/{EXTRACTS_DIR}/")) {
            bail!("that is the docx's conversion twin — read the extract instead: {stem}.docx.md");
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
    let amending = input.get("id").and_then(Value::as_i64);
    let (outcome, audit_id, title, class_id) = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let written = match amending {
            Some(id) => amend_deadline(conn, &class, id, input)?,
            None => create_deadline(conn, &class, input)?,
        };
        Ok((written.0, written.1, written.2, class.id))
    })?;
    emit_hub_change(app, "deadlines");
    let verb = if amending.is_none() { "Added" } else { "Changed" };
    notify(app, format!("{verb} {title}"), vec![audit_id], Some(class_id));
    Ok(outcome)
}

/// A chat deadline write: the tool's answer, the audit row it wrote and the
/// title, for the notice.
type Written = (Outcome, i64, String);

fn create_deadline(conn: &Connection, class: &ClassRow, input: &Value) -> Result<Written> {
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
    let audit_id = audit(
        conn,
        "chat.upsert_deadline",
        json!({ "id": id, "classId": class.id, "title": title, "kind": kind,
                "dueAt": due_at, "notes": notes, "created": true }),
    )?;
    let outcome = Outcome::ok(format!(
        "Deadline recorded — {title} ({kind}) due {due_at} · {} [#{id}]\n\
         Amend with upsert_deadline(id: {id}); close with complete_deadline when it's done.",
        class.display_name
    ));
    Ok((outcome, audit_id, title))
}

fn amend_deadline(conn: &Connection, class: &ClassRow, id: i64, input: &Value) -> Result<Written> {
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
    let audit_id = audit(
        conn,
        "chat.upsert_deadline",
        json!({ "id": id, "classId": class.id,
                "before": { "title": old_title, "kind": old_kind, "dueAt": old_due, "notes": old_notes },
                "after": { "title": title, "kind": kind, "dueAt": due_at, "notes": notes } }),
    )?;
    let outcome = Outcome::ok(format!(
        "Deadline amended — {title} ({kind}) due {due_at} · {} [#{id}]{}",
        class.display_name,
        if status == "done" { "\n(It is marked done.)" } else { "" }
    ));
    Ok((outcome, audit_id, title))
}

fn complete_deadline(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let (outcome, written) = with_conn(app, |conn| {
        let id = int_arg(input, "id")?;
        let (title, class_name, status) = deadline_brief(conn, id)?;
        if status == "done" {
            return Ok((
                Outcome::ok(format!(
                    "Deadline was already done — {title} · {class_name} [#{id}]"
                )),
                None,
            ));
        }
        let class_id: i64 =
            conn.query_row("SELECT class_id FROM deadlines WHERE id = ?1", [id], |r| r.get(0))?;
        let tx = conn.unchecked_transaction()?;
        tx.execute("UPDATE deadlines SET status = 'done' WHERE id = ?1", [id])?;
        let audit_id = audit(
            &tx,
            "chat.complete_deadline",
            json!({ "id": id, "classId": class_id, "status": "done", "before": "open" }),
        )?;
        tx.commit()?;
        Ok((
            Outcome::ok(format!("Deadline done — {title} · {class_name} [#{id}]")),
            Some((audit_id, title, class_id)),
        ))
    })?;
    emit_hub_change(app, "deadlines");
    if let Some((audit_id, title, class_id)) = written {
        notify(app, format!("Marked {title} done"), vec![audit_id], Some(class_id));
    }
    Ok(outcome)
}

fn delete_deadline(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let (outcome, audit_id, title, class_id, forgotten) = with_conn(app, |conn| {
        let id = int_arg(input, "id")?;
        let row = crate::deadlines::deadline_row(conn, id)?
            .with_context(|| format!("no deadline #{id} — get_overview lists the ids"))?;
        let title = row["title"].as_str().unwrap_or_default().to_string();
        let due_at = row["dueAt"].as_str().unwrap_or_default().to_string();
        let class_id = row["classId"].as_i64().unwrap_or_default();
        // One transaction, because the promise below — that the row survives
        // in the audit log — is only true if both statements land together.
        let tx = conn.unchecked_transaction()?;
        // Its brief goes with it (SPEC §8.6): the row here, the files after.
        let forgotten = crate::briefs::forget(&tx, class_id, id)?;
        tx.execute("DELETE FROM deadlines WHERE id = ?1", [id])?;
        // A tracked assignment's card is declined, or the next sync would
        // write the deadline back (SPEC §7.2).
        crate::deadlines::decline_assignment(&tx, &row)?;
        // The full row rides the audit entry, so a deletion is recoverable.
        let audit_id = audit(&tx, "chat.delete_deadline", row)?;
        tx.commit()?;
        Ok((
            Outcome::ok(format!(
                "Deadline deleted — {title} (was due {due_at}) [#{id}]\nThe full row is kept in the audit log."
            )),
            audit_id,
            title,
            class_id,
            forgotten,
        ))
    })?;
    crate::deadlines::remove_forgotten_files(app, class_id, &forgotten);
    emit_hub_change(app, "deadlines");
    notify(app, format!("Deleted {title}"), vec![audit_id], Some(class_id));
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
    let (outcome, audit_id, name, class_id, created) = with_conn(app, |conn| {
        let class = resolve_class(conn, &str_arg(input, "class")?)?;
        let name = str_arg(input, "name")?;
        let weight = float_arg(input, "weight")?;
        if !(0.0..=100.0).contains(&weight) {
            bail!("weight is a percentage between 0 and 100");
        }
        let existing: Option<(i64, String, f64)> = conn
            .query_row(
                "SELECT id, name, weight FROM grade_categories
                 WHERE class_id = ?1 AND LOWER(name) = LOWER(?2)",
                params![class.id, name],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        // The same row shape the Grades section's save writes, so one undo
        // reads both: the row on a create, `before`/`after` on an edit.
        let (verb, audit_id, created) = match existing {
            Some((id, old_name, old_weight)) => {
                conn.execute(
                    "UPDATE grade_categories SET name = ?1, weight = ?2 WHERE id = ?3",
                    params![name, weight, id],
                )?;
                let audit_id = audit(
                    conn,
                    "chat.upsert_grade_category",
                    json!({ "id": id, "classId": class.id,
                            "before": { "name": old_name, "weight": old_weight },
                            "after": { "name": name, "weight": weight } }),
                )?;
                ("updated", audit_id, false)
            }
            None => {
                conn.execute(
                    "INSERT INTO grade_categories (class_id, name, weight) VALUES (?1, ?2, ?3)",
                    params![class.id, name, weight],
                )?;
                let audit_id = audit(
                    conn,
                    "chat.upsert_grade_category",
                    json!({ "id": conn.last_insert_rowid(), "classId": class.id,
                            "name": name, "weight": weight, "created": true }),
                )?;
                ("created", audit_id, true)
            }
        };
        let outcome = Outcome::ok(format!(
            "Grade category {verb} — {name} at {}% · {}\n{}",
            trim_num(weight),
            class.display_name,
            weights_line(conn, class.id)?
        ));
        Ok((outcome, audit_id, name, class.id, created))
    })?;
    emit_hub_change(app, "grades");
    let verb = if created { "Added" } else { "Changed" };
    notify(app, format!("{verb} {name}"), vec![audit_id], Some(class_id));
    Ok(outcome)
}

fn add_grade_item(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let (outcome, audit_id, name, class_id) = with_conn(app, |conn| {
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
        let audit_id = audit(
            conn,
            "chat.add_grade_item",
            json!({ "id": conn.last_insert_rowid(), "categoryId": category_id,
                    "classId": class.id, "name": name, "score": score,
                    "maxScore": max_score, "gradedAt": graded_at, "created": true }),
        )?;
        let grade = weighted_grade(conn, class.id)?
            .map(|pct| format!("\nCurrent weighted grade over graded items: {pct:.1}%."))
            .unwrap_or_default();
        let outcome = Outcome::ok(format!(
            "Grade recorded — {name}: {}/{} in {category_name} · {}{grade}",
            trim_num(score),
            trim_num(max_score),
            class.display_name
        ));
        Ok((outcome, audit_id, name, class.id))
    })?;
    emit_hub_change(app, "grades");
    notify(app, format!("Recorded {name}"), vec![audit_id], Some(class_id));
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
        let outcome = Outcome::ok(format!(
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
        ));
        Ok((outcome, written, class.id))
    })?;
    emit_hub_change(app, "notes");
    let (outcome, written, class_id) = outcome;
    let verb = if written.created { "Wrote" } else { "Updated" };
    notify(
        app,
        format!("{verb} {}", crate::notes::note_title(&written.rel_path)),
        vec![written.audit_id],
        Some(class_id),
    );
    Ok(outcome)
}

/// What a synthesis or practice trigger can be pointed at (SPEC §8.1–§8.3).
#[derive(Debug, Clone, PartialEq)]
enum Scope {
    /// The semester master, or the whole class for a practice exam.
    Master,
    /// A depth-0 folder holding indexed files, by its rel path.
    Folder(String),
    /// One of the course's own divisions, by its `units` row. `kind` and
    /// `number` — the course's own, falling back to the list position for a
    /// row named without one — are what the row answers to as `week3`.
    Unit { id: i64, name: String, kind: String, number: i64 },
}

impl Scope {
    /// The `jobs.scope` / `guides.scope` key this resolves to.
    fn key(&self) -> String {
        match self {
            Scope::Master => crate::db::MASTER_SCOPE.to_string(),
            Scope::Folder(rel) => rel.clone(),
            Scope::Unit { id, .. } => crate::db::unit_scope(*id),
        }
    }
}

/// `master` (and natural synonyms) selects the semester master; anything else
/// must match one of the course's own divisions or one of the class's folders
/// — the same tolerant matching as classes, so "week 3" finds `Week 3 — Data
/// Exploration…`, "module 1" or "m1" finds `Module 1`, and a topic finds the
/// division named for it.
///
/// A division answers to its whole name, to its kind and number (`week3`),
/// and to its storage key (`unit:24`, as a listing prints it), so "Week 1" is
/// an exact hit on Week 1 rather than a substring
/// of Weeks 10–15. A folder named exactly like a division is that division's
/// folder (`units::attach_folder_paths` joins them on that equality — exact,
/// case and all, so the comparison here is the same one), so it is dropped
/// from the candidates and the division wins: its sources include the
/// folder's files and its lectures both. A folder that only nearly matches
/// (`module 1` beside `Module 1`) was never attached, so it stays a scope of
/// its own and a query hitting both is ambiguous rather than guessed.
fn resolve_scope(conn: &Connection, class: &ClassRow, scope: &str) -> Result<Scope> {
    // A scope copied out of a guide listing carries the storage prefix; one
    // from before ids carried the name after it, and still resolves by it.
    let raw = scope
        .trim()
        .strip_prefix(crate::db::UNIT_SCOPE_PREFIX)
        .unwrap_or(scope.trim());
    let needle = squash(raw);
    if ["master", "semester", "semestermaster", "wholesemester", "all"]
        .contains(&needle.as_str())
    {
        return Ok(Scope::Master);
    }
    let units = crate::units::list_units(conn, class.id)?;
    let as_scope = |u: &crate::units::UnitInfo| Scope::Unit {
        id: u.id,
        name: u.name.clone(),
        kind: u.kind.clone(),
        number: u.number.unwrap_or(u.ordinal),
    };
    if let Some(id) = crate::db::unit_scope_id(scope.trim()) {
        return units
            .iter()
            .find(|u| u.id == id)
            .map(as_scope)
            .with_context(|| format!("{} has no division with id {id}", class.display_name));
    }
    let folders: Vec<String> = folder_counts(conn, class.id)?
        .into_keys()
        .filter(|f| f != "(class folder)")
        .filter(|f| !units.iter().any(|u| &u.name == f))
        .collect();
    let candidates: Vec<Scope> = units
        .iter()
        .map(as_scope)
        .chain(folders.iter().map(|f| Scope::Folder(f.clone())))
        .collect();
    let list = || {
        let mut parts = Vec::new();
        if !units.is_empty() {
            parts.push(format!(
                "divisions: {}",
                units.iter().map(|u| u.name.as_str()).collect::<Vec<_>>().join(", ")
            ));
        }
        if !folders.is_empty() {
            parts.push(format!("folders: {}", folders.join(", ")));
        }
        parts.push("or 'master' for the whole semester".to_string());
        parts.join("; ")
    };
    if candidates.is_empty() {
        bail!(
            "{} declares no divisions and has no folders yet — only 'master' is possible",
            class.display_name
        );
    }
    // Same guard as `resolve_class`, and for a sharper reason: an empty needle
    // is `contains`-true against every candidate, so a scope of "?" or "—"
    // squashes to nothing, matches everything, and — in a class with exactly
    // one candidate — resolves silently. That would enqueue a full synthesis
    // run against a scope the model never actually named.
    if needle.is_empty() {
        bail!("which scope? {}", list());
    }
    // Master never enters the candidates — it was answered above by name.
    let Some(tier) = best_match(&candidates, &needle, |c| match c {
        Scope::Unit { name, kind, number, .. } => vec![squash(name), format!("{kind}{number}")],
        Scope::Folder(rel) => vec![squash(rel)],
        Scope::Master => unreachable!("master is resolved before matching"),
    }) else {
        bail!("nothing in {} matches '{scope}' — {}", class.display_name, list());
    };
    let describe = |c: &Scope| match c {
        Scope::Unit { name, .. } => name.clone(),
        Scope::Folder(rel) => format!("folder {rel}"),
        Scope::Master => unreachable!("master is resolved before matching"),
    };
    match tier.as_slice() {
        [only] => Ok((*only).clone()),
        many => bail!(
            "'{scope}' matches {} scopes ({}) — be specific",
            many.len(),
            many.iter().map(|c| describe(c)).collect::<Vec<_>>().join(", ")
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
        Scope::Master => {
            let job_id = crate::guides::synthesize_master(app, class.id, ctx.today)?;
            Ok(Outcome::ok(format!(
                "Semester master synthesis queued — {} (job #{job_id})\n\
                 Exclusive job: it waits for running jobs to drain, then runs alone — often \
                 30+ minutes. Progress is live in the Job Center; the guide appears in the \
                 workspace when it succeeds.",
                class.display_name
            )))
        }
        Scope::Folder(module_rel) => {
            let job_id =
                crate::guides::synthesize_module(app, class.id, &module_rel, ctx.today)?;
            Ok(Outcome::ok(format!(
                "Guide synthesis queued — folder {module_rel} · {} (job #{job_id})\n\
                 Progress is live in the Job Center; the guide appears on the folder's row when \
                 it succeeds (typically 10–30 minutes).",
                class.display_name
            )))
        }
        Scope::Unit { id, name, .. } => {
            let job_id = crate::guides::synthesize_unit(app, class.id, id, ctx.today)?;
            Ok(Outcome::ok(format!(
                "Guide synthesis queued — {name} · {} (job #{job_id})\n\
                 Built from the division's folder, if it has one, and its distilled lectures. \
                 Progress is live in the Job Center; the guide appears on the division's row in \
                 the Structure list when it succeeds (typically 10–30 minutes).",
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
    let key = scope.key();
    let (job_id, output_rel) = crate::guides::generate_practice(
        app,
        class.id,
        &key,
        focus.as_deref(),
        ctx.today,
        ctx.today_iso,
        None,
    )?;
    Ok(Outcome::ok(format!(
        "Practice exam queued — {} · {} (job #{job_id})\n\
         It will land at {}/{output_rel} and show in the workspace's practice list when \
         the job succeeds (live in the Job Center now).{}",
        match &scope {
            Scope::Master => "semester scope".to_string(),
            Scope::Folder(rel) => format!("folder {rel}"),
            Scope::Unit { name, .. } => name.clone(),
        },
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

// ---------------------------------------------------------------------------
// Running the pipeline (SPEC §9)
//
// Each of these calls the same audited function its button does, so a move
// chat approved is the move the Inbox card makes, with the same audit row and
// the same notice with its `Undo`. Nothing here is a second path into the
// data: what is new is only that a sentence reaches it.

/// The proposal a chat approval is about, checked before the move so the
/// refusal names the queue rather than a rename.
fn pending_move(conn: &Connection, id: i64) -> Result<(i64, String, String)> {
    let row: Option<(i64, String, String, String)> = conn
        .query_row(
            "SELECT class_id, source_rel_path, dest_rel_path, status
             FROM move_proposals WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((class_id, source, dest, status)) = row else {
        bail!("no file-move proposal has id {id} — get_overview lists the waiting ones");
    };
    if status != "pending" {
        bail!("proposal {id} is already {status} — it is no longer waiting");
    }
    Ok((class_id, source, dest))
}

fn approve_move(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let id = int_arg(input, "id")?;
    let destination = opt_str_arg(input, "destination");
    let (_, source, proposed) = with_conn(app, |conn| pending_move(conn, id))?;
    let outcome = crate::sorter::resolve_proposal(app, id, true, destination.clone())?;
    let dest = destination.unwrap_or(proposed);
    Ok(Outcome::ok(format!(
        "Approved — {source} → {dest}\n{outcome}\nThe move is audit-logged and the notice at \
         the bottom of the window offers Undo."
    )))
}

fn dismiss_move(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let id = int_arg(input, "id")?;
    let (_, source, _) = with_conn(app, |conn| pending_move(conn, id))?;
    crate::sorter::resolve_proposal(app, id, false, None)?;
    Ok(Outcome::ok(format!(
        "Declined — {source} stays where it is and the card has left the queue.\nNothing \
         moved, so nothing was written and this cannot be undone; the file can be proposed \
         again by sorting the inbox."
    )))
}

fn approve_all_moves(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let class = with_conn(app, |conn| resolve_class(conn, &str_arg(input, "class")?))?;
    let waiting: i64 = with_conn(app, |conn| {
        Ok(conn.query_row(
            "SELECT COUNT(*) FROM move_proposals WHERE class_id = ?1 AND status = 'pending'",
            [class.id],
            |r| r.get(0),
        )?)
    })?;
    if waiting == 0 {
        bail!("{} has no file-move proposals waiting", class.display_name);
    }
    let outcome = crate::sorter::approve_all(app, class.id)?;
    let mut text = format!(
        "{} file(s) filed in {} — one batch, one Undo on the notice\n",
        outcome.approved.len(),
        class.display_name
    );
    for line in &outcome.skipped {
        text.push_str(&format!("- left waiting: {line}\n"));
    }
    Ok(Outcome::ok(text))
}

fn run_sort(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let class = with_conn(app, |conn| resolve_class(conn, &str_arg(input, "class")?))?;
    let job_id = crate::sorter::run_sort_job(app, class.id)?;
    Ok(Outcome::ok(format!(
        "Sort queued — job #{job_id} · {}\nIt reads the inbox and proposes a destination per \
         file. Nothing moves until Daniel approves. Report it as queued, not done.",
        class.display_name
    )))
}

fn run_syllabus_scan(app: &AppHandle, input: &Value, ctx: &ToolCtx) -> Result<Outcome> {
    let class = with_conn(app, |conn| resolve_class(conn, &str_arg(input, "class")?))?;
    let target = opt_str_arg(input, "target");
    let job_id = crate::deadlines::run_scan(app, class.id, target.as_deref(), ctx.today)?;
    Ok(Outcome::ok(format!(
        "Syllabus scan queued — job #{job_id} · {} · reading {}\nIts deadlines arrive as \
         proposals to approve; the course's divisions and its grade weights are recorded \
         directly. Report it as queued.",
        class.display_name,
        target.unwrap_or_else(|| "the whole class folder".into())
    )))
}

fn approve_deadlines(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let ids: Vec<i64> = match input["ids"].as_array() {
        Some(list) if !list.is_empty() => list
            .iter()
            .map(|v| {
                v.as_i64()
                    .context("every entry of 'ids' must be a proposal id")
            })
            .collect::<Result<Vec<_>>>()?,
        // A class instead: every proposal it has waiting, as `Add all` takes.
        _ => {
            let name = opt_str_arg(input, "class")
                .context("pass either 'ids' or a 'class' whose proposals to approve")?;
            with_conn(app, |conn| {
                let class = resolve_class(conn, &name)?;
                let mut stmt = conn.prepare(
                    "SELECT id FROM deadline_proposals
                     WHERE class_id = ?1 AND status = 'pending' ORDER BY id",
                )?;
                let ids = stmt
                    .query_map([class.id], |r| r.get::<_, i64>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                if ids.is_empty() {
                    bail!("{} has no deadline proposals waiting", class.display_name);
                }
                Ok(ids)
            })?
        }
    };
    let outcome = crate::deadlines::approve_proposals(app, &ids)?;
    let mut text = format!(
        "{} deadline(s) added — one batch, one Undo on the notice\n",
        outcome.approved.len()
    );
    for line in &outcome.skipped {
        text.push_str(&format!("- not added: {line}\n"));
    }
    Ok(Outcome::ok(text))
}

fn add_lecture(app: &AppHandle, input: &Value) -> Result<Outcome> {
    let class = with_conn(app, |conn| resolve_class(conn, &str_arg(input, "class")?))?;
    let date = str_arg(input, "date")?;
    // The week the form would show: for a course whose weeks carry dates the
    // nearest meeting's, resolved here rather than left to the caller, since
    // a request without one routes the lecture to the inbox for the sorter —
    // which for a dated course is a step nothing asked for.
    let week = match input["week"].as_i64() {
        Some(week) => Some(week),
        None => with_conn(app, |conn| {
            let slots = crate::units::week_slots(conn, class.id)?;
            Ok(if slots.iter().any(|s| s.meets_on.is_some()) {
                crate::units::nearest_week(&slots, &date)
            } else {
                None
            })
        })?,
    };
    let request = crate::lectures::AddRequest {
        class_id: class.id,
        source: str_arg(input, "source")?,
        week,
        date,
        title: opt_str_arg(input, "title"),
        // Off unless asked: a digest is a long subscription job, and the
        // reader saying "file this lecture" has not asked for one.
        digest: input["digest"].as_bool().unwrap_or(false),
        recording_id: None,
    };
    // The same one-per-class claim the Add lecture form takes, so a chat
    // filing and a form filing cannot run over each other.
    let _claim = crate::lectures::claim_ingest(class.id)?;
    let result = crate::lectures::add(app, &request, &|stage| {
        eprintln!("chat add_lecture: {stage}");
    })?;

    let mut text = if result.routed_to_inbox {
        format!(
            "Filed to the inbox — {}\nIts week could not be resolved, so it waits in \
             {}'s inbox for a sort proposal to place it.\n",
            result.rel_path, class.display_name
        )
    } else {
        format!(
            "Filed — {}{}\n",
            result.rel_path,
            result
                .unit_name
                .as_deref()
                .map(|u| format!(" · counts toward {u}"))
                .unwrap_or_default()
        )
    };
    if !result.speakers.is_empty() {
        text.push_str(&format!("Speakers named: {}\n", result.speakers.join(", ")));
    }
    match (result.digest_job_id, result.digest_error.as_deref()) {
        (Some(job), _) => text.push_str(&format!(
            "Session document queued — job #{job}. Report it as queued.\n"
        )),
        (None, Some(error)) => text.push_str(&format!("No session document: {error}\n")),
        (None, None) => text.push_str(
            "No session document was made. Ask for one with 'distil that lecture' if it \
             is wanted.\n",
        ),
    }
    Ok(Outcome::ok(text))
}

fn run_shift(app: &AppHandle) -> Result<Outcome> {
    let run_id = crate::shift::run_now(app)?;
    Ok(Outcome::ok(format!(
        "Shift started — run #{run_id}. It works through tonight's plan under tonight's caps \
         and reports in the Job Center. Report it as started, not finished."
    )))
}

fn undo_last(app: &AppHandle) -> Result<Outcome> {
    let last = with_conn(app, |conn| crate::undo::last_reversible(conn))?;
    let Some((ids, action)) = last else {
        bail!("nothing on record can be reversed — the log holds no action with an inverse");
    };
    let outcome = crate::undo::undo(app, &ids)?;
    let mut text = format!(
        "Reversed {} — {} row(s) put back\n",
        action,
        outcome.undone.len()
    );
    for line in &outcome.refused {
        text.push_str(&format!("- refused: {line}\n"));
    }
    Ok(Outcome::ok(text))
}

fn list_hints(conn: &Connection, input: &Value) -> Result<Outcome> {
    let class = resolve_class(conn, &str_arg(input, "class")?)?;
    let since = opt_str_arg(input, "since");
    let kind = opt_str_arg(input, "kind");
    let hints = crate::lectures::list_hints(conn, class.id)?;
    let kept: Vec<_> = hints
        .iter()
        .filter(|h| since.as_deref().is_none_or(|s| h.date.as_str() >= s))
        .filter(|h| kind.as_deref().is_none_or(|k| h.kind == k))
        .collect();
    if kept.is_empty() {
        return Ok(Outcome::ok(format!(
            "Nothing flagged in {}{}. A lecture has to be distilled before what the \
             professor said out loud is on record.",
            class.display_name,
            since.map(|s| format!(" since {s}")).unwrap_or_default()
        )));
    }
    let mut text = format!(
        "{} flagged item(s) in {}, newest session first\n",
        kept.len(),
        class.display_name
    );
    let mut session = String::new();
    for hint in kept {
        let heading = format!("{} — {}", hint.date, hint.unit_name);
        if heading != session {
            text.push_str(&format!(
                "\n## {heading}{}\n  transcript: {}/{}\n",
                if hint.title.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", hint.title)
                },
                class.folder_name,
                hint.rel_path
            ));
            session = heading;
        }
        text.push_str(&format!(
            "- [{}] {}{}\n",
            crate::lectures::hint_kind_label(&hint.kind),
            hint.text,
            hint.anchor
                .as_deref()
                .map(|a| format!(" ({a})"))
                .unwrap_or_default()
        ));
    }
    text.push_str(
        "\nCite a transcript's path with its HH:MM so it opens at that moment.\n",
    );
    Ok(Outcome::ok(text))
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


#[cfg(test)]
mod tests {
    use super::{
        definitions, is_write, overview_text, resolve_scope, waiting_line, ClassRow, Scope,
        WRITE_TOOLS,
    };
    use rusqlite::Connection;

    /// Biostatistics (seeded id 3) with a few of its weeks, two folders of
    /// material, one filed lecture and one waiting deadline proposal; Applied
    /// Generative AI (id 4) with an undated Part.
    fn fixture() -> Connection {
        let conn = crate::db::memory_db();
        let root = std::env::temp_dir().join(format!("classhub-overview-{}", std::process::id()));
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        for (class_id, ordinal, kind, name, starts_on) in [
            (3, 1, "week", "Week 1 \u{2014} Introduction", Some("2026-08-20")),
            (3, 2, "week", "Week 2 \u{2014} Study Designs", Some("2026-08-27")),
            (3, 3, "week", "Week 3 \u{2014} Data Exploration", Some("2026-09-03")),
            (3, 10, "week", "Week 10 \u{2014} Model evaluation", Some("2026-10-22")),
            (3, 13, "week", "Week 13 \u{2014} Reproducibility", Some("2026-11-12")),
            (4, 1, "part", "Part I: Deep Learning (Weeks 1-8)", None),
        ] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, starts_on, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'syllabus')",
                rusqlite::params![class_id, ordinal, kind, name, starts_on],
            )
            .expect("unit");
        }
        for rel_path in [
            "Module 1/Slides/deck.pptx",
            "Module 1/Reading Material/paper.pdf",
            "Module 2/Slides/deck.pptx",
            "Weeks/Week 02 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md",
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (3, ?1, 'abc', 1, 1, 'other')",
                [rel_path],
            )
            .expect("file");
        }
        let week2: i64 = conn
            .query_row(
                "SELECT id FROM units WHERE class_id = 3 AND ordinal = 2",
                [],
                |row| row.get(0),
            )
            .expect("week 2");
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (3, ?1, 'Weeks/Week 02 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md',
                     0, 1, 1, 1, '.classhub/corpus/Week 2 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md',
                     'Whole session', 'high', 'applied', 1)",
            [week2],
        )
        .expect("contribution");
        conn.execute(
            "INSERT INTO deadline_proposals (class_id, title, kind, due_at, status, source, created_at)
             VALUES (3, 'Form Teams', 'project', '2026-09-02', 'pending', 'syllabus', 1)",
            [],
        )
        .expect("proposal");
        conn
    }

    fn biostatistics() -> ClassRow {
        ClassRow {
            id: 3,
            display_name: "Biostatistics for AI".into(),
            folder_name: "Biostatistics for AI".into(),
        }
    }

    fn block(text: &str, name: &str) -> String {
        text.split("\n## ")
            .find(|block| block.starts_with(name))
            .expect("class block")
            .to_string()
    }

    /// Each dated course's block names its divisions and where it is today;
    /// a course that published no dates gets no such line rather than a
    /// computed one.
    #[test]
    fn the_overview_names_the_divisions_and_the_current_one_per_class() {
        let conn = fixture();
        let text = overview_text(&conn, false, "2026-09-02", None).expect("overview");
        let biostats = block(&text, "Biostatistics for AI");
        assert!(biostats.contains("\nDivisions: 5 weeks from the syllabus\n"), "{text}");
        assert!(biostats.contains("\nNow: Week 2 \u{2014} Study Designs\n"), "{text}");
        let applied = block(&text, "Applied Generative AI in Medicine");
        assert!(applied.contains("\nDivisions: 1 part from the syllabus\n"), "{text}");
        assert!(!applied.contains("Now:"), "{text}");
        let fundamentals = block(&text, "Fundamentals of AI in Medicine I");
        assert!(fundamentals.contains("\nDivisions: none declared\n"), "{text}");
        assert!(!fundamentals.contains("Now:"), "{text}");
    }

    /// The compact form counts lectures, names folders as folders and says
    /// what waits; the detailed form lists divisions with dates, lectures with
    /// their notes, and proposals with their ids.
    #[test]
    fn the_overview_carries_lectures_folders_and_proposals() {
        let conn = fixture();
        let compact = block(
            &overview_text(&conn, false, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(
            compact.contains(
                "\nLectures: 1 filed, 0 distilled, 0 session documents · 1 in the current division (0 distilled)\n"
            ),
            "{compact}"
        );
        // Names of divisions with lectures belong to the detailed form.
        assert!(!compact.contains("Study Designs (1)"), "{compact}");
        assert!(
            compact.contains("folders: Module 1 (2), Module 2 (1), Weeks (1)\n"),
            "{compact}"
        );
        assert!(compact.contains("\nWaiting: 1 deadline proposal\n"), "{compact}");
        assert!(!compact.contains("#"), "ids belong to the detailed form: {compact}");

        let detailed = block(
            &overview_text(&conn, true, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(
            detailed.contains(
                "\n- 2. Week 2 \u{2014} Study Designs · from 2026-08-27 · NOW · 1 lecture (0 distilled)\n"
            ),
            "{detailed}"
        );
        assert!(detailed.contains("\n- 3. Week 3 \u{2014} Data Exploration · from 2026-09-03\n"), "{detailed}");
        assert!(
            detailed.contains(
                "\n- Weeks/Week 02 \u{2014} Study Designs/2026-08-27 \u{2014} Lecture.md · feeds Week 2 \u{2014} Study Designs · not distilled yet\n"
            ),
            "{detailed}"
        );
        assert!(
            detailed.contains(
                "\n- deadline proposal #1 · Form Teams (project) due 2026-09-02 · from the syllabus scan\n"
            ),
            "{detailed}"
        );
    }

    /// The professor's latest announcements ride the compact form as titles
    /// alone, and the detailed form quotes their text; a course with none
    /// gets no line.
    #[test]
    fn the_overview_carries_the_latest_notices() {
        let conn = fixture();
        let long_body = "Bring your laptop. ".repeat(50);
        for (canvas_id, title, body, posted_at) in [
            ("1", "Welcome", "Slides are under Module 1.", "2026-08-20T00:00"),
            ("2", "Office hours\nmoved", "Thursday at <b>6 PM</b> this week.\n\nBen", "2026-09-02T15:03"),
            ("3", "Quiz 1 posted", long_body.as_str(), "2026-08-28T09:00"),
            ("4", "Reading for week 2", "Chapter 3.", "2026-08-25T09:00"),
        ] {
            conn.execute(
                "INSERT INTO announcements (class_id, canvas_id, title, body, posted_at)
                 VALUES (3, ?1, ?2, ?3, ?4)",
                rusqlite::params![canvas_id, title, body, posted_at],
            )
            .expect("announcement");
        }
        let compact = block(
            &overview_text(&conn, false, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(
            compact.contains(
                "\nNotices: \"Office hours moved\" (2026-09-02); \"Quiz 1 posted\" (2026-08-28); \"Reading for week 2\" (2026-08-25)\n"
            ),
            "{compact}"
        );
        assert!(!compact.contains("Thursday"), "bodies belong to the detailed form: {compact}");
        let detailed = block(
            &overview_text(&conn, true, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(detailed.contains("\nNotices (latest 3 of 4 announcements):\n"), "{detailed}");
        assert!(
            detailed.contains("\n- 2026-09-02 · Office hours moved\n  Thursday at <b>6 PM</b> this week. Ben\n"),
            "{detailed}"
        );
        assert!(!detailed.contains("Welcome"), "only the latest three: {detailed}");
        // A long body is quoted up to the cap and ellipsized, on one line.
        let quiz_line = detailed
            .lines()
            .find(|line| line.starts_with("  Bring your laptop."))
            .expect("the quiz body line");
        assert!(quiz_line.ends_with('…'), "{quiz_line}");
        assert_eq!(quiz_line.trim_start().chars().count(), super::NOTICE_BODY_CHARS + 1);
        let applied = block(
            &overview_text(&conn, true, "2026-09-02", None).expect("overview"),
            "Applied Generative AI in Medicine",
        );
        assert!(!applied.contains("Notices"), "{applied}");
    }

    /// The scope both triggers share: a division by its number, its topic or
    /// its storage key, a folder by name, the master by any of its names.
    #[test]
    fn the_scope_parser_finds_divisions_and_folders() {
        let conn = fixture();
        let class = biostatistics();
        let unit_named = |scope: &str| match resolve_scope(&conn, &class, scope) {
            Ok(Scope::Unit { name, .. }) => name,
            other => panic!("{scope}: {other:?}"),
        };
        assert_eq!(unit_named("Week 3"), "Week 3 \u{2014} Data Exploration");
        // The number is an exact key, so Week 1 is not a substring of Week 10
        // and Week 13.
        assert_eq!(unit_named("week 1"), "Week 1 \u{2014} Introduction");
        assert_eq!(unit_named("Data Exploration"), "Week 3 \u{2014} Data Exploration");
        assert_eq!(
            unit_named("unit:Week 3 \u{2014} Data Exploration"),
            "Week 3 \u{2014} Data Exploration"
        );
        assert_eq!(
            resolve_scope(&conn, &class, "Module 1").expect("folder"),
            Scope::Folder("Module 1".into())
        );
        assert_eq!(
            resolve_scope(&conn, &class, "m2").expect("folder"),
            Scope::Folder("Module 2".into())
        );
        assert_eq!(resolve_scope(&conn, &class, "master").expect("master"), Scope::Master);
        assert_eq!(
            resolve_scope(&conn, &class, "whole semester").expect("master"),
            Scope::Master
        );
        // The storage key names the row, so a scope copied out of a listing
        // resolves to it without a name.
        let week3 = match resolve_scope(&conn, &class, "Week 3").expect("week 3") {
            Scope::Unit { id, .. } => id,
            other => panic!("{other:?}"),
        };
        assert_eq!(unit_named(&format!("unit:{week3}")), "Week 3 \u{2014} Data Exploration");
        assert!(resolve_scope(&conn, &class, "unit:999").is_err());
        assert_eq!(
            Scope::Unit {
                id: 24,
                name: "Week 3 \u{2014} Data Exploration".into(),
                kind: "week".into(),
                number: 3,
            }
            .key(),
            "unit:24"
        );
    }

    /// Nothing is guessed: an empty needle, a needle matching several scopes,
    /// and one matching none are each an error naming the candidates.
    #[test]
    fn the_scope_parser_refuses_rather_than_guessing() {
        let conn = fixture();
        let class = biostatistics();
        let error = |scope: &str| match resolve_scope(&conn, &class, scope) {
            Err(e) => format!("{e:#}"),
            Ok(found) => panic!("{scope} resolved to {found:?}"),
        };
        assert!(error("?").starts_with("which scope?"), "{}", error("?"));
        let ambiguous = error("week");
        assert!(ambiguous.contains("matches 6 scopes"), "{ambiguous}");
        let missing = error("Week 99");
        assert!(missing.contains("divisions: Week 1"), "{missing}");
        assert!(missing.contains("folders: Module 1, Module 2, Weeks"), "{missing}");
    }

    /// A folder named exactly like a division is that division's folder, so
    /// the division wins and its sources include the folder's files. A folder
    /// that only nearly matches was never attached, so it stays its own scope
    /// and a query hitting both is refused as ambiguous.
    #[test]
    fn a_folder_named_like_a_division_resolves_to_the_division() {
        let conn = fixture();
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, rel_path, source)
             VALUES (3, 1, 'module', 'Module 1', 'Module 1', 'canvas')",
            [],
        )
        .expect("unit");
        match resolve_scope(&conn, &biostatistics(), "Module 1").expect("resolves") {
            Scope::Unit { name, .. } => assert_eq!(name, "Module 1"),
            other => panic!("{other:?}"),
        }

        conn.execute(
            "UPDATE units SET name = 'module 2', rel_path = NULL WHERE name = 'Module 1'",
            [],
        )
        .expect("rename");
        match resolve_scope(&conn, &biostatistics(), "Module 2") {
            Err(e) => assert!(format!("{e:#}").contains("matches 2 scopes"), "{e:#}"),
            Ok(found) => panic!("guessed {found:?}"),
        }
    }

    /// The overview's small helpers: the session document's twin and stem,
    /// who proposed what, each scope's storage key, and the divisions line
    /// across sources and kinds.
    #[test]
    fn the_overview_s_helpers_name_things_the_way_the_app_does() {
        use super::{divisions_line, document_stem, markdown_twin, proposer};
        assert_eq!(
            markdown_twin("Study Guides/Sessions/2026-09-01 \u{2014} Topic.html"),
            "Study Guides/Sessions/2026-09-01 \u{2014} Topic.md"
        );
        assert_eq!(markdown_twin("Study Guides/Sessions/odd.html.html"), "Study Guides/Sessions/odd.html.md");
        assert_eq!(document_stem("Study Guides/Sessions/2026-09-01 \u{2014} Topic.html"), "2026-09-01 \u{2014} Topic");
        assert_eq!(document_stem("bare"), "bare");
        assert_eq!(proposer("canvas"), "Canvas");
        assert_eq!(proposer("syllabus"), "the syllabus scan");
        assert_eq!(proposer("sort_job"), "a sort job");
        assert_eq!(proposer("chat"), "chat");
        assert_eq!(proposer("by_name"), "its file name");
        assert_eq!(proposer("?"), "an unknown reader");
        assert_eq!(Scope::Master.key(), "master");
        assert_eq!(Scope::Folder("Module 1".into()).key(), "Module 1");

        let unit = |kind: &str, source: &str| crate::units::UnitInfo {
            id: 1,
            ordinal: 1,
            kind: kind.into(),
            name: "x".into(),
            number: None,
            rel_path: None,
            starts_on: None,
            ends_on: None,
            first_week: None,
            last_week: None,
            source: source.into(),
            materials: None,
        };
        assert_eq!(divisions_line(&[]), "Divisions: none declared\n");
        assert_eq!(divisions_line(&[unit("week", "syllabus")]), "Divisions: 1 week from the syllabus\n");
        assert_eq!(
            divisions_line(&[unit("module", "canvas"), unit("module", "canvas")]),
            "Divisions: 2 modules from Canvas\n"
        );
        assert_eq!(
            divisions_line(&[unit("part", "canvas"), unit("week", "syllabus")]),
            "Divisions: 2 divisions from Canvas and the syllabus\n"
        );
    }

    /// The one new line of user-facing prose in the overview: nothing when
    /// nothing waits, each queue named only when it holds something, both
    /// joined when both do.
    #[test]
    fn the_waiting_line_names_only_the_queues_that_hold_something() {
        assert_eq!(waiting_line(0, 0), None);
        assert_eq!(
            waiting_line(2, 0).as_deref(),
            Some("2 file move proposal(s) awaiting Daniel's approval")
        );
        assert_eq!(
            waiting_line(0, 9).as_deref(),
            Some("9 deadline proposal(s) awaiting Daniel's approval")
        );
        assert_eq!(
            waiting_line(2, 9).as_deref(),
            Some("2 file move proposal(s) and 9 deadline proposal(s) awaiting Daniel's approval")
        );
    }

    /// What the professor flagged rides the overview (SPEC §8.4): a count and
    /// the earliest session's date in the compact form, the items newest
    /// first in the detailed one, and no line for a class with none.
    #[test]
    fn the_overview_counts_what_was_flagged() {
        let conn = fixture();
        let compact = block(
            &overview_text(&conn, false, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(!compact.contains("Flagged"), "{compact}");
        let contribution: i64 = conn
            .query_row("SELECT id FROM lecture_contributions WHERE class_id = 3", [], |row| row.get(0))
            .expect("contribution");
        let week2: i64 = conn
            .query_row("SELECT unit_id FROM lecture_contributions WHERE id = ?1", [contribution], |row| row.get(0))
            .expect("unit");
        for (kind, text, anchor) in [
            ("exam_hint", "Study designs are on Quiz 1", Some("00:45")),
            ("confusion", "Case-control versus cohort", None),
        ] {
            conn.execute(
                "INSERT INTO lecture_hints (class_id, unit_id, contribution_id, kind, text, anchor, created_at)
                 VALUES (3, ?1, ?2, ?3, ?4, ?5, 1)",
                rusqlite::params![week2, contribution, kind, text, anchor],
            )
            .expect("hint");
        }
        let compact = block(
            &overview_text(&conn, false, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(compact.contains("\nFlagged: 2 items since 2026-08-27\n"), "{compact}");

        // The whole hub in detail counts them and leaves the items to
        // `list_hints`: the four classes' ledgers together ran the one tool
        // result past its cap, and the ids the approval tools read sit after
        // them.
        let all = block(
            &overview_text(&conn, true, "2026-09-02", None).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(all.contains("\nFlagged: 2 items since 2026-08-27\n"), "{all}");
        assert!(!all.contains("Study designs are on Quiz 1"), "{all}");

        // One class asked about carries its ledger.
        let focused = block(
            &overview_text(&conn, true, "2026-09-02", Some(3)).expect("overview"),
            "Biostatistics for AI",
        );
        assert!(focused.contains("\nFlagged (2 items since 2026-08-27, newest first):\n"), "{focused}");
        assert!(focused.contains("\n- 2026-08-27 00:45 · exam hint · Study designs are on Quiz 1\n"), "{focused}");
        assert!(focused.contains("\n- 2026-08-27 · where the room got stuck · Case-control versus cohort\n"), "{focused}");

        // And nothing else: a focused overview is one class's.
        let whole = overview_text(&conn, true, "2026-09-02", Some(3)).expect("overview");
        assert!(!whole.contains("## Applied Generative AI in Medicine"), "{whole}");
        assert!(whole.contains("## Open deadlines"), "{whole}");
    }

    /// The waiting queue comes before the ledger, so the ids the approval
    /// tools read are the last thing a cut result would lose.
    #[test]
    fn the_waiting_queue_precedes_the_flagged_ledger() {
        let conn = fixture();
        let text = overview_text(&conn, true, "2026-09-02", Some(3)).expect("overview");
        let waiting = text.find("Waiting for approval:").expect("waiting block");
        let flagged = text.find("Flagged").unwrap_or(usize::MAX);
        assert!(waiting < flagged, "the ids must come first");
    }

    /// The compact overview carries a week of deadlines and a count of the
    /// rest; the detailed form still carries them all with their ids, so
    /// nothing chat could address is out of reach. Measured on the real hub,
    /// this took the block that rides every turn from 4,436 bytes to 741.
    #[test]
    fn the_compact_overview_carries_a_week_of_deadlines_and_counts_the_rest() {
        let conn = fixture();
        conn.execute("DELETE FROM deadlines", []).expect("clear");
        for (title, due) in [
            ("Overdue homework", "2026-08-30"),
            ("Due today", "2026-09-02"),
            ("Due inside the week", "2026-09-09T23:59"),
            ("Due the day after the week", "2026-09-10"),
            ("Due next month", "2026-10-04"),
        ] {
            conn.execute(
                "INSERT INTO deadlines (class_id, title, kind, due_at, status, source)
                 VALUES (3, ?1, 'assignment', ?2, 'open', 'manual')",
                rusqlite::params![title, due],
            )
            .expect("deadline");
        }

        let compact = overview_text(&conn, false, "2026-09-02", None).expect("overview");
        let block = compact.split("## Open deadlines\n").nth(1).expect("block");
        // Overdue and inside the week, in due order; the last day counts as in.
        assert!(block.contains("Overdue homework"), "{block}");
        assert!(block.contains("Due today"), "{block}");
        assert!(block.contains("Due inside the week"), "{block}");
        assert!(!block.contains("Due the day after the week"), "{block}");
        assert!(!block.contains("Due next month"), "{block}");
        assert!(
            block.contains("2 more open after the next 7 days"),
            "{block}"
        );

        // The detailed form is what carries the ids, and carries them all.
        let detailed = overview_text(&conn, true, "2026-09-02", None).expect("overview");
        let block = detailed.split("## Open deadlines\n").nth(1).expect("block");
        assert!(block.contains("Due next month"), "{block}");
        assert!(!block.contains("more open"), "{block}");
    }

    /// The write-tool list the sidebar reads is the list `is_write` answers
    /// from, and every name on it is a tool `execute` dispatches — a write
    /// tool that fell out of either would render as a read, quietly.
    #[test]
    fn the_write_tool_names_are_the_schemas_own() {
        let schemas = definitions();
        let names: Vec<&str> = schemas
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for tool in WRITE_TOOLS {
            assert!(names.contains(tool), "{tool} has no schema");
            assert!(is_write(tool), "{tool} does not read as a write");
        }
        // The read tools are not on it.
        for read in ["get_overview", "list_material", "search_material", "read_material", "list_hints"] {
            assert!(names.contains(&read), "{read} has no schema");
            assert!(!is_write(read), "{read} reads as a write");
        }
        // Every schema is one or the other, and none is named twice.
        let mut seen = std::collections::HashSet::new();
        for name in &names {
            assert!(seen.insert(*name), "{name} is defined twice");
        }
        assert_eq!(names.len(), WRITE_TOOLS.len() + 5);
    }
}
