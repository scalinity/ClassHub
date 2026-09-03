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

fn run_pipeline(app: &AppHandle, class_id: i64) -> Result<()> {
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
            return Ok(());
        }
        (crate::scanner::class_dir(&conn, class_id)?, stale_files(&conn, class_id)?)
    };
    if stale.is_empty() {
        eprintln!("extract pipeline class {class_id}: nothing stale — zero work");
        return Ok(());
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
            record(&conn, class_id, &file.rel_path, &extract_rel, &file.sha256)
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
        return Ok(());
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
        return Ok(());
    };
    eprintln!(
        "extract pipeline class {class_id}: enqueued extract job {job_id} for {} PDF(s)",
        batch.len()
    );
    Ok(())
}

/// SPEC §7 step 4: record keeping after a successful claude extract job.
/// Called by the job runner before it marks the row succeeded, so a scan that
/// sees no active job also sees up-to-date extract columns.
pub fn finalize_job(app: &AppHandle, class_id: i64, payload: &str) -> Result<()> {
    let items: Vec<BatchItem> =
        serde_json::from_str(payload).context("parsing extract batch payload")?;
    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let class_dir = crate::scanner::class_dir(&conn, class_id)?;
    for item in items {
        let written = fs::metadata(class_dir.join(&item.extract_rel_path))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if written {
            record(&conn, class_id, &item.rel_path, &item.extract_rel_path, &item.sha256)?;
        } else {
            eprintln!(
                "extract job wrote no output for {} — stays stale for the next scan",
                item.rel_path
            );
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// DB helpers

fn stale_files(conn: &Connection, class_id: i64) -> Result<Vec<StaleFile>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, sha256, kind FROM files
         WHERE class_id = ?1
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

fn record(
    conn: &Connection,
    class_id: i64,
    rel_path: &str,
    extract_rel_path: &str,
    sha256: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE files SET extract_rel_path = ?1, extracted_at = ?2, extracted_sha256 = ?3
         WHERE class_id = ?4 AND rel_path = ?5",
        params![extract_rel_path, now(), sha256, class_id, rel_path],
    )?;
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
/// export (SPEC §7 step 3): drops tags plus script/style payloads, decodes
/// common entities, keeps `<pre>` text intact, and collapses runs of blank
/// lines. Outside `<pre>` a newline in the text is source formatting — HTML
/// renders it as a space — and it is folded into one, because LibreOffice
/// hard-wraps every paragraph at about seventy characters and a phrase that
/// crossed the wrap would be unfindable by a line-based search. Zero tokens,
/// zero dependencies.
fn strip_html(html: &str) -> String {
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
/// from the deck as it is now. An unconverted deck is refused, and the tree
/// opens it in its default app instead.
pub fn pdf_view_path(conn: &Connection, class_id: i64, rel_path: &str) -> Result<PathBuf> {
    let source = crate::scanner::resolve_rel(conn, class_id, rel_path)?;
    match crate::scanner::kind_for(&source) {
        "pdf" => Ok(source),
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
            if !twin.is_current(&sha256) {
                bail!("not converted to PDF yet — the next scan converts it");
            }
            Ok(twin.abs)
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

    // A unit's sources come from two places that share no path prefix: the
    // folder holding its material, and the lectures the calendar mapped to it
    // (SPEC §8.5). The union is what makes a unit guide go stale when its
    // lecture changes — with the transcripts left out nothing visibly breaks,
    // the guide just quietly stops updating.
    if let Some(unit_name) = scope.strip_prefix(crate::db::UNIT_SCOPE_PREFIX) {
        let unit: Option<(i64, Option<String>)> = conn
            .query_row(
                "SELECT id, rel_path FROM units WHERE class_id = ?1 AND name = ?2",
                rusqlite::params![class_id, unit_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((unit_id, unit_folder)) = unit else {
            return Ok(Vec::new());
        };

        let mut entries = match unit_folder {
            Some(folder) => folder_manifest(conn, class_id, &folder)?,
            None => Vec::new(),
        };
        let mut stmt = conn.prepare(
            "SELECT f.rel_path, f.sha256 FROM files f
             JOIN lecture_contributions lc
               ON lc.class_id = f.class_id AND lc.rel_path = f.rel_path
             WHERE f.class_id = ?1 AND lc.unit_id = ?2 AND lc.status = 'applied'",
        )?;
        let rows = stmt.query(rusqlite::params![class_id, unit_id])?;
        entries.extend(read(rows)?);
        // A transcript filed inside the unit's own folder would otherwise be
        // counted twice, and a manifest that holds a duplicate never equals the
        // set it is compared against.
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        entries.dedup_by(|a, b| a.rel_path == b.rel_path);
        return Ok(entries);
    }

    let entries = if scope == crate::db::MASTER_SCOPE {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 ORDER BY rel_path",
        )?;
        let rows = stmt.query([class_id])?;
        read(rows)?
    } else {
        folder_manifest(conn, class_id, scope)?
    };
    Ok(entries)
}

/// Everything indexed at or under one folder.
fn folder_manifest(conn: &Connection, class_id: i64, folder: &str) -> Result<Vec<ManifestEntry>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, sha256 FROM files
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
            Ok(ManifestEntry {
                rel_path: row.get(0)?,
                sha256: row.get(1)?,
            })
        })
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// SPEC §7 step 5: a guide is stale when its stored `source_manifest` differs
/// from the current set (order-insensitive set comparison).
pub fn manifest_is_stale(stored_manifest_json: &str, current: &[ManifestEntry]) -> bool {
    let Ok(stored) = serde_json::from_str::<Vec<ManifestEntry>>(stored_manifest_json) else {
        return true; // unparseable manifest = stale
    };
    let stored: std::collections::HashSet<&ManifestEntry> = stored.iter().collect();
    let current: std::collections::HashSet<&ManifestEntry> = current.iter().collect();
    stored != current
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
            class_dir.join("Slides/paper.pdf")
        );
        let err = pdf_view_path(&conn, class_id, "Slides/deck.pptx").unwrap_err();
        assert!(err.to_string().contains("not converted"), "{err:#}");

        let twin = class_dir.join(".classhub/extracts/Slides/deck.pptx.pdf");
        std::fs::write(&twin, b"pdf").unwrap();
        std::fs::write(class_dir.join(".classhub/extracts/Slides/deck.pptx.pdf.sha256"), "abc").unwrap();
        assert_eq!(pdf_view_path(&conn, class_id, "Slides/deck.pptx").expect("twin"), twin);

        // The deck changed since the twin was made: back to the default app
        // until the next scan converts it again.
        conn.execute("UPDATE files SET sha256 = 'abd' WHERE rel_path = 'Slides/deck.pptx'", [])
            .expect("edit");
        assert!(pdf_view_path(&conn, class_id, "Slides/deck.pptx").is_err());

        let err = pdf_view_path(&conn, class_id, "Slides/notes.md").unwrap_err();
        assert!(err.to_string().contains("no PDF"), "{err:#}");
        assert!(pdf_view_path(&conn, class_id, "../etc/passwd").is_err());
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

        let scope = format!("{}Week 3 — Transformers", crate::db::UNIT_SCOPE_PREFIX);
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
        assert!(super::manifest_is_stale(&stored, &after), "the union missed the lecture");
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
        let scope = format!("{}Week 9", crate::db::UNIT_SCOPE_PREFIX);
        assert!(current_manifest(&conn, 1, &scope).expect("manifest").is_empty());
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
}
