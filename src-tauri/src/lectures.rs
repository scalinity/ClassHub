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
//! `extract::route` sends it down the zero-token text path, module guides pick
//! it up through the same rel-path prefix match they use for slides, and chat
//! searches it. The transcript joins the pipeline instead of sitting beside it.
//!
//! The digest is the separate, token-spending half — a `claude -p` job over the
//! transcript that writes the session document and names it.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

use crate::db::{
    audit, emit_hub_change, lock, now, with_conn, INBOX_DIR, SESSIONS_DIR, SESSION_SCOPE_PREFIX,
    TRANSCRIPTS_DIR,
};

const PROMPT_TEMPLATE: &str = include_str!("../prompts/lecture_digest.md");

/// What the UI hands over to add a lecture.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddRequest {
    pub class_id: i64,
    /// An absolute path to a caption track or a recording, or a Zoom link.
    pub source: String,
    /// Class-relative module folder. `None` routes through `_Inbox/` so the
    /// sorter proposes a home instead.
    pub module_rel_path: Option<String>,
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
    /// True when no module was chosen and the sorter is proposing one.
    pub routed_to_inbox: bool,
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

    let (caption, source_name) = fetch(app, &req.source, on_stage)?;
    let cues = crate::transcripts::parse(&caption);
    if cues.is_empty() {
        bail!("{source_name} holds no readable speech");
    }

    let title = req.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let file_name = transcript_file_name(&req.date, title.unwrap_or("Lecture"))?;
    let markdown = crate::transcripts::to_markdown(
        &cues,
        &crate::transcripts::Meta {
            title: file_name.trim_end_matches(".md"),
            date: &req.date,
            source_name: &source_name,
        },
    );

    // No module chosen means the sorter gets to propose one, which it can only
    // do from `_Inbox/` (SPEC §10).
    let routed_to_inbox = req.module_rel_path.is_none();
    let dir_rel = match &req.module_rel_path {
        Some(module) => format!("{}/{TRANSCRIPTS_DIR}", validate_module(module)?),
        None => INBOX_DIR.to_string(),
    };

    // Only the lookup needs the connection. Writing a multi-megabyte markdown
    // with it held blocks every other command, the chat tools and the job
    // runner for the duration — the arrangement `scan_class` and
    // `search_material` were both restructured away from.
    let class_dir = with_conn(app, |conn| crate::scanner::class_dir(conn, req.class_id))?;
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

/// A module must be a real folder inside the class and not an app-managed one —
/// the transcript is source material and belongs in the material tree.
///
/// Returns the normalized path, so the check and the write agree on what was
/// checked: validating a trimmed copy while building the destination from the
/// caller's original let a leading slash through, and joining an absolute path
/// discards the class directory entirely.
fn validate_module(module: &str) -> Result<String> {
    let module = module.trim().trim_matches('/');
    if module.is_empty() {
        bail!("choose a module folder");
    }
    let first = module.split('/').next().unwrap_or_default();
    if module
        .split('/')
        .any(|seg| seg.trim().is_empty() || seg == ".." || seg.starts_with('.'))
        || crate::scanner::APP_MANAGED_DIRS.contains(&first)
    {
        bail!("'{module}' is not a module folder");
    }
    Ok(module.to_string())
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
}

/// Queues the distillation of one filed transcript.
pub fn enqueue_digest(
    app: &AppHandle,
    class_id: i64,
    transcript_rel_path: &str,
    date: &str,
) -> Result<i64> {
    let scope = session_scope(transcript_rel_path);
    let (prompt, manifest) = with_conn(app, |conn| {
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
        let prompt = render_prompt(&DigestPromptVars {
            class: &class_name,
            date,
            transcript: transcript_rel_path,
            transcript_lines: lines,
            accent_light,
            accent_dark,
            context: &module_context(conn, class_id, transcript_rel_path)?,
        });
        let manifest =
            serde_json::to_string(&crate::extract::current_manifest(conn, class_id, &scope)?)?;
        Ok((prompt, manifest))
    })?;

    let payload = serde_json::to_string(&DigestPayload {
        transcript_rel_path: transcript_rel_path.to_string(),
        date: date.to_string(),
        source_manifest: manifest,
    })?;
    crate::jobs::enqueue_lecture_digest(app, class_id, transcript_rel_path, &prompt, payload)
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
}

