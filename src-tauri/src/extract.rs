//! SPEC §7 — ingestion & extraction pipeline (steps 2–4) and the staleness
//! manifest utility (step 5 groundwork; badges land in M5).
//!
//! A file is stale when `extracted_sha256` differs from its current `sha256`
//! (the scanner's upsert deliberately leaves extract columns untouched).
//! Text-native formats are extracted locally — zero tokens. PPTX sources are
//! converted to PDF via headless LibreOffice, then all stale PDFs go into one
//! batched `extract` job per class through the job runner (SPEC §6, the single
//! gateway to the subscription). Extract paths mirror the source rel path with
//! `.md` appended under `.classhub/extracts/` (SPEC §4).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
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

enum Route {
    Text,
    Html,
    Pdf,
    Pptx,
    Skip,
}

fn route(rel_path: &str, kind: &str) -> Route {
    match kind {
        "rmd" | "r" | "md" => Route::Text,
        "html" => Route::Html,
        "pdf" => Route::Pdf,
        "pptx" => Route::Pptx,
        _ if rel_path.to_lowercase().ends_with(".txt") => Route::Text,
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
            // scan picks up anything it left stale.
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
        match route(&file.rel_path, &file.kind) {
            Route::Text | Route::Html => {
                let html = matches!(route(&file.rel_path, &file.kind), Route::Html);
                match extract_local(&class_dir, &file.rel_path, &extract_rel, html) {
                    Ok(()) => {
                        let db = app.state::<crate::Db>();
                        record(&lock(&db.0), class_id, &file.rel_path, &extract_rel, &file.sha256)?;
                    }
                    Err(e) => eprintln!(
                        "extract pipeline class {class_id}: local extract of {} failed: {e:#}",
                        file.rel_path
                    ),
                }
            }
            Route::Pdf => batch.push(BatchItem {
                rel_path: file.rel_path.clone(),
                sha256: file.sha256.clone(),
                input_rel_path: file.rel_path.clone(),
                extract_rel_path: extract_rel,
            }),
            Route::Pptx => match convert_pptx(app, &class_dir, &file.rel_path, &file.sha256) {
                Ok(pdf_rel) => batch.push(BatchItem {
                    rel_path: file.rel_path.clone(),
                    sha256: file.sha256.clone(),
                    input_rel_path: pdf_rel,
                    extract_rel_path: extract_rel,
                }),
                Err(e) => eprintln!(
                    "extract pipeline class {class_id}: pptx conversion of {} failed: {e:#}",
                    file.rel_path
                ),
            },
            Route::Skip => {}
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
    let job_id = crate::jobs::enqueue_extract(app, class_id, &prompt, payload)?;
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

fn extract_local(class_dir: &Path, rel_path: &str, extract_rel: &str, html: bool) -> Result<()> {
    let raw = fs::read(class_dir.join(rel_path))
        .with_context(|| format!("reading {rel_path}"))?;
    let text = String::from_utf8_lossy(&raw).replace("\r\n", "\n");
    let mut content = if html { strip_html(&text) } else { text };
    content.truncate(content.trim_end().len());
    content.push('\n');

    let abs = class_dir.join(extract_rel);
    let parent = abs.parent().context("extract path has no parent")?;
    fs::create_dir_all(parent)?;
    fs::write(&abs, content).with_context(|| format!("writing {extract_rel}"))?;
    Ok(())
}

const SKIP_CONTENT_TAGS: &[&str] = &["script", "style", "noscript"];
const BLOCK_TAGS: &[&str] = &[
    "p", "div", "br", "li", "ul", "ol", "tr", "table", "thead", "tbody", "pre",
    "section", "article", "header", "footer", "blockquote", "hr", "h1", "h2",
    "h3", "h4", "h5", "h6",
];

/// Minimal tag-strip for R-rendered notebook HTML (SPEC §7 step 3): drops tags
/// plus script/style payloads, decodes common entities, keeps `<pre>` text
/// intact, and collapses runs of blank lines. Zero tokens, zero dependencies.
fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 4);
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        decode_entities(&rest[..open], &mut out);
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
        if BLOCK_TAGS.contains(&name.as_str()) {
            out.push('\n');
        } else if name == "td" || name == "th" {
            out.push(' ');
        }
    }
    decode_entities(rest, &mut out);

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
// PPTX → PDF conversion (SPEC §7 step 2)

/// Converts `<rel>.pptx` to `.classhub/extracts/<rel>.pptx.pdf`, skipping when
/// the existing PDF was produced from the current source hash (sidecar file).
/// Returns the converted PDF's class-relative path.
fn convert_pptx(app: &AppHandle, class_dir: &Path, rel_path: &str, sha256: &str) -> Result<String> {
    let pdf_rel = format!("{EXTRACTS_DIR}/{rel_path}.pdf");
    let pdf_abs = class_dir.join(&pdf_rel);
    let sidecar = class_dir.join(format!("{pdf_rel}.sha256"));
    let current = pdf_abs.is_file()
        && fs::read_to_string(&sidecar)
            .map(|s| s.trim() == sha256)
            .unwrap_or(false);
    if current {
        return Ok(pdf_rel);
    }

    let out_dir = pdf_abs.parent().context("pdf path has no parent")?;
    fs::create_dir_all(out_dir)?;
    // A dedicated user profile keeps headless runs independent of any open
    // LibreOffice GUI instance (they otherwise refuse to start concurrently).
    let profile = app
        .path()
        .app_data_dir()
        .context("resolving app data dir")?
        .join("soffice-profile");
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
        .args(["--headless", "--convert-to", "pdf", "--outdir"])
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

    // soffice names its output `<stem>.pdf`; move it to `<name>.pptx.pdf` (§4).
    let stem = Path::new(rel_path)
        .file_stem()
        .context("pptx has no file stem")?
        .to_string_lossy();
    let produced = out_dir.join(format!("{stem}.pdf"));
    if !produced.is_file() {
        bail!(
            "soffice reported success but produced no {}: {}",
            produced.display(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
    fs::rename(&produced, &pdf_abs)?;
    fs::write(&sidecar, sha256)?;
    Ok(pdf_rel)
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
#[derive(Serialize, Deserialize, PartialEq, Eq, Hash)]
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
    let entries = if scope == crate::db::MASTER_SCOPE {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files WHERE class_id = ?1 ORDER BY rel_path",
        )?;
        let rows = stmt.query([class_id])?;
        read(rows)?
    } else {
        let mut stmt = conn.prepare(
            "SELECT rel_path, sha256 FROM files
             WHERE class_id = ?1 AND (rel_path = ?2 OR rel_path LIKE ?3 ESCAPE '\\')
             ORDER BY rel_path",
        )?;
        // The separator is appended before matching, so `Module 1` cannot
        // capture `Module 10`; LIKE wildcards in a folder name are escaped.
        let prefix = format!(
            "{}/%",
            scope.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
        );
        let rows = stmt.query(rusqlite::params![class_id, scope, prefix])?;
        read(rows)?
    };
    Ok(entries)
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
