//! SPEC §7 — ingestion & extraction pipeline (steps 2–4) and the staleness
//! manifest utility (step 5 groundwork; badges land in M5).
//!
//! A file is stale when `extracted_sha256` differs from its current `sha256`
//! (the scanner's upsert deliberately leaves extract columns untouched).
//! Text-native formats are extracted locally — zero tokens — and so are the
//! two that need a local pass first: a DOCX goes through headless LibreOffice
//! to HTML and then the HTML stripper, a notebook through `notebook::flatten`.
//! PPTX sources are converted to PDF via headless LibreOffice, then all stale
//! PDFs go into one batched `extract` job per class through the job runner
//! (SPEC §6, the single gateway to the subscription). Extract paths mirror the
//! source rel path with `.md` appended under `.classhub/extracts/` (SPEC §4).

use std::collections::BTreeSet;
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::{EXTRACTS_DIR, lock, now};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/extract.md");

/// Serializes pipeline runs: headless soffice tolerates one instance at a
/// time, and the check-then-enqueue job guard must not race a second scan.
static PIPELINE_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Pipeline

struct StaleFile {
    rel_path: String,
    sha256: String,
    kind: String,
}

/// One PDF handed to the batched claude job. `input_rel_path` is what claude
/// reads (the source PDF, or the converted `<name>.pptx.pdf`); the rest is the
/// record-keeping manifest finalized after the job succeeds.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BatchItem {
    rel_path: String,
    sha256: String,
    input_rel_path: String,
    extract_rel_path: String,
}

#[cfg(test)]
impl BatchItem {
    /// A PDF item as the pipeline would batch it: read as itself, written to
    /// its mirrored extract path.
    fn test_item(rel_path: &str, sha256: &str) -> Self {
        BatchItem {
            rel_path: rel_path.to_string(),
            sha256: sha256.to_string(),
            input_rel_path: rel_path.to_string(),
            extract_rel_path: format!("{EXTRACTS_DIR}/{rel_path}.md"),
        }
    }
}

#[cfg_attr(test, derive(Debug, PartialEq))]
enum Route {
    Text,
    Html,
    Caption,
    Notebook,
    Csv,
    Pdf,
    Pptx,
    Docx,
    Skip,
}

fn route(rel_path: &str, kind: &str) -> Route {
    match kind {
        "rmd" | "r" | "md" | "py" => Route::Text,
        "csv" => Route::Csv,
        "html" => Route::Html,
        "caption" => Route::Caption,
        "ipynb" => Route::Notebook,
        "pdf" => Route::Pdf,
        "pptx" => Route::Pptx,
        "docx" => Route::Docx,
        // Deliberately skipped: transcribing is minutes of compute, so it
        // happens when a lecture is explicitly added, never as a side effect of
        // a scan noticing a media file somewhere in the tree.
        "media" => Route::Skip,
        // Zoom's in-meeting "Save Transcript" writes a `.txt` that is a caption
        // track in everything but extension. Down the text route its extract is
        // the raw two-second timing grid — the exact thing `Caption` exists to
        // collapse — and the same file added through Add lecture is normalized,
        // so the two paths disagreed about one input. `parse` falls through to
        // untimed blocks when there is no grid, so an ordinary .txt is fine.
        _ if rel_path.to_lowercase().ends_with(".txt") => Route::Caption,
        _ => Route::Skip,
    }
}

/// SPEC §7: extraction runs automatically after every scan. Everything here is
/// idempotent against `extracted_sha256`, so a scan that found no changes does
/// zero work and spends zero tokens.
pub fn spawn_pipeline(app: &AppHandle, class_id: i64) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(e) = run_pipeline(&app, class_id) {
            eprintln!("extract pipeline class {class_id}: {e:#}");
        }
    });
}

/// A single conversion may take a while on a large deck, but not this long —
/// past it, soffice is wedged rather than slow.
const CONVERT_TIMEOUT: Duration = Duration::from_secs(180);

/// The pipeline run to its end on the calling thread, answering with the extract
/// job it enqueued, if any — what the scan spawns on a thread and the shift
/// waits on (SPEC §6).
pub(crate) fn run_pipeline(app: &AppHandle, class_id: i64) -> Result<Option<i64>> {
    let _serial = lock(&PIPELINE_LOCK);

    let (class_dir, stale) = {
        let db = app.state::<crate::Db>();
        let conn = lock(&db.0);
        if has_active_extract_job(&conn, class_id)? {
            // The running job's record keeping lands on completion; the next
            // scan picks up anything it left stale. This is an early-out, not
            // the guard: the conversions below take long enough for the other
            // process to enqueue meanwhile, so the enqueue re-checks inside its
            // own transaction.
            return Ok(None);
        }
        (crate::scanner::class_dir(&conn, class_id)?, stale_files(&conn, class_id)?)
    };
    if stale.is_empty() {
        eprintln!("extract pipeline class {class_id}: nothing stale — zero work");
        return Ok(None);
    }

    let mut batch: Vec<BatchItem> = Vec::new();
    for file in &stale {
        let extract_rel = format!("{EXTRACTS_DIR}/{}.md", file.rel_path);
        // Zero-token: read `input_rel` (the source, or what LibreOffice made
        // of it), write the extract, record it against the source.
        let local = |input_rel: &str, how: Route| -> Result<()> {
            extract_local(&class_dir, input_rel, &extract_rel, &how)?;
            let db = app.state::<crate::Db>();
            let conn = lock(&db.0);
            record(&conn, &class_dir, class_id, &file.rel_path, &extract_rel, &file.sha256)
        };
        let outcome = match route(&file.rel_path, &file.kind) {
            how @ (Route::Text | Route::Html | Route::Caption | Route::Notebook | Route::Csv) => {
                local(&file.rel_path, how)
            }
            Route::Docx => convert(app, &class_dir, &file.rel_path, &file.sha256, &DOCX_TO_HTML)
                .and_then(|html_rel| local(&html_rel, Route::Html)),
            Route::Pdf => {
                batch.push(BatchItem {
                    rel_path: file.rel_path.clone(),
                    sha256: file.sha256.clone(),
                    input_rel_path: file.rel_path.clone(),
                    extract_rel_path: extract_rel.clone(),
                });
                Ok(())
            }
            Route::Pptx => convert(app, &class_dir, &file.rel_path, &file.sha256, &PPTX_TO_PDF)
                .map(|pdf_rel| {
                    batch.push(BatchItem {
                        rel_path: file.rel_path.clone(),
                        sha256: file.sha256.clone(),
                        input_rel_path: pdf_rel,
                        extract_rel_path: extract_rel.clone(),
                    })
                }),
            Route::Skip => Ok(()),
        };
        if let Err(e) = outcome {
            eprintln!(
                "extract pipeline class {class_id}: extract of {} failed: {e:#}",
                file.rel_path
            );
        }
    }

    if batch.is_empty() {
        return Ok(None);
    }
    let files_list = batch
        .iter()
        .map(|b| format!("- input: {}\n  output: {}", b.input_rel_path, b.extract_rel_path))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = PROMPT_TEMPLATE.replace("{files}", &files_list);
    let payload = serde_json::to_string(&batch)?;
    let Some(job_id) = crate::jobs::enqueue_extract(app, class_id, &prompt, payload)? else {
        // The other process enqueued this class's batch while this one was
        // converting; its run records the results.
        eprintln!("extract pipeline class {class_id}: an extract job is already active — not enqueuing a second");
        return Ok(None);
    };
    eprintln!(
        "extract pipeline class {class_id}: enqueued extract job {job_id} for {} PDF(s)",
        batch.len()
    );
    Ok(Some(job_id))
}

/// SPEC §7 step 4: record keeping after a successful claude extract job.
/// Called by the job runner before it marks the row succeeded, so a scan that
/// sees no active job also sees up-to-date extract columns.
pub fn finalize_job(app: &AppHandle, class_id: i64, payload: &str) -> Result<()> {
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    reconcile(&conn, class_id, payload, |_, item| {
        eprintln!(
            "extract job wrote no output for {} — stays stale for the next scan",
            item.rel_path
        );
        Ok(false)
    })?;
    Ok(())
}

/// The record keeping every extract run ends with: an extract the run wrote
/// — a non-empty file at the contracted path — is recorded as the source's,
/// and each item it wrote nothing for is handed to `unwritten`, which says
/// whether it counts as refused. Returns the paths it counted.
fn reconcile(
    conn: &Connection,
    class_id: i64,
    payload: &str,
    mut unwritten: impl FnMut(&Connection, &BatchItem) -> Result<bool>,
) -> Result<Vec<String>> {
    let items: Vec<BatchItem> =
        serde_json::from_str(payload).context("parsing extract batch payload")?;
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let mut counted = Vec::new();
    for item in items {
        let written = fs::metadata(class_dir.join(&item.extract_rel_path))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if written {
            record(conn, &class_dir, class_id, &item.rel_path, &item.extract_rel_path, &item.sha256)?;
        } else if unwritten(conn, &item)? {
            counted.push(item.rel_path);
        }
    }
    Ok(counted)
}

/// Whether a failed run's error is the model refusing the material outright —
/// Claude's content filter on a PDF — rather than a stall a retry could pass.
pub fn refused_by_filter(error: &str) -> bool {
    error.contains("content filtering")
}

/// Record keeping after a run the model refused (`refused_by_filter`): an
/// extract the run did write is recorded as any extract is; a file the run
/// read and wrote nothing for was the one refused, and is marked attempted at
/// its current hash with no extract, so the pipeline does not enqueue the
/// same refusal on every scan — Biostatistics' Week 4 reading cost a run per
/// launch on 2026-09-08 until it did; a file the run never reached stays
/// stale for the next run, since the refusal was not its. `read` is the run's
/// own log (`jobs::log_paths`); when the log could not be read the batch is
/// marked only if it held one file, which the refusal can only have been.
/// A changed file is tried again, since its hash moves. Returns the paths
/// left without an extract, for the job's error line.
pub fn record_refusal(
    app: &AppHandle,
    class_id: i64,
    payload: &str,
    read: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    refusal_in(&conn, class_id, payload, read)
}

