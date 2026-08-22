//! SPEC §9 — the chat agent's read tools: `get_overview`, `list_material`,
//! `search_material`, `read_material`.
//!
//! Retrieval is agentic, not vector-based (SPEC §1: no embeddings API): the
//! agent greps the pre-extracted markdown under `.classhub/extracts/` plus
//! notes and generated guides, then reads bounded windows of what it found.
//! Every path crossing the tool boundary is relative to the AIBHS root and
//! starts with a class folder name, so a path the agent reads back is a path it
//! can hand straight to `read_material` — and one the sidebar can open.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde_json::{json, Value};

const EXTRACTS_DIR: &str = ".classhub/extracts";
const GUIDES_DIR: &str = "Study Guides";
const NOTES_DIR: &str = "Notes";

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

/// Tool schemas sent with every request (SPEC §9 read tools; write tools land
/// in M8).
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
            "description": "Grep every extract, note and study guide, returning matching lines with file paths and line numbers. This is the primary way to find content: search before reading. The query is a regular expression, case-insensitive unless it contains an uppercase letter. Prefer short distinctive phrases; if a query comes back thin, try a synonym or a narrower term.",
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
        }
    ])
}

/// Runs one tool call. Failures come back as tool results, not transport
/// errors — the model can correct a bad path or a thin query on its own.
pub fn execute(conn: &Connection, name: &str, input: &Value) -> Outcome {
    let result = match name {
        "get_overview" => overview_text(conn, true).map(Outcome::ok),
        "list_material" => list_material(conn, input),
        "search_material" => search_material(conn, input),
        "read_material" => read_material(conn, input),
        other => Err(anyhow::anyhow!(
            "unknown tool '{other}' — available: get_overview, list_material, search_material, read_material"
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

    let mut exact = Vec::new();
    let mut partial = Vec::new();
    let mut loose = Vec::new();
    for row in &rows {
        let candidates = [squash(&row.display_name), squash(&row.folder_name)];
        if candidates.iter().any(|c| *c == needle) {
            exact.push(row);
        } else if candidates.iter().any(|c| c.contains(&needle)) {
            partial.push(row);
        } else if candidates.iter().any(|c| is_subsequence(&needle, c)) {
            loose.push(row);
        }
    }
    for tier in [exact, partial, loose] {
        match tier.len() {
            1 => return Ok(tier[0].clone()),
            0 => {}
            n => bail!(
                "'{query}' matches {n} classes ({}) — use the full name",
                tier.iter()
                    .map(|r| r.display_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    bail!("no class matches '{query}'. Classes: {}", names())
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
            let now = now_secs();
            let described = guides
                .iter()
                .map(|g| {
                    let scope = if g.scope == "master" {
                        "Semester Master".to_string()
                    } else {
                        g.scope.clone()
                    };
                    if detailed {
                        format!(
                            "{scope} — {} ({}, {})",
                            if g.stale { "STALE" } else { "fresh" },
                            g.rel_path,
                            days_ago(now, g.generated_at)
                        )
                    } else {
                        format!("{scope} ({})", if g.stale { "STALE" } else { "fresh" })
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(&format!("Guides: {described}\n"));
        }
    }

    let mut deadline_stmt = conn.prepare(
        "SELECT c.display_name, d.title, d.kind, d.due_at, d.notes
         FROM deadlines d JOIN classes c ON c.id = d.class_id
         WHERE d.status = 'open' ORDER BY d.due_at LIMIT 25",
    )?;
    let deadlines = deadline_stmt
        .query_map([], |row| {
            let class: String = row.get(0)?;
            let title: String = row.get(1)?;
            let kind: String = row.get(2)?;
            let due: String = row.get(3)?;
            let notes: Option<String> = row.get(4)?;
            Ok(format!(
                "- {due} · {class} · {title} ({kind}){}",
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
    for (rel_path, kind, size, extracted) in &rows {
        if let Some(sub) = &subpath {
            let inside = rel_path == sub || rel_path.starts_with(&format!("{sub}/"));
            if !inside {
                continue;
            }
        }
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
            "{} — {shown} of {} indexed file(s) under '{sub}'\n",
            class.display_name,
            rows.len()
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

    let notes = list_notes(&root.join(&class.folder_name).join(NOTES_DIR));
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

fn list_notes(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.starts_with('.'))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// search_material (ripgrep over extracts, notes and guides — SPEC §9)

fn search_material(conn: &Connection, input: &Value) -> Result<Outcome> {
    let query = str_arg(input, "query")?;
    let root = crate::db::aibhs_root(conn)?;
    let classes = match opt_str_arg(input, "class") {
        Some(name) => vec![resolve_class(conn, &name)?],
        None => class_rows(conn)?,
    };

    let mut dirs = Vec::new();
    for class in &classes {
        let base = root.join(&class.folder_name);
        for sub in [EXTRACTS_DIR, NOTES_DIR, GUIDES_DIR] {
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

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
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

fn format_size(bytes: i64) -> String {
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
