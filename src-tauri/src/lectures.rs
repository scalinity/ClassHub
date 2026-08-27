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
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
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
        Some(module) => {
            validate_module(module)?;
            format!("{}/{TRANSCRIPTS_DIR}", module.trim_end_matches('/'))
        }
        None => INBOX_DIR.to_string(),
    };

    let rel_path = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, req.class_id)?;
        let dir = class_dir.join(&dir_rel);
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

        // Never overwrite: two lectures on one date, or a re-add of the same
        // one, are both ordinary and neither should silently replace a file.
        let rel_path = unique_rel_path(&class_dir, &dir_rel, &file_name);
        crate::db::write_atomic(&class_dir.join(&rel_path), &markdown)?;
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
        )?;
        Ok(rel_path)
    })?;

    // Index it before anything downstream looks for it: the digest job's
    // manifest reads the `files` row, and the sorter lists the inbox.
    on_stage("Indexing…");
    {
        let db = app.state::<crate::Db>();
        let _ = crate::scanner::scan_class(&db.0, req.class_id);
    }
    crate::extract::spawn_pipeline(app, req.class_id);

    let digest_job_id = if req.digest && !routed_to_inbox {
        match enqueue_digest(app, req.class_id, &rel_path, &req.date) {
            Ok(id) => Some(id),
            // The transcript is filed and that is the durable half; a digest
            // that failed to enqueue is a button away, not a reason to unwind.
            Err(e) => {
                eprintln!("lecture digest failed to enqueue: {e:#}");
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

    let mut speakers: Vec<String> = Vec::new();
    for cue in &cues {
        if let Some(name) = &cue.speaker {
            if !speakers.contains(name) {
                speakers.push(name.clone());
            }
        }
    }
    Ok(AddResult { rel_path, routed_to_inbox, speakers, digest_job_id })
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
    let text = fs::read(path)
        .map(|b| String::from_utf8_lossy(&b).replace("\r\n", "\n"))
        .with_context(|| format!("reading {source}"))?;
    Ok((text, name))
}

/// A module must be a real folder inside the class and not an app-managed one —
/// the transcript is source material and belongs in the material tree.
fn validate_module(module: &str) -> Result<()> {
    let module = module.trim_matches('/');
    if module.is_empty() {
        bail!("choose a module folder");
    }
    let first = module.split('/').next().unwrap_or_default();
    if module.split('/').any(|seg| seg == ".." || seg.starts_with('.'))
        || crate::scanner::APP_MANAGED_DIRS.contains(&first)
    {
        bail!("'{module}' is not a module folder");
    }
    Ok(())
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

        match add(&app, &req, &on_stage) {
            Ok(result) => emit("Done", true, Some(&result), None),
            Err(e) => emit("Failed", true, None, Some(format!("{e:#}"))),
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
}

/// Queues the distillation of one filed transcript.
pub fn enqueue_digest(
    app: &AppHandle,
    class_id: i64,
    transcript_rel_path: &str,
    date: &str,
) -> Result<i64> {
    let prompt = with_conn(app, |conn| {
        let class_dir = crate::scanner::class_dir(conn, class_id)?;
        if !class_dir.join(transcript_rel_path).is_file() {
            bail!("no transcript at {transcript_rel_path}");
        }
        let class_name: String = conn.query_row(
            "SELECT display_name FROM classes WHERE id = ?1",
            [class_id],
            |row| row.get(0),
        )?;
        Ok(PROMPT_TEMPLATE
            .replace("{class}", &class_name)
            .replace("{date}", date)
            .replace("{transcript}", transcript_rel_path)
            .replace("{sessions_dir}", SESSIONS_DIR)
            .replace("{context}", &module_context(conn, class_id, transcript_rel_path)?))
    })?;

    let payload = serde_json::to_string(&DigestPayload {
        transcript_rel_path: transcript_rel_path.to_string(),
        date: date.to_string(),
    })?;
    crate::jobs::enqueue_lecture_digest(app, class_id, transcript_rel_path, &prompt, payload)
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

/// Verifies the two contracted documents exist where the job says it put them,
/// then records the digest. Both halves are checked because "wrote the HTML,
/// skipped the markdown" would otherwise pass as success and quietly leave the
/// digest out of chat's search scope.
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

    let conn_paths = [&result.rel_path_html, &result.rel_path_md];
    for rel in conn_paths {
        if !rel.starts_with(&format!("{SESSIONS_DIR}/")) {
            bail!("the digest wrote to {rel}, outside {SESSIONS_DIR}/");
        }
    }

    let db = app.state::<crate::Db>();
    let conn = lock(&db.0);
    let class_dir = crate::scanner::class_dir(&conn, class_id)?;
    for rel in conn_paths {
        let written = fs::metadata(class_dir.join(rel))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false);
        if !written {
            bail!("no session document written at {rel}");
        }
    }

    let scope = format!("{SESSION_SCOPE_PREFIX}{}", payload.transcript_rel_path);
    let manifest = serde_json::to_string(&crate::extract::current_manifest(
        &conn, class_id, &scope,
    )?)?;
    conn.execute(
        "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(class_id, scope) DO UPDATE SET
           rel_path = excluded.rel_path,
           generated_at = excluded.generated_at,
           source_manifest = excluded.source_manifest",
        rusqlite::params![class_id, scope, result.rel_path_html, now(), manifest],
    )?;
    drop(conn);

    // No hub-change push: this runs before the job row leaves `running`, and
    // the settle edge is what refetches guides — the same path module and
    // master guides take, and the reason `guides::finalize_job` emits nothing.
    Ok(format!("{} · {}", result.title, payload.date))
}

/// Session digests live in the `guides` table so they inherit the viewer and
/// staleness, but they are not module guides — the Study Guides list tells them
/// apart by this.
pub fn is_session_scope(scope: &str) -> bool {
    scope.starts_with(SESSION_SCOPE_PREFIX)
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
        assert!(validate_module("Module 1").is_ok());
        assert!(validate_module("Module 1/Week 2").is_ok());
        assert!(validate_module("").is_err());
        assert!(validate_module("Study Guides").is_err());
        assert!(validate_module("Notes").is_err());
        assert!(validate_module("_Inbox").is_err());
        assert!(validate_module("../../etc").is_err());
        assert!(validate_module(".classhub/extracts").is_err());
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

    #[test]
    fn recognises_a_session_scope() {
        assert!(is_session_scope("session:Module 1/Transcripts/2026-08-24 — Lecture.md"));
        assert!(!is_session_scope("Module 1"));
        assert!(!is_session_scope("master"));
    }
}