fn refusal_in(
    conn: &Connection,
    class_id: i64,
    payload: &str,
    read: &BTreeSet<String>,
) -> Result<Vec<String>> {
    let alone = serde_json::from_str::<Vec<BatchItem>>(payload).map(|b| b.len() == 1).unwrap_or(false);
    reconcile(conn, class_id, payload, |conn, item| {
        let reached = read.contains(&item.input_rel_path) || (read.is_empty() && alone);
        if !reached {
            eprintln!(
                "extract of {} was not reached before the run was refused — stays stale for \
                 the next run",
                item.rel_path
            );
            return Ok(false);
        }
        mark_attempted(conn, class_id, &item.rel_path, &item.sha256)?;
        eprintln!(
            "extract of {} was refused by the model's content filter — left without an \
             extract until the file changes",
            item.rel_path
        );
        Ok(true)
    })
}

/// A row tried at this hash and left without an extract: `stale_files` skips
/// it until its hash moves, and every reader of `extract_rel_path` already
/// says "no extract available" for it.
fn mark_attempted(conn: &Connection, class_id: i64, rel_path: &str, sha256: &str) -> Result<()> {
    conn.execute(
        "UPDATE files SET extract_rel_path = NULL, extracted_at = ?1, extracted_sha256 = ?2
         WHERE class_id = ?3 AND rel_path = ?4",
        params![now(), sha256, class_id, rel_path],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// DB helpers

fn stale_files(conn: &Connection, class_id: i64) -> Result<Vec<StaleFile>> {
    // A duplicate is read once, through its canonical copy (SPEC §7 step 1):
    // it is not extracted while it is marked, and becomes canonical at the
    // next scan if the other copy leaves.
    let mut stmt = conn.prepare(
        "SELECT rel_path, sha256, kind FROM files
         WHERE class_id = ?1 AND duplicate_of IS NULL
           AND (extracted_sha256 IS NULL OR extracted_sha256 != sha256)
         ORDER BY rel_path",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(StaleFile {
                rel_path: row.get(0)?,
                sha256: row.get(1)?,
                kind: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn has_active_extract_job(conn: &Connection, class_id: i64) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = 'extract' AND class_id = ?1 AND status IN ('queued', 'running')",
        [class_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Records an extract against its source's row. The scan takes no pipeline
/// lock, so a source can be deleted between `stale_files` and this write: by
/// now a scan has dropped its row and cleared its mirror, and the extract just
/// written would be an orphan nothing points at and chat's search still finds.
/// A record no row takes therefore goes the way of the row's mirror.
fn record(
    conn: &Connection,
    class_dir: &Path,
    class_id: i64,
    rel_path: &str,
    extract_rel_path: &str,
    sha256: &str,
) -> Result<()> {
    let took = conn.execute(
        "UPDATE files SET extract_rel_path = ?1, extracted_at = ?2, extracted_sha256 = ?3
         WHERE class_id = ?4 AND rel_path = ?5",
        params![extract_rel_path, now(), sha256, class_id, rel_path],
    )?;
    if took == 0 {
        eprintln!("extract of {rel_path} outlived its row; its mirror entries go with it");
        remove_mirror(class_dir, rel_path);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Local (zero-token) extraction

/// Largest source a local route reads whole. A notebook carries every figure
/// as base64 and the read is copied twice before it is parsed, so past this a
/// file is skipped with a logged reason rather than taking the pipeline
/// thread down with it; it stays stale, and the log says why. A CSV is not
/// read whole at all.
const MAX_LOCAL_BYTES: u64 = 32 * 1024 * 1024;

/// The file as text, CRLF normalized, refused above `cap` bytes.
fn read_text(source: &Path, input_rel: &str, cap: u64) -> Result<String> {
    let len = fs::metadata(source)
        .with_context(|| format!("reading {input_rel}"))?
        .len();
    if len > cap {
        bail!(
            "{input_rel} is {:.0} MB — too large to extract locally (the cap is {} MB)",
            len as f64 / 1_048_576.0,
            cap / 1_048_576
        );
    }
    let raw = fs::read(source).with_context(|| format!("reading {input_rel}"))?;
    Ok(String::from_utf8_lossy(&raw).replace("\r\n", "\n"))
}

/// The file line by line, each decoded on its own, so a dataset of any size
/// costs one line of memory at a time.
fn text_lines(source: &Path, input_rel: &str) -> Result<impl Iterator<Item = String>> {
    let file = fs::File::open(source).with_context(|| format!("reading {input_rel}"))?;
    Ok(std::io::BufReader::new(file)
        .split(b'\n')
        .map_while(Result::ok)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim_end_matches('\r').to_string()))
}

/// `input_rel` is the file read — the source itself, or for a DOCX the HTML
/// LibreOffice made of it — while the extract lands at `extract_rel`.
fn extract_local(class_dir: &Path, input_rel: &str, extract_rel: &str, how: &Route) -> Result<()> {
    let source = class_dir.join(input_rel);
    let mut content = match how {
        // Streamed: the cap is known before the file is opened.
        Route::Csv => cap_csv(text_lines(&source, input_rel)?),
        Route::Html => strip_html(&read_text(&source, input_rel, MAX_LOCAL_BYTES)?),
        Route::Notebook => {
            crate::notebook::flatten(&read_text(&source, input_rel, MAX_LOCAL_BYTES)?)?
        }
        // A raw caption track is thousands of two-second fragments. What the
        // agent should search is the merged prose, not the timing grid.
        Route::Caption => {
            let text = read_text(&source, input_rel, MAX_LOCAL_BYTES)?;
            let name = Path::new(input_rel)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| input_rel.to_string());
            crate::transcripts::to_markdown(
                &crate::transcripts::parse(&text),
                &crate::transcripts::Meta {
                    title: name
                        .trim_end_matches(".vtt")
                        .trim_end_matches(".srt")
                        .trim_end_matches(".txt"),
                    date: "",
                    source_name: &name,
                },
            )
        }
        Route::Text => read_text(&source, input_rel, MAX_LOCAL_BYTES)?,
        // Named rather than caught by a catch-all, so a new local route has
        // to say what it does — and refused, so a caller that hands the
        // source of a converted format here gets an error rather than the
        // raw bytes recorded as its extract.
        Route::Pdf | Route::Pptx | Route::Docx | Route::Skip => {
            bail!("{input_rel} is not extracted locally as it is")
        }
    };
    content.truncate(content.trim_end().len());
    content.push('\n');

    let abs = class_dir.join(extract_rel);
    let parent = abs.parent().context("extract path has no parent")?;
    fs::create_dir_all(parent)?;
    crate::db::write_atomic(&abs, &content)?;
    Ok(())
}

/// Lines of a CSV kept in its extract. An extract exists to be searched — the
/// header and the shape of the rows are what a question about the data needs —
/// not to hold the dataset; the rest stays in the source. These are physical
/// lines, so a quoted field holding a newline can be cut mid-record at the
/// boundary; for an index whose note says where the source takes over, that
/// is acceptable.
const MAX_CSV_LINES: usize = 300;

fn cap_csv(lines: impl IntoIterator<Item = String>) -> String {
    let mut kept: Vec<String> = Vec::with_capacity(MAX_CSV_LINES);
    let mut total = 0usize;
    for line in lines {
        total += 1;
        if kept.len() < MAX_CSV_LINES {
            kept.push(line);
        }
    }
    let mut out = kept.join("\n");
    if total > MAX_CSV_LINES {
        out.push_str(&format!(
            "\n\n… {} more lines not extracted ({total} in the file, header included) — \
             read the source for the rest.",
            total - MAX_CSV_LINES
        ));
    }
    out
}

const SKIP_CONTENT_TAGS: &[&str] = &["script", "style", "noscript"];
const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "tr", "table", "thead", "tbody", "pre",
    "section", "article", "header", "footer", "blockquote", "hr", "h1", "h2",
    "h3", "h4", "h5", "h6",
];

/// Minimal tag-strip for R-rendered notebook HTML and LibreOffice's DOCX
/// export (SPEC §7 step 3), and for what Canvas serves as HTML — announcement
/// bodies, Pages and the syllabus page (SPEC §7.2), none of which is ever
/// rendered in the app: drops tags plus script/style payloads, decodes
/// common entities, keeps `<pre>` text intact, and collapses runs of blank
/// lines. Outside `<pre>` a newline in the text is source formatting — HTML
/// renders it as a space — and it is folded into one, because LibreOffice
/// hard-wraps every paragraph at about seventy characters and a phrase that
/// crossed the wrap would be unfindable by a line-based search. Zero tokens,
/// zero dependencies.
pub(crate) fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let mut rest = html;
    let mut pre_depth = 0usize;
    // A newline seen in prose, owed as one space before the next word.
    let mut soft_break = false;
    let emit = |text: &str, out: &mut String, in_pre: bool, soft_break: &mut bool| {
        let mut decoded = String::new();
        decode_entities(text, &mut decoded);
        if in_pre {
            out.push_str(&decoded);
            return;
        }
        for c in decoded.chars() {
            if c == '\n' || c == '\r' {
                *soft_break = true;
                continue;
            }
            if *soft_break {
                *soft_break = false;
                if !c.is_whitespace() && out.chars().last().is_some_and(|l| !l.is_whitespace()) {
                    out.push(' ');
                }
            }
            out.push(c);
        }
    };
    while let Some(open) = rest.find('<') {
        emit(&rest[..open], &mut out, pre_depth > 0, &mut soft_break);
        rest = &rest[open..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map(|i| &after[i + 3..]).unwrap_or("");
            continue;
        }
        let Some(close) = rest.find('>') else {
            break; // truncated tag at EOF
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if !tag.starts_with('/') && SKIP_CONTENT_TAGS.contains(&name.as_str()) {
            let closer = format!("</{name}");
            match find_ci(rest, &closer) {
                Some(i) => {
                    rest = &rest[i..];
                    rest = rest.find('>').map(|j| &rest[j + 1..]).unwrap_or("");
                }
                None => rest = "",
            }
            continue;
        }
        if name == "pre" {
            pre_depth = if tag.starts_with('/') { pre_depth.saturating_sub(1) } else { pre_depth + 1 };
        }
        if BLOCK_TAGS.contains(&name.as_str()) {
            out.push('\n');
            soft_break = false;
        } else if name == "td" || name == "th" {
            out.push(' ');
            soft_break = false;
        }
    }
    emit(rest, &mut out, pre_depth > 0, &mut soft_break);

    let mut result = String::with_capacity(out.len());
    let mut blanks = 0;
    for line in out.lines().map(str::trim_end) {
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 || result.is_empty() {
                continue;
            }
        } else {
            blanks = 0;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

/// Case-insensitive ASCII search; the needle is ASCII, so a byte match is
/// always at a char boundary.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle.as_bytes()))
}

fn decode_entities(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let entity_end = rest.find(';').filter(|&i| i <= 12);
        let decoded = entity_end.and_then(|end| {
            let entity = &rest[1..end];
            let c = match entity {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" | "#39" => Some('\''),
                "nbsp" => Some(' '),
                _ => entity.strip_prefix('#').and_then(|num| {
                    match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    }
                    .and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
}

// ---------------------------------------------------------------------------
// LibreOffice conversions (SPEC §7 step 2)

/// What headless LibreOffice is asked to make of a source, and the extension
/// its output carries in the extracts mirror: `<rel>.pptx.pdf`, `<rel>.docx.html`.
struct Conversion {
    /// soffice's `--convert-to` argument — an extension, or one with a filter
    /// and its options.
    convert_to: &'static str,
    ext: &'static str,
}

const PPTX_TO_PDF: Conversion = Conversion { convert_to: "pdf", ext: "pdf" };

/// `EmbedImages` keeps the export to one file. Without it Writer writes every
/// figure beside the HTML as a loose PNG — eleven of them, 2.9 MB, for the one
/// docx in the tree — and the stripper drops an `<img>` whichever way its
/// source is written, so nothing is lost by inlining that was kept by the
/// files.
const DOCX_TO_HTML: Conversion = Conversion {
    convert_to: "html:HTML (StarWriter):EmbedImages",
    ext: "html",
};

/// Every suffix the mirror can hold for one source — its extract, and each
/// conversion's output with its sidecar — so a source moved by the sorter
/// takes all of them along (SPEC §10 step 4). Kept beside the conversions
/// it is derived from; a test holds the two together.
pub const MIRROR_SUFFIXES: [&str; 5] = [".md", ".pdf", ".pdf.sha256", ".html", ".html.sha256"];

/// Where one of a source's mirror entries lives: the source's class-relative
/// path under the mirror root, with one of `MIRROR_SUFFIXES` appended. The
/// one place the shape is written, for the sorter's moves and the remover.
pub fn mirror_entry(class_dir: &Path, rel_path: &str, suffix: &str) -> PathBuf {
    class_dir.join(EXTRACTS_DIR).join(format!("{rel_path}{suffix}"))
}

/// Removes what the mirror holds for a source the index no longer does — its
/// extract and any conversion with its sidecar — and prunes the folders that
/// emptied, up to the mirror root, the way a corpus folder emptied of its
/// last note is pruned. Only a plain class-relative path is acted on, and
/// only at the entries the pipeline itself writes, so a row holding anything
/// else removes nothing. Best effort, after the scan's commit: a removal that
/// fails is logged, and the entry is dead weight rather than a wrong answer.
pub fn remove_mirror(class_dir: &Path, rel_path: &str) {
    let path = Path::new(rel_path);
    // Every component plain — the shape `corpus_note_path` and the sorter's
    // `clean_rel` each check for their own paths — and not the empty path,
    // for which `all` holds vacuously and whose "parent" is the root's.
    let plain = path
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !plain || path.as_os_str().is_empty() {
        return;
    }
    // The Canvas sync keeps the course's Pages under a folder of its own in
    // the mirror, with no `files` row behind them; a source folder of the
    // same name would mirror into it, and a vanished source there must not
    // take the professor's pages with it. Left as they are: the sync
    // reconciles that folder itself.
    if path.starts_with(crate::canvas_sync::CANVAS_TEXTS_DIR) {
        return;
    }
    let root = class_dir.join(EXTRACTS_DIR);
    for suffix in MIRROR_SUFFIXES {
        let entry = mirror_entry(class_dir, rel_path, suffix);
        if !entry.is_file() {
            continue;
        }
        if let Err(e) = fs::remove_file(&entry) {
            eprintln!("mirror entry removal failed ({}): {e}", entry.display());
        }
    }
    // `remove_dir` refuses a folder with anything left in it, which is the
    // whole check; the root itself stays, empty or not. A plain path's
    // parents all sit under the root, so reaching it is the only stop.
    let mut dir = root.join(rel_path);
    while let Some(parent) = dir.parent().map(Path::to_path_buf) {
        if parent == root || fs::remove_dir(&parent).is_err() {
            break;
        }
        dir = parent;
    }
}

/// Where a source's conversion lives in the mirror, and the sidecar that
/// records which source hash it was made from.
struct ConversionPaths {
    /// Class-relative, the form the batch payload and the callers keep.
    rel: String,
    abs: PathBuf,
    sidecar: PathBuf,
}

impl ConversionPaths {
    fn of(class_dir: &Path, rel_path: &str, how: &Conversion) -> Self {
        let rel = format!("{EXTRACTS_DIR}/{rel_path}.{}", how.ext);
        let abs = class_dir.join(&rel);
        let sidecar = class_dir.join(format!("{rel}.sha256"));
        Self { rel, abs, sidecar }
    }

    /// Whether the mirror already holds this source's conversion as it is
    /// now: the sidecar carries the hash it was converted from.
    fn is_current(&self, sha256: &str) -> bool {
        self.abs.is_file()
            && fs::read_to_string(&self.sidecar)
                .map(|s| s.trim() == sha256)
                .unwrap_or(false)
    }
}

/// Converts `<rel>` into the extracts mirror under `how`, skipping when the
/// existing output was produced from the current source hash (sidecar file).
/// Returns the output's class-relative path.
fn convert(
    app: &AppHandle,
    class_dir: &Path,
    rel_path: &str,
    sha256: &str,
    how: &Conversion,
) -> Result<String> {
    let out = ConversionPaths::of(class_dir, rel_path, how);
    if out.is_current(sha256) {
        return Ok(out.rel);
    }

    let out_dir = out.abs.parent().context("conversion path has no parent")?;
    fs::create_dir_all(out_dir)?;
    // A dedicated user profile keeps headless runs independent of any open
    // LibreOffice GUI instance (they otherwise refuse to start concurrently).
    let profile = crate::data_dir(app)?.join("soffice-profile");
    // UNO requires a valid URL; the app-data path contains a space
    // ("Application Support"), which unencoded aborts soffice with a
    // RuntimeException.
    let profile_url = format!("file://{}", profile.display()).replace(' ', "%20");
    // Bounded: headless soffice hanging on a stale profile lock is a known
    // failure mode, and this runs while the pipeline lock is held — an
    // unbounded wait would park every later scan's thread behind it and leave
    // extraction dead for the rest of the session.
    let child = Command::new(soffice_bin())
        .arg(format!("-env:UserInstallation={profile_url}"))
        .args(["--headless", "--convert-to", how.convert_to, "--outdir"])
        .arg(out_dir)
        .arg(class_dir.join(rel_path))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning soffice (is LibreOffice installed?)")?;
    let output = crate::jobs::wait_bounded(child, CONVERT_TIMEOUT).with_context(|| {
        format!(
            "soffice did not finish converting {rel_path} within {}s — it was stopped",
            CONVERT_TIMEOUT.as_secs()
        )
    })?;
    if !output.status.success() {
        bail!(
            "soffice exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // soffice names its output `<stem>.<ext>`; move it to `<name>.<source
    // ext>.<ext>` (§4). The stem keeps whatever the name had before its
    // extension, a trailing space included.
    let stem = Path::new(rel_path)
        .file_stem()
        .context("source has no file stem")?
        .to_string_lossy();
    let produced = out_dir.join(format!("{stem}.{}", how.ext));
    if !produced.is_file() {
        bail!(
            "soffice reported success but produced no {}: {}",
            produced.display(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    fs::rename(&produced, &out.abs)?;
    fs::write(&out.sidecar, sha256)?;
    Ok(out.rel)
}

/// SPEC §12: the absolute path the viewer's PDF frame loads through the asset
/// protocol — a PDF itself, or a deck's converted twin when the twin was made
/// from the deck as it is now. `None` is a deck with no current twin, which
/// the tree opens in its default app; an error is something that went wrong.
pub fn pdf_view_path(conn: &Connection, class_id: i64, rel_path: &str) -> Result<Option<PathBuf>> {
    let source = crate::scanner::resolve_rel(conn, class_id, rel_path)?;
    match crate::scanner::kind_for(&source) {
        "pdf" => Ok(Some(source)),
        "pptx" => {
            let sha256: String = conn
                .query_row(
                    "SELECT sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
                    params![class_id, rel_path],
                    |row| row.get(0),
                )
                .optional()?
                .context("not indexed yet — rescan the class")?;
            let class_dir = crate::scanner::class_dir(conn, class_id)?;
            let twin = ConversionPaths::of(&class_dir, rel_path, &PPTX_TO_PDF);
            Ok(twin.is_current(&sha256).then_some(twin.abs))
        }
        kind => bail!("a {kind} file has no PDF to show"),
    }
}

fn soffice_bin() -> PathBuf {
    let cask = PathBuf::from("/Applications/LibreOffice.app/Contents/MacOS/soffice");
    if cask.is_file() {
        cask
    } else {
        PathBuf::from("soffice") // fall back to PATH
    }
}

// ---------------------------------------------------------------------------
// Staleness (SPEC §7 step 5) — consumed by guide generation and the badges

/// One `{rel_path, sha256}` pair of a guide's `source_manifest` (SPEC §5).
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
#[derive(Debug)]
pub struct ManifestEntry {
    pub rel_path: String,
    pub sha256: String,
}

/// Current `{rel_path, sha256}` set for a guide scope: `'master'` covers the
/// whole class, anything else is a module rel-path prefix.
/// The prefix filter runs in SQL rather than over every row of the class:
/// `list_guides` calls this once per guide, `stale_guide_count` wraps
/// `list_guides`, and `db::list_classes` calls that per class — so filtering in
/// Rust made one dashboard refetch materialize every file row, once per guide,
/// once per class.
pub fn current_manifest(
    conn: &Connection,
    class_id: i64,
    scope: &str,
) -> Result<Vec<ManifestEntry>> {
    current_manifest_in(conn, class_id, scope, None)
}

/// What the scopes over the week folders read: the course's week slots and
/// the files filed under `Weeks/`. A listing loads them once for every row
/// (SPEC §8.5) rather than once per division, brief and pre-read.
pub struct ClassSources {
    pub slots: Vec<crate::units::WeekSlot>,
    pub filed: Vec<(ManifestEntry, Option<String>)>,
}

impl ClassSources {
    pub fn load(conn: &Connection, class_id: i64) -> Result<Self> {
        Ok(Self {
            slots: crate::units::week_slots(conn, class_id)?,
            filed: filed_under_weeks(conn, class_id)?,
        })
    }
}

/// `current_manifest` with the class's sources loaded once by the caller;
/// `None` loads them where a scope needs them.
pub fn current_manifest_in(
    conn: &Connection,
    class_id: i64,
    scope: &str,
    sources: Option<&ClassSources>,
) -> Result<Vec<ManifestEntry>> {
    let read = |rows: rusqlite::Rows<'_>| -> rusqlite::Result<Vec<ManifestEntry>> {
        rows.mapped(|row| {
            Ok(ManifestEntry {
                rel_path: row.get(0)?,
                sha256: row.get(1)?,
            })
        })
        .collect()
    };
    // A session digest's scope names one transcript rather than a folder, so
    // the prefix match below would find nothing and report every digest stale
    // forever. Its manifest is that single file.
    if let Some(transcript_rel) = scope.strip_prefix(crate::db::SESSION_SCOPE_PREFIX) {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
        )?;
        let rows = stmt.query(rusqlite::params![class_id, transcript_rel])?;
        return Ok(read(rows)?);
    }

    // A unit's sources come from three places that share no path prefix: the
    // folder holding its material, the week folders its weeks name, and the
    // lectures the calendar mapped to it (SPEC §8.5). The union is what makes
    // a unit guide go stale when its lecture or its deck changes — with either
    // left out nothing visibly breaks, the guide just quietly stops updating.
    // A practice exam's scope is its file (SPEC §8.3): no set of its own, so
    // its staleness is its manifest's entries resolved one by one.
    if crate::db::is_practice_scope(scope) {
        return Ok(Vec::new());
    }

    // The small documents (SPEC §8.6): a brief's set is its window's
    // divisions, the workbook's the project's files, a pre-read's the
    // division it precedes, a kit's the one paper.
    if let Some(deadline_id) = crate::db::brief_scope_id(scope) {
        return crate::briefs::window_manifest(conn, class_id, deadline_id, sources);
    }
    if scope == crate::db::PROJECT_SCOPE {
        return crate::workbook::project_manifest(conn, class_id);
    }
    if let Some(paper) = scope.strip_prefix(crate::db::KIT_SCOPE_PREFIX) {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
        )?;
        let rows = stmt.query(rusqlite::params![class_id, paper])?;
        return Ok(read(rows)?);
    }

    if scope.starts_with(crate::db::UNIT_SCOPE_PREFIX)
        || scope.starts_with(crate::db::PREREAD_SCOPE_PREFIX)
    {
        // A unit scope from before ids names no row, and neither does one
        // whose division is gone: no sources, rather than the whole class.
        let id = crate::db::unit_scope_id(scope).or_else(|| crate::db::preread_scope_id(scope));
        let unit: Option<(i64, Option<String>)> = match id {
            Some(unit_id) => conn
                .query_row(
                    "SELECT id, rel_path FROM units WHERE class_id = ?1 AND id = ?2",
                    rusqlite::params![class_id, unit_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?,
            None => None,
        };
        let Some((unit_id, unit_folder)) = unit else {
            return Ok(Vec::new());
        };
        let loaded;
        let sources = match sources {
            Some(sources) => sources,
            None => {
                loaded = ClassSources::load(conn, class_id)?;
                &loaded
            }
        };
        return unit_manifest(conn, class_id, unit_id, unit_folder.as_deref(), &sources.slots, &sources.filed);
    }

    // A duplicate is left out of every scope's set (SPEC §7 step 1): the
    // reading is read once, through its canonical copy.
    let entries = if scope == crate::db::MASTER_SCOPE {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files
             WHERE class_id = ?1 AND duplicate_of IS NULL ORDER BY rel_path",
        )?;
        let rows = stmt.query([class_id])?;
        read(rows)?
    } else {
        folder_manifest(conn, class_id, scope)?
    };
    Ok(entries)
}

/// Everything indexed at or under one folder, as a scope's own set: a
/// duplicate whose canonical copy sits in the same folder is left out — the
/// reading is read once per scope (SPEC §7 step 1) — while one whose
/// canonical sits elsewhere is this scope's only copy and stays.
fn folder_manifest(conn: &Connection, class_id: i64, folder: &str) -> Result<Vec<ManifestEntry>> {
    let entries = folder_rows(conn, class_id, folder)?;
    Ok(once_per_scope(entries))
}

/// Every indexed row at or under one folder, duplicates included, with the
/// canonical copy's path where the row is one.
fn folder_rows(conn: &Connection, class_id: i64, folder: &str) -> Result<Vec<(ManifestEntry, Option<String>)>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, sha256, duplicate_of FROM files
         WHERE class_id = ?1 AND (rel_path = ?2 OR rel_path LIKE ?3 ESCAPE '\\')
         ORDER BY rel_path",
    )?;
    // The separator is appended before matching, so `Module 1` cannot capture
    // `Module 10`; LIKE wildcards in a folder name are escaped.
    let prefix = format!(
        "{}/%",
        folder.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let rows = stmt.query(rusqlite::params![class_id, folder, prefix])?;
    Ok(rows
        .mapped(|row| {
            Ok((
                ManifestEntry {
                    rel_path: row.get(0)?,
                    sha256: row.get(1)?,
                },
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A scope's set with each reading once: a duplicate is dropped only where
/// its canonical copy is in the same set.
fn once_per_scope(entries: Vec<(ManifestEntry, Option<String>)>) -> Vec<ManifestEntry> {
    let present: std::collections::HashSet<&str> =
        entries.iter().map(|(entry, _)| entry.rel_path.as_str()).collect();
    let keep: Vec<bool> = entries
        .iter()
        .map(|(_, canonical)| canonical.as_deref().is_none_or(|c| !present.contains(c)))
        .collect();
    entries
        .into_iter()
        .zip(keep)
        .filter_map(|((entry, _), keep)| keep.then_some(entry))
        .collect()
}

/// Everything indexed under `Weeks/`, duplicates included with their
/// canonical copies, read once for a listing that asks about every division
/// of a class; each division settles its own copies (`unit_manifest`).
pub fn filed_under_weeks(conn: &Connection, class_id: i64) -> Result<Vec<(ManifestEntry, Option<String>)>> {
    folder_rows(conn, class_id, crate::db::WEEKS_DIR)
}

/// A division's manifest (SPEC §8.5): its folder, the week folders its weeks
/// name, and the lectures mapped to it. What is filed under a week folder is
/// that week's material, and the week feeds the division (SPEC §4) — the join
/// a transcript makes, made for the deck and the notebook filed beside it,
/// read off the folder's number the way `week_from_rel_path` reads a
/// transcript's, so a week the syllabus renames still counts the folder it
/// was filed under. The slots and the `Weeks/` listing come from the caller,
/// so a listing of fifteen divisions reads them once rather than once per row.
pub fn unit_manifest(
    conn: &Connection,
    class_id: i64,
    unit_id: i64,
    unit_folder: Option<&str>,
    slots: &[crate::units::WeekSlot],
    filed: &[(ManifestEntry, Option<String>)],
) -> Result<Vec<ManifestEntry>> {
    let mut entries = match unit_folder {
        Some(folder) => folder_rows(conn, class_id, folder)?,
        None => Vec::new(),
    };
    let weeks: std::collections::BTreeSet<i64> = slots
        .iter()
        .filter(|slot| slot.unit_id == unit_id)
        .map(|slot| slot.week)
        .collect();
    if !weeks.is_empty() {
        entries.extend(
            filed
                .iter()
                .filter(|(entry, _)| {
                    crate::units::week_from_rel_path(&entry.rel_path)
                        .is_some_and(|week| weeks.contains(&week))
                })
                .cloned(),
        );
    }
    let mut stmt = conn.prepare(
        "SELECT f.rel_path, f.sha256, f.duplicate_of FROM files f
         JOIN lecture_contributions lc
           ON lc.class_id = f.class_id AND lc.rel_path = f.rel_path
         WHERE f.class_id = ?1 AND lc.unit_id = ?2 AND lc.status = 'applied'",
    )?;
    let mapped = stmt
        .query_map(rusqlite::params![class_id, unit_id], |row| {
            Ok((
                ManifestEntry {
                    rel_path: row.get(0)?,
                    sha256: row.get(1)?,
                },
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    entries.extend(mapped);
    // A transcript is under its week folder and on a contribution row, so it
    // arrives twice, and a manifest that holds a duplicate never equals the
    // set it is compared against. A copy of a reading another division also
    // holds is this one's only copy, and stays (`once_per_scope`).
    entries.sort_by(|a, b| a.0.rel_path.cmp(&b.0.rel_path));
    entries.dedup_by(|a, b| a.0.rel_path == b.0.rel_path);
    Ok(once_per_scope(entries))
}


/// What separates a guide's stored manifest from its sources as they are now
/// (SPEC §7 step 5): the scope's entries the manifest never named, the named
/// entries whose hash moved, and the named entries that are gone. The three
/// lists are what the row's `Rewrite · 2 files added, 1 changed` reads, and
/// what a rewrite's prompt is told; `is_stale` is their union being non-empty.
#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManifestDiff {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
    /// The stored manifest could not be read: stale whatever the lists hold,
    /// since a scope whose current set is empty would otherwise read fresh.
    pub unreadable: bool,
}

impl ManifestDiff {
    pub fn is_stale(&self) -> bool {
        self.unreadable
            || !(self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty())
    }
}

/// The diff between a stored manifest and the scope's current set. A stored
/// entry the set does not name is looked up through `resolve`, which answers
/// with its current hash — the index's for a file row, the file's on disk for
/// a corpus note or a Canvas page mirror — or `None` when it is gone; that is
/// what lets a manifest widen past its scope (SPEC §7 step 5) without reading
/// as stale forever. An unreadable manifest is stale over every current entry.
pub fn manifest_diff(
    stored_manifest_json: &str,
    current: &[ManifestEntry],
    resolve: impl Fn(&str) -> Option<String>,
) -> ManifestDiff {
    let (stored, unreadable) = match serde_json::from_str::<Vec<ManifestEntry>>(stored_manifest_json) {
        Ok(stored) => (stored, false),
        Err(_) => (Vec::new(), true),
    };
    let current_by_path: std::collections::HashMap<&str, &str> = current
        .iter()
        .map(|e| (e.rel_path.as_str(), e.sha256.as_str()))
        .collect();
    let stored_paths: std::collections::HashSet<&str> =
        stored.iter().map(|e| e.rel_path.as_str()).collect();
    let mut diff = ManifestDiff { unreadable, ..ManifestDiff::default() };
    for entry in current {
        if !stored_paths.contains(entry.rel_path.as_str()) {
            diff.added.push(entry.rel_path.clone());
        }
    }
    for entry in &stored {
        match current_by_path.get(entry.rel_path.as_str()) {
            Some(hash) => {
                if *hash != entry.sha256 {
                    diff.changed.push(entry.rel_path.clone());
                }
            }
            None => match resolve(&entry.rel_path) {
                Some(hash) if hash == entry.sha256 => {}
                Some(_) => diff.changed.push(entry.rel_path.clone()),
                None => diff.removed.push(entry.rel_path.clone()),
            },
        }
    }
    for list in [&mut diff.added, &mut diff.changed, &mut diff.removed] {
        list.sort();
        list.dedup();
    }
    diff
}

/// Whether a path outside the file index still counts as a source a manifest
/// may name: a corpus note, a Canvas page mirror, one of the reader's notes.
/// Everything else outside the index — a guide, an inbox file, a job's own
/// output — is never a source.
fn hashed_from_disk(rel_path: &str) -> bool {
    rel_path.starts_with(&format!("{}/", crate::db::CORPUS_DIR))
        || rel_path.starts_with(&format!("{}/Canvas/", crate::db::EXTRACTS_DIR))
        || rel_path.starts_with(&format!("{}/", crate::db::NOTES_DIR))
}

/// The current hash of a manifest entry: the index's for a file row, the
/// file's own for a path the index does not hold and `hashed_from_disk`
/// admits, `None` for a path that is gone or was never a source.
pub fn current_hash(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    rel_path: &str,
) -> Result<Option<String>> {
    let indexed: Option<String> = conn
        .query_row(
            "SELECT sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
            rusqlite::params![class_id, rel_path],
            |row| row.get(0),
        )
        .optional()?;
    if indexed.is_some() {
        return Ok(indexed);
    }
    if !hashed_from_disk(rel_path) || !plain_relative(rel_path) {
        return Ok(None);
    }
    let abs = class_dir.join(rel_path);
    if !abs.is_file() {
        return Ok(None);
    }
    Ok(Some(crate::scanner::hash_file(&abs)?))
}

/// Every component ordinary and the path relative: what a manifest may name.
fn plain_relative(rel_path: &str) -> bool {
    !rel_path.is_empty()
        && Path::new(rel_path)
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
}

/// The manifest entry a job's read of `rel_path` stands for (SPEC §7 step 5):
/// the file row it names, or the row behind the extract or converted twin it
/// opened, with the index's hash; a corpus note, a Canvas page mirror or a
/// note with the hash of the file on disk; nothing for anything else — its own
/// output under `Study Guides/`, an inbox file, a path that is no source.
pub fn source_of(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    rel_path: &str,
) -> Result<Option<ManifestEntry>> {
    if !plain_relative(rel_path) {
        return Ok(None);
    }
    let row = |sql: &str, key: &str| -> Result<Option<ManifestEntry>> {
        Ok(conn
            .query_row(sql, rusqlite::params![class_id, key], |row| {
                Ok(ManifestEntry {
                    rel_path: row.get(0)?,
                    sha256: row.get(1)?,
                })
            })
            .optional()?)
    };
    if let Some(entry) = row(
        "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
        rel_path,
    )? {
        return Ok(Some(entry));
    }
    if let Some(entry) = row(
        "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 AND extract_rel_path = ?2",
        rel_path,
    )? {
        return Ok(Some(entry));
    }
    let extracts = format!("{}/", crate::db::EXTRACTS_DIR);
    if let Some(mirrored) = rel_path.strip_prefix(&extracts) {
        // A converted twin or an extract whose row has yet to record it:
        // strip the mirror suffix and the source is what is left.
        for suffix in MIRROR_SUFFIXES {
            if let Some(source) = mirrored.strip_suffix(suffix) {
                if let Some(entry) = row(
                    "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 AND rel_path = ?2",
                    source,
                )? {
                    return Ok(Some(entry));
                }
            }
        }
    }
    if !hashed_from_disk(rel_path) {
        return Ok(None);
    }
    let abs = class_dir.join(rel_path);
    if !abs.is_file() {
        return Ok(None);
    }
    Ok(Some(ManifestEntry {
        rel_path: rel_path.to_string(),
        sha256: crate::scanner::hash_file(&abs)?,
    }))
}

/// The manifest a finished job records (SPEC §7 step 5): what it was told
/// about, at the hashes captured when it was enqueued, widened by what its own
/// log shows it read. A listed source the job never opened stays — it was
/// told about it, and a change to it is still a reason to rebuild — so the
/// union only widens; a read the listing already names keeps the listed hash.
pub fn union_manifest(
    conn: &Connection,
    class_id: i64,
    class_dir: &Path,
    listed: Vec<ManifestEntry>,
    read: &std::collections::BTreeSet<String>,
) -> Result<Vec<ManifestEntry>> {
    let mut entries = listed;
    let mut named: std::collections::HashSet<String> =
        entries.iter().map(|e| e.rel_path.clone()).collect();
    for rel_path in read {
        if named.contains(rel_path) {
            continue;
        }
        if let Some(entry) = source_of(conn, class_id, class_dir, rel_path)? {
            if named.insert(entry.rel_path.clone()) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::{cap_csv, current_manifest, pdf_view_path, strip_html, MAX_CSV_LINES};

    /// A CSV's extract is for search, not for holding the dataset: past the
    /// cap the rows stay in the source and the note says how many.
    #[test]
    fn a_csv_past_the_cap_keeps_its_head_and_names_the_rest() {
        let mut text = String::from("id,age,outcome\n");
        for i in 1..=1_000 {
            text.push_str(&format!("{i},{},{}\n", 20 + i % 60, i % 2));
        }
        let capped = cap_csv(text.lines().map(String::from));
        let lines: Vec<&str> = capped.lines().collect();
        assert_eq!(lines[0], "id,age,outcome");
        assert_eq!(lines[MAX_CSV_LINES - 1], "299,79,1");
        assert!(lines[MAX_CSV_LINES].is_empty());
        assert_eq!(
            lines[MAX_CSV_LINES + 1],
            "… 701 more lines not extracted (1001 in the file, header included) — read the source for the rest."
        );
        assert!(!capped.contains("\n300,"), "{capped}");
    }

    /// The cap on a whole-file read is what keeps one oversized notebook from
    /// taking the pipeline thread with it; a CSV never goes through it.
    #[test]
    fn a_source_past_the_local_cap_is_refused_and_a_csv_is_streamed() {
        let dir = std::env::temp_dir().join(format!("classhub-local-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let big = dir.join("big.py");
        std::fs::write(&big, "x = 1\r\n".repeat(4)).unwrap();
        let err = super::read_text(&big, "big.py", 10).unwrap_err();
        assert!(err.to_string().contains("too large"), "{err:#}");
        assert_eq!(super::read_text(&big, "big.py", 1024).unwrap(), "x = 1\n".repeat(4));

        let csv = dir.join("data.csv");
        std::fs::write(&csv, "a,b\r\n1,2\r\n3,4\r\n").unwrap();
        let lines: Vec<String> = super::text_lines(&csv, "data.csv").unwrap().collect();
        assert_eq!(lines, ["a,b", "1,2", "3,4"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one table deciding whether a kind is extracted at all, and how; a
    /// kind that falls to Skip here silently never gets an extract.
    #[test]
    fn every_kind_takes_its_route() {
        use super::{route, Route};
        for (kind, expected) in [
            ("rmd", Route::Text),
            ("r", Route::Text),
            ("md", Route::Text),
            ("py", Route::Text),
            ("csv", Route::Csv),
            ("html", Route::Html),
            ("caption", Route::Caption),
            ("ipynb", Route::Notebook),
            ("pdf", Route::Pdf),
            ("pptx", Route::Pptx),
            ("docx", Route::Docx),
            ("media", Route::Skip),
            ("other", Route::Skip),
        ] {
            assert_eq!(route("Module 1/file", kind), expected, "{kind}");
        }
        // Zoom's saved transcript is a caption track with a .txt extension.
        assert_eq!(route("Weeks/Week 02/transcript.txt", "other"), Route::Caption);
        assert_eq!(route("Module 1/notes.TXT", "other"), Route::Caption);
    }

    /// The sorter moves a source's whole mirror by this list; a conversion
    /// added without its suffixes here would leave its twin orphaned.
    #[test]
    fn the_mirror_suffixes_cover_every_conversion() {
        for how in [&super::PPTX_TO_PDF, &super::DOCX_TO_HTML] {
            let twin = format!(".{}", how.ext);
            let sidecar = format!(".{}.sha256", how.ext);
            assert!(super::MIRROR_SUFFIXES.contains(&twin.as_str()), "{twin}");
            assert!(super::MIRROR_SUFFIXES.contains(&sidecar.as_str()), "{sidecar}");
        }
        assert!(super::MIRROR_SUFFIXES.contains(&".md"));
    }

    /// An extract recorded for a source whose row is gone — deleted while the
    /// pipeline was writing it — is removed rather than left as an orphan;
    /// one whose row is there is recorded and kept.
    #[test]
    fn a_record_no_row_takes_removes_the_extract_it_was_for() {
        let conn = crate::db::memory_db();
        let class_dir =
            std::env::temp_dir().join(format!("classhub-record-orphan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&class_dir);
        let extract_rel = ".classhub/extracts/Weeks/Week 04/deck.pdf.md";
        let extract = class_dir.join(extract_rel);
        std::fs::create_dir_all(extract.parent().expect("parent")).expect("dir");
        std::fs::write(&extract, "# extract").expect("write");

        super::record(&conn, &class_dir, 4, "Weeks/Week 04/deck.pdf", extract_rel, "abc")
            .expect("record");
        assert!(!extract.exists(), "an extract with no row behind it stayed");

        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (4, 'Weeks/Week 04/deck.pdf', 'abc', 1, 1, 'pdf')",
            [],
        )
        .expect("row");
        std::fs::create_dir_all(extract.parent().expect("parent")).expect("dir");
        std::fs::write(&extract, "# extract").expect("write");
        super::record(&conn, &class_dir, 4, "Weeks/Week 04/deck.pdf", extract_rel, "abc")
            .expect("record");
        let recorded: Option<String> = conn
            .query_row("SELECT extract_rel_path FROM files WHERE class_id = 4", [], |row| row.get(0))
            .expect("row");
        assert_eq!(recorded.as_deref(), Some(extract_rel));
        assert!(extract.is_file(), "a recorded extract went");
        let _ = std::fs::remove_dir_all(&class_dir);
    }

    /// `remove_mirror` deletes only under the mirror and only what the
    /// pipeline writes there: a plain class-relative path's entries and the
    /// folders they emptied — never the root, never a folder a sibling still
    /// uses, and nothing at all for a path that is not plain.
    #[test]
    fn the_mirror_is_removed_only_at_a_plain_path_under_the_root() {
        let class_dir =
            std::env::temp_dir().join(format!("classhub-remove-mirror-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&class_dir);
        let root = class_dir.join(super::EXTRACTS_DIR);
        let write = |rel: &str| {
            let path = class_dir.join(rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            std::fs::write(&path, "made from a source").expect("write");
            path
        };

        // A sibling's extract keeps the folder both share.
        let a = write(".classhub/extracts/Weeks/Week 04/a.pdf.md");
        let b = write(".classhub/extracts/Weeks/Week 04/b.pdf.md");
        super::remove_mirror(&class_dir, "Weeks/Week 04/a.pdf");
        assert!(!a.exists() && b.is_file(), "a sibling's entry went");
        assert!(root.join("Weeks/Week 04").is_dir(), "a folder still in use was pruned");
        // The last entry takes the folders it emptied with it, up to the root.
        super::remove_mirror(&class_dir, "Weeks/Week 04/b.pdf");
        assert!(!root.join("Weeks").exists() && root.is_dir());

        // A top-level source's parent is the root, which stays.
        let top = write(".classhub/extracts/syllabus.pdf.md");
        super::remove_mirror(&class_dir, "syllabus.pdf");
        assert!(!top.exists() && root.is_dir(), "the root was pruned");

        // Nothing but a plain relative path is acted on.
        let inside = write(".classhub/extracts/keep.md");
        let outside = write("keep.md");
        for rel in ["", "../../keep", "./keep", "/keep", "Weeks/../keep"] {
            super::remove_mirror(&class_dir, rel);
            assert!(inside.is_file() && outside.is_file(), "{rel:?} removed something");
        }

        // The sync's Canvas texts are nobody's extract, whatever a source
        // folder is called.
        let page = write(".classhub/extracts/Canvas/Home.md");
        super::remove_mirror(&class_dir, "Canvas/Home");
        assert!(page.is_file(), "a Canvas page went with a source");
        let _ = std::fs::remove_dir_all(&class_dir);
    }

    #[test]
    fn a_csv_within_the_cap_is_kept_whole() {
        let lines = ["a,b", "1,2", "3,4"].map(String::from);
        assert_eq!(cap_csv(lines), "a,b\n1,2\n3,4");
    }

    /// What the viewer's PDF frame is given: the PDF, or a deck's twin only
    /// while the twin was made from the deck as it is now.
    #[test]
    fn the_pdf_view_path_is_the_pdf_or_a_current_twin() {
        let root = std::env::temp_dir().join(format!("classhub-pdf-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let class_dir = root.join("Biostatistics for AI");
        std::fs::create_dir_all(class_dir.join(".classhub/extracts/Slides")).unwrap();
        std::fs::create_dir_all(class_dir.join("Slides")).unwrap();
        std::fs::write(class_dir.join("Slides/deck.pptx"), b"pptx").unwrap();
        std::fs::write(class_dir.join("Slides/paper.pdf"), b"pdf").unwrap();
        std::fs::write(class_dir.join("Slides/notes.md"), b"md").unwrap();

        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let class_id: i64 = conn
            .query_row(
                "SELECT id FROM classes WHERE folder_name = 'Biostatistics for AI'",
                [],
                |row| row.get(0),
            )
            .expect("seeded class");
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (?1, 'Slides/deck.pptx', 'abc', 4, 1, 'pptx')",
            [class_id],
        )
        .expect("file");

        assert_eq!(
            pdf_view_path(&conn, class_id, "Slides/paper.pdf").expect("pdf"),
            Some(class_dir.join("Slides/paper.pdf"))
        );
        // No twin yet is not an error: the tree opens the deck elsewhere.
        assert_eq!(pdf_view_path(&conn, class_id, "Slides/deck.pptx").expect("deck"), None);

        let twin = class_dir.join(".classhub/extracts/Slides/deck.pptx.pdf");
        std::fs::write(&twin, b"pdf").unwrap();
        std::fs::write(class_dir.join(".classhub/extracts/Slides/deck.pptx.pdf.sha256"), "abc").unwrap();
        assert_eq!(pdf_view_path(&conn, class_id, "Slides/deck.pptx").expect("twin"), Some(twin));

        // The deck changed since the twin was made: back to the default app
        // until the next scan converts it again.
        conn.execute("UPDATE files SET sha256 = 'abd' WHERE rel_path = 'Slides/deck.pptx'", [])
            .expect("edit");
        assert_eq!(pdf_view_path(&conn, class_id, "Slides/deck.pptx").expect("stale twin"), None);

        // Something wrong, as against something not there, is an error.
        let err = pdf_view_path(&conn, class_id, "Slides/notes.md").unwrap_err();
        assert!(err.to_string().contains("no PDF"), "{err:#}");
        assert!(pdf_view_path(&conn, class_id, "../etc/passwd").is_err());
        std::fs::write(class_dir.join("Slides/loose.pptx"), b"pptx").unwrap();
        let err = pdf_view_path(&conn, class_id, "Slides/loose.pptx").unwrap_err();
        assert!(err.to_string().contains("not indexed"), "{err:#}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// SPEC §8.5's least visible requirement. A unit's sources come from a
    /// folder and from lectures that share no path prefix with it, so a
    /// prefix-only manifest holds the folder and misses every transcript — and
    /// nothing breaks visibly: the guide just quietly stops going stale when
    /// its lecture changes.
    #[test]
    fn a_unit_s_manifest_unions_its_folder_with_its_lectures() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, rel_path, source)
             VALUES (7, 1, 3, 'week', 'Week 3 — Transformers', 'Module 1', 'syllabus')",
            [],
        )
        .expect("unit");
        let transcript = "Weeks/Week 03 — Transformers/2026-09-10 — Lecture.md";
        for (rel_path, sha) in [
            ("Module 1/Slides/deck.pdf", "aaa"),
            (transcript, "bbb"),
            ("Module 2/Slides/other.pdf", "ccc"),
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (1, ?1, ?2, 1, 1, 'pdf')",
                rusqlite::params![rel_path, sha],
            )
            .expect("file");
        }
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (1, 7, ?1, 0, 0, 1, 1, 'x.md', 's', 'high', 'applied', 0)",
            [transcript],
        )
        .expect("contribution");

        let scope = crate::db::unit_scope(7);
        let manifest = current_manifest(&conn, 1, &scope).expect("manifest");
        let paths: Vec<&str> = manifest.iter().map(|e| e.rel_path.as_str()).collect();
        assert_eq!(paths, ["Module 1/Slides/deck.pdf", transcript]);

        // Editing the transcript is what a regenerated guide has to notice.
        let stored = serde_json::to_string(&manifest).expect("json");
        conn.execute(
            "UPDATE files SET sha256 = 'bbb2' WHERE rel_path = ?1",
            [transcript],
        )
        .expect("edit");
        let after = current_manifest(&conn, 1, &scope).expect("manifest");
        assert!(super::manifest_diff(&stored, &after, |_| None).is_stale(), "the union missed the lecture");
    }

    /// SPEC §4/§8.5: what is filed under a week folder is that week's
    /// material, and the week feeds a division. A Part's manifest covers the
    /// folders of its own weeks and not the next Part's; a week-numbered
    /// course's covers its own folder alone, read by the folder's number so a
    /// week renamed since its folder was made still counts it; and a corrected
    /// deck is what a regenerated guide has to notice. Nothing outside
    /// `Weeks/` arrives this way.
    #[test]
    fn a_unit_s_manifest_covers_the_week_folders_its_weeks_name() {
        let conn = crate::db::memory_db();
        // Applied Generative AI's shape: Parts over ranges, no week rows.
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (37, 4, 1, 'part', 'Part I', 1, 1, 8, 'syllabus'),
                    (38, 4, 2, 'part', 'Part II', 2, 9, 12, 'syllabus')",
            [],
        )
        .expect("parts");
        // Fundamentals' shape: numbered weeks, one renamed since its folder was made.
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, number, source)
             VALUES (24, 1, 2, 'week', 'Week 2 — Responsible AI', 2, 'syllabus'),
                    (25, 1, 3, 'week', 'Week 3 — Data', 3, 'syllabus')",
            [],
        )
        .expect("weeks");
        let transcript = "Weeks/Week 02/2026-09-01 — Lecture.md";
        for (class_id, rel_path, sha) in [
            (4, "Weeks/Week 01/deck.pdf", "a"),
            (4, transcript, "b"),
            (4, "Weeks/Week 02/notebook.ipynb", "c"),
            (4, "Weeks/Week 09/part-two.pdf", "d"),
            (4, "Slides/loose.pdf", "e"),
            (1, "Weeks/Week 02 — Responsible AI, Ethics/deck.pptx", "f"),
            (1, "Weeks/Week 03 — Data/other.pdf", "g"),
            // Under `Weeks/` but not in a week folder: nobody's.
            (4, "Weeks/Midterm Review/questions.pdf", "h"),
            (4, "Weeks/loose.pdf", "i"),
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (?1, ?2, ?3, 1, 1, 'pdf')",
                rusqlite::params![class_id, rel_path, sha],
            )
            .expect("file");
        }
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (4, 37, ?1, 0, 0, 1, 1, 'x.md', 's', 'high', 'applied', 0)",
            [transcript],
        )
        .expect("contribution");

        let paths = |class_id: i64, unit_id: i64| -> Vec<String> {
            current_manifest(&conn, class_id, &crate::db::unit_scope(unit_id))
                .expect("manifest")
                .into_iter()
                .map(|e| e.rel_path)
                .collect()
        };
        assert_eq!(
            paths(4, 37),
            ["Weeks/Week 01/deck.pdf", transcript, "Weeks/Week 02/notebook.ipynb"]
        );
        assert_eq!(paths(4, 38), ["Weeks/Week 09/part-two.pdf"]);
        assert_eq!(paths(1, 24), ["Weeks/Week 02 — Responsible AI, Ethics/deck.pptx"]);

        let stored = serde_json::to_string(
            &current_manifest(&conn, 4, &crate::db::unit_scope(37)).expect("manifest"),
        )
        .expect("json");
        conn.execute("UPDATE files SET sha256 = 'a2' WHERE rel_path = 'Weeks/Week 01/deck.pdf'", [])
            .expect("edit");
        let after = current_manifest(&conn, 4, &crate::db::unit_scope(37)).expect("manifest");
        assert!(super::manifest_diff(&stored, &after, |_| None).is_stale(), "the week folder's deck was missed");
    }

    /// A division nothing declares any more has no sources, rather than
    /// silently taking the whole class.
    #[test]
    fn a_unit_no_longer_declared_has_an_empty_manifest() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (1, 'Module 1/x.pdf', 'a', 1, 1, 'pdf')",
            [],
        )
        .expect("file");
        assert!(current_manifest(&conn, 1, &crate::db::unit_scope(9)).expect("manifest").is_empty());
        // A scope from before ids names no row either.
        let legacy = format!("{}Week 9", crate::db::UNIT_SCOPE_PREFIX);
        assert!(current_manifest(&conn, 1, &legacy).expect("manifest").is_empty());
    }

    /// Class HTML notebooks are extracted locally, so this parser is what
    /// stands between their markup and the text the agent later searches.
    #[test]
    fn keeps_text_and_drops_tags() {
        assert_eq!(strip_html("<p>Hello <b>world</b></p>").trim(), "Hello world");
    }

    /// LibreOffice hard-wraps every paragraph of a DOCX export; the phrase
    /// across the wrap has to land on one line or search never finds it.
    /// Inside `<pre>` the newlines are the content and stay.
    #[test]
    fn folds_soft_line_breaks_in_prose_but_not_in_pre() {
        let html = "<p>Introducing Navigator\nToolkit <b>API</b>\nKey to\r\n<span>Posit</span> </p>\n<pre>x &lt;- 1\ny &lt;- 2</pre>";
        assert_eq!(
            strip_html(html),
            "Introducing Navigator Toolkit API Key to Posit\n\nx <- 1\ny <- 2\n"
        );
    }

    #[test]
    fn drops_script_and_style_content_entirely() {
        let html = "<p>before</p><script>var x = 1 < 2;</script><p>after</p>";
        let out = strip_html(html);
        assert!(!out.contains("var x"), "script body leaked: {out:?}");
        assert!(out.contains("before") && out.contains("after"));

        let styled = "<style>.a { color: red }</style><p>body</p>";
        let out = strip_html(styled);
        assert!(!out.contains("color"), "style body leaked: {out:?}");
        assert!(out.contains("body"));
    }

    #[test]
    fn drops_comments() {
        let out = strip_html("<p>a</p><!-- hidden note --><p>b</p>");
        assert!(!out.contains("hidden"), "comment leaked: {out:?}");
    }

    #[test]
    fn decodes_entities() {
        let out = strip_html("<p>a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &nbsp;f</p>");
        assert!(out.contains("a & b"), "{out:?}");
        assert!(out.contains("<c>"), "{out:?}");
        assert!(out.contains("\"d\""), "{out:?}");
    }

    #[test]
    fn survives_a_truncated_tag_at_eof() {
        // No panic and no infinite loop: a half-written file is a real input.
        let _ = strip_html("<p>text</p><div class=\"unclosed");
        let _ = strip_html("<!-- unterminated comment");
        let _ = strip_html("<script>never closed");
    }

    #[test]
    fn matches_a_closing_tag_case_insensitively() {
        let out = strip_html("<p>a</p><SCRIPT>junk</SCRIPT><p>b</p>");
        assert!(!out.contains("junk"), "{out:?}");
        assert!(out.contains("b"), "{out:?}");
    }
    /// The diff behind `stale` (SPEC §7 step 5): what the scope now holds that
    /// the manifest never named, what it named whose hash moved — resolved
    /// through the index or the disk for an entry outside the scope's own set
    /// — and what it named that is gone. Equal sets are fresh; an unreadable
    /// manifest is stale over everything.
    #[test]
    fn the_manifest_diff_names_what_was_added_changed_and_removed() {
        use super::{manifest_diff, ManifestEntry};
        let entry = |p: &str, h: &str| ManifestEntry { rel_path: p.into(), sha256: h.into() };
        let stored = serde_json::to_string(&[
            entry("a.pdf", "1"),
            entry("b.Rmd", "2"),
            entry(".classhub/corpus/W/n.md", "n1"),
            entry("gone.pdf", "g"),
        ])
        .unwrap();
        let current = [entry("a.pdf", "1"), entry("b.Rmd", "2x"), entry("c.pptx", "3")];
        let diff = manifest_diff(&stored, &current, |path| match path {
            ".classhub/corpus/W/n.md" => Some("n2".to_string()),
            _ => None,
        });
        assert_eq!(diff.added, vec!["c.pptx"]);
        assert_eq!(diff.changed, vec![".classhub/corpus/W/n.md", "b.Rmd"]);
        assert_eq!(diff.removed, vec!["gone.pdf"]);
        assert!(diff.is_stale());

        let same = manifest_diff(&serde_json::to_string(&current).unwrap(), &current, |_| None);
        assert!(!same.is_stale(), "{same:?}");
        let unread = manifest_diff("not json", &current, |_| None);
        assert_eq!(unread.added.len(), 3);
        assert!(unread.unreadable && unread.is_stale());
        // A scope with no set of its own — an exam's — still reads stale over
        // a manifest it cannot read.
        assert!(manifest_diff("not json", &[], |_| None).is_stale());
        assert!(!manifest_diff("[]", &[], |_| None).is_stale());
    }

    /// The union a finished job records (SPEC §7 step 5): a read outside the
    /// listed sources joins — an extract as the source row it mirrors, with the
    /// index's hash; a corpus note as itself, hashed from disk — a listed
    /// source never read stays at its listed hash even when the index has
    /// moved on, and the job's own output and an inbox file never join.
    #[test]
    fn the_manifest_union_widens_by_what_the_log_read_and_keeps_what_was_listed() {
        use super::{current_hash, union_manifest, ManifestEntry};
        use std::fs;
        let conn = crate::db::memory_db();
        let root = std::env::temp_dir().join(format!("classhub-union-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let class_dir = root.join("Biostatistics for AI");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        for (rel, sha, extract) in [
            ("Slides/deck.pptx", "d1", Some(".classhub/extracts/Slides/deck.pptx.md")),
            ("Reading/p.pdf", "p1", None),
            ("Weeks/Week 03/L.md", "t2", None),
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind, extract_rel_path)
                 VALUES (3, ?1, ?2, 1, 1, 'other', ?3)",
                rusqlite::params![rel, sha, extract],
            )
            .expect("file");
        }
        let note = class_dir.join(".classhub/corpus/Week 3/n.md");
        fs::create_dir_all(note.parent().unwrap()).unwrap();
        fs::write(&note, "# distilled").unwrap();
        let note_hash = crate::scanner::hash_file(&note).unwrap();

        let listed = vec![ManifestEntry { rel_path: "Weeks/Week 03/L.md".into(), sha256: "t1".into() }];
        let read: std::collections::BTreeSet<String> = [
            ".classhub/extracts/Slides/deck.pptx.md",
            ".classhub/extracts/Reading/p.pdf.md",
            ".classhub/corpus/Week 3/n.md",
            "Weeks/Week 03/L.md",
            "Study Guides/Week 3.html",
            "_Inbox/x.pdf",
            "../outside.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let union = union_manifest(&conn, 3, &class_dir, listed, &read).expect("union");
        let pairs: Vec<(String, String)> =
            union.iter().map(|e| (e.rel_path.clone(), e.sha256.clone())).collect();
        assert_eq!(
            pairs,
            vec![
                (".classhub/corpus/Week 3/n.md".to_string(), note_hash),
                ("Reading/p.pdf".to_string(), "p1".to_string()),
                ("Slides/deck.pptx".to_string(), "d1".to_string()),
                ("Weeks/Week 03/L.md".to_string(), "t1".to_string()),
            ]
        );
        // Resolving the widened entries reads the same places back.
        assert_eq!(
            current_hash(&conn, 3, &class_dir, ".classhub/corpus/Week 3/n.md").unwrap().as_deref(),
            Some(union[0].sha256.as_str())
        );
        assert_eq!(current_hash(&conn, 3, &class_dir, "Slides/deck.pptx").unwrap().as_deref(), Some("d1"));
        assert_eq!(current_hash(&conn, 3, &class_dir, "Study Guides/Week 3.html").unwrap(), None);
        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;

    /// A file the model refused is tried once at its hash — marked attempted
    /// with no extract, out of `stale_files` — and again once it changes.
    #[test]
    fn a_refused_file_is_not_retried_until_it_changes() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (3, 'Quizzes/Quiz 1.pdf', 'h1', 4, 1, 'pdf')",
            [],
        )
        .unwrap();
        assert_eq!(stale_files(&conn, 3).unwrap().len(), 1);
        mark_attempted(&conn, 3, "Quizzes/Quiz 1.pdf", "h1").unwrap();
        assert!(stale_files(&conn, 3).unwrap().is_empty(), "not enqueued again");
        let extract: Option<String> = conn
            .query_row("SELECT extract_rel_path FROM files WHERE rel_path = 'Quizzes/Quiz 1.pdf'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(extract, None, "no extract is claimed");
        conn.execute("UPDATE files SET sha256 = 'h2' WHERE rel_path = 'Quizzes/Quiz 1.pdf'", []).unwrap();
        assert_eq!(stale_files(&conn, 3).unwrap().len(), 1, "a changed file is tried again");
        assert!(refused_by_filter("API Error: 400 Output blocked by content filtering policy"));
        assert!(!refused_by_filter("API Error: 529 overloaded"));
    }

    /// A refusal marks only the file the run read and wrote nothing for: a
    /// file the run never reached stays stale, an extract it did write is
    /// recorded, and with no log a batch of one is the refusal itself.
    #[test]
    fn a_refusal_marks_only_the_file_the_run_reached() {
        let conn = crate::db::memory_db();
        let root = std::env::temp_dir().join(format!("classhub-refusal-{}", std::process::id()));
        let class_dir = root.join("Biostatistics for AI");
        std::fs::create_dir_all(class_dir.join(".classhub/extracts/Readings")).unwrap();
        crate::db::set_setting(&conn, "aibhs_root", root.to_str().unwrap()).unwrap();
        for (path, hash) in [("Quizzes/Quiz 1.pdf", "h1"), ("Readings/Week 4.pdf", "h2"), ("Readings/Week 5.pdf", "h3")] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
                 VALUES (3, ?1, ?2, 4, 1, 'pdf')",
                rusqlite::params![path, hash],
            )
            .unwrap();
        }
        std::fs::write(class_dir.join(".classhub/extracts/Readings/Week 5.pdf.md"), "# read\n").unwrap();
        let payload = serde_json::to_string(&[
            BatchItem::test_item("Quizzes/Quiz 1.pdf", "h1"),
            BatchItem::test_item("Readings/Week 4.pdf", "h2"),
            BatchItem::test_item("Readings/Week 5.pdf", "h3"),
        ])
        .unwrap();
        let read: BTreeSet<String> = ["Readings/Week 4.pdf".to_string(), "Readings/Week 5.pdf".to_string()].into();
        let refused = refusal_in(&conn, 3, &payload, &read).expect("reconcile");
        assert_eq!(refused, ["Readings/Week 4.pdf"], "the one read and unwritten");
        let stale: Vec<String> = stale_files(&conn, 3).unwrap().into_iter().map(|f| f.rel_path).collect();
        assert_eq!(stale, ["Quizzes/Quiz 1.pdf"], "never reached, so tried next run");
        let recorded: Option<String> = conn
            .query_row("SELECT extract_rel_path FROM files WHERE rel_path = 'Readings/Week 5.pdf'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(recorded.as_deref(), Some(".classhub/extracts/Readings/Week 5.pdf.md"), "written, so recorded");
        // No log and a batch of one: the refusal can only have been that file.
        let alone = serde_json::to_string(&[BatchItem::test_item("Quizzes/Quiz 1.pdf", "h1")]).unwrap();
        let refused = refusal_in(&conn, 3, &alone, &BTreeSet::new()).expect("reconcile");
        assert_eq!(refused, ["Quizzes/Quiz 1.pdf"]);
        assert!(stale_files(&conn, 3).unwrap().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod scope_duplicate_tests {
    use super::*;

    fn slot(week: i64, unit_id: i64) -> crate::units::WeekSlot {
        crate::units::WeekSlot {
            week,
            folder: format!("Week 0{week}"),
            unit_id,
            unit_name: format!("Week {week}"),
            unit_kind: "week".to_string(),
            meets_on: None,
        }
    }

    /// A duplicate leaves a scope only where its canonical copy is in the
    /// same scope: a paper posted for Week 3 and Week 5 stays in Week 5's
    /// set, while a second copy inside one folder reads once there, and the
    /// master reads each content once.
    #[test]
    fn a_duplicate_is_read_once_per_scope() {
        let conn = crate::db::memory_db();
        for (path, hash, canonical) in [
            ("Weeks/Week 03/paper.pdf", "p", None),
            ("Weeks/Week 05/paper.pdf", "p", Some("Weeks/Week 03/paper.pdf")),
            ("Weeks/Week 03/sub/paper.pdf", "p", Some("Weeks/Week 03/paper.pdf")),
            ("Module 1/deck.pdf", "d", None),
            ("Module 1/old/deck.pdf", "d", Some("Module 1/deck.pdf")),
        ] {
            conn.execute(
                "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind, duplicate_of)
                 VALUES (3, ?1, ?2, 4, 1, 'pdf', ?3)",
                rusqlite::params![path, hash, canonical],
            )
            .unwrap();
        }
        let slots = [slot(3, 13), slot(5, 15)];
        let filed = filed_under_weeks(&conn, 3).unwrap();
        let week3: Vec<String> = unit_manifest(&conn, 3, 13, None, &slots, &filed)
            .unwrap()
            .into_iter()
            .map(|e| e.rel_path)
            .collect();
        assert_eq!(week3, ["Weeks/Week 03/paper.pdf"], "one copy of the paper in Week 3");
        let week5: Vec<String> = unit_manifest(&conn, 3, 15, None, &slots, &filed)
            .unwrap()
            .into_iter()
            .map(|e| e.rel_path)
            .collect();
        assert_eq!(week5, ["Weeks/Week 05/paper.pdf"], "Week 5 keeps its only copy");
        let module: Vec<String> = folder_manifest(&conn, 3, "Module 1")
            .unwrap()
            .into_iter()
            .map(|e| e.rel_path)
            .collect();
        assert_eq!(module, ["Module 1/deck.pdf"]);
        let master = current_manifest(&conn, 3, "master").unwrap();
        assert_eq!(master.len(), 2, "{master:?}");
    }
}