/// The rest of the module the transcript sits in, so the digest can tie what
/// was said to the slides and readings it was said about.
fn module_context(conn: &Connection, class_id: i64, transcript_rel: &str) -> Result<String> {
    let module = transcript_rel
        .rsplit_once(&format!("/{TRANSCRIPTS_DIR}/"))
        .map(|(module, _)| module.to_string());
    let Some(module) = module else {
        return Ok("(none — this transcript is not filed inside a module)".into());
    };

    let mut stmt = conn.prepare(
        "SELECT rel_path, extract_rel_path FROM files
         WHERE class_id = ?1 AND rel_path LIKE ?2 ESCAPE '\\' AND rel_path != ?3
         ORDER BY rel_path",
    )?;
    let prefix = format!(
        "{}/%",
        module.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
    );
    let lines: Vec<String> = stmt
        .query_map(rusqlite::params![class_id, prefix, transcript_rel], |row| {
            let rel: String = row.get(0)?;
            let extract: Option<String> = row.get(1)?;
            Ok(match extract {
                Some(e) => format!("- {rel}\n  extract: {e}"),
                None => format!("- {rel}"),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(if lines.is_empty() {
        format!("(no other material in {module} yet)")
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
    Ok(format!("{} · {}", result.title, payload.date))
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn rejects_app_managed_and_escaping_module_paths() {
        assert_eq!(validate_module("Module 1").unwrap(), "Module 1");
        assert_eq!(validate_module("Module 1/Week 2").unwrap(), "Module 1/Week 2");
        assert!(validate_module("").is_err());
        assert!(validate_module("Study Guides").is_err());
        assert!(validate_module("Notes").is_err());
        assert!(validate_module("_Inbox").is_err());
        assert!(validate_module("../../etc").is_err());
        assert!(validate_module(".classhub/extracts").is_err());
    }

    /// The check normalizes and the write must use what was checked. An
    /// absolute path joined onto the class directory discards it outright, so
    /// "passes validation" and "writes inside the class" have to be the same
    /// question about the same string.
    #[test]
    fn normalizes_what_it_validates() {
        assert_eq!(validate_module("/Module 1/").unwrap(), "Module 1");
        assert_eq!(validate_module("  Module 1  ").unwrap(), "Module 1");
        assert!(validate_module("Module 1//Week 2").is_err());
        assert!(validate_module("/").is_err());

        // Whatever it returns is relative, so joining it onto the class
        // directory cannot land anywhere else — an absolute path would have
        // discarded the class directory outright.
        for candidate in ["/Users/danny/elsewhere", "/etc/passwd", "//tmp/x"] {
            if let Ok(module) = validate_module(candidate) {
                assert!(!Path::new(&module).is_absolute(), "{candidate} → {module}");
                assert!(!module.contains(".."), "{candidate} → {module}");
            }
        }
    }

    /// The digest job writes through the ordinary job write-contract. If
    /// `SESSIONS_DIR` ever moved out from under `GUIDES_DIR`, every digest run
    /// would be demoted as an out-of-contract write — with nothing else in the
    /// codebase to say why.
    #[test]
    fn keeps_the_digest_inside_the_contracted_write_scope() {
        assert!(SESSIONS_DIR.starts_with(crate::db::GUIDES_DIR), "{SESSIONS_DIR}");
        assert!(crate::db::JOB_WRITABLE.contains(&crate::db::GUIDES_DIR));
    }

    /// The digest job has an output contract, and the prompt is the only place
    /// it is stated. An unfilled `{…}` reaches the model as literal text, which
    /// is what happened to the accent hue for the whole of M12 — the prompt
    /// asked for a colour nothing ever substituted, and no test could see it.
    #[test]
    fn fills_every_placeholder_in_the_digest_prompt() {
        let rendered = render_prompt(&DigestPromptVars {
            class: "Biostatistics for AI",
            date: "2026-08-24",
            transcript: "Module 1/Transcripts/2026-08-24 — Lecture.md",
            transcript_lines: 4_210,
            accent_light: "oklch(0.578 0.135 158)",
            accent_dark: "oklch(0.732 0.13 158)",
            context: "- Module 1/slides.pdf",
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
        ] {
            assert!(rendered.contains(expected), "missing {expected:?}");
        }
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
        let dir = std::env::temp_dir().join("classhub-digest-pair");
        let sessions = dir.join(SESSIONS_DIR);
        fs::create_dir_all(&sessions).expect("sessions dir");
        let html = "Study Guides/Sessions/2026-08-24 — Attention.html";
        fs::write(dir.join(html), "<html></html>").expect("html");

        let conn = Connection::open_in_memory().expect("conn");
        let payload = DigestPayload {
            transcript_rel_path: "Module 1/Transcripts/2026-08-24 — Lecture.md".into(),
            date: "2026-08-24".into(),
            source_manifest: "[]".into(),
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

    #[test]
    fn recognises_a_session_scope() {
        assert!(crate::db::is_session_scope("session:Module 1/Transcripts/2026-08-24 — Lecture.md"));
        assert!(!crate::db::is_session_scope("Module 1"));
        assert!(!crate::db::is_session_scope("master"));
    }
}
