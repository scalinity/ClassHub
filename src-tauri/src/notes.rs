//! SPEC §11 — per-class markdown notes in `<Class>/Notes/`. Chat's
//! `write_note` tool (M8) and the workspace editor (M11) share one write path.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

use crate::db::{NOTES_DIR, audit, emit_hub_change, notify, with_conn};

/// Notes are prose; anything bigger than this is not a note.
pub(crate) const MAX_NOTE_BYTES: usize = 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteFile {
    pub name: String,
    /// Class-relative, e.g. `Notes/Week 3 recap.md`.
    pub rel_path: String,
    pub modified_at: i64,
}

pub struct WrittenNote {
    pub rel_path: String,
    pub created: bool,
    /// The audit row the save wrote, for the notice's `Undo`.
    pub audit_id: i64,
}

/// A note title becomes its file name; slashes and colons would change the
/// path, so they flatten to dashes rather than erroring on a natural title.
fn note_file_name(title: &str) -> Result<String> {
    let cleaned: String = title
        .trim()
        .trim_end_matches(".md")
        .chars()
        .map(|c| if c == '/' || c == ':' { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        bail!("the note needs a title");
    }
    if cleaned.chars().count() > 80 {
        bail!("note title is too long — keep it under 80 characters");
    }
    Ok(format!("{cleaned}.md"))
}

/// One write path for every surface (chat tool and editor): `audit_action`
/// names the caller (`chat.write_note` / `ui.write_note`) on the audit row
/// that parks the replaced content.
pub fn write_note(
    conn: &Connection,
    class_id: i64,
    title: &str,
    content: &str,
    audit_action: &str,
) -> Result<WrittenNote> {
    let file_name = note_file_name(title)?;
    let dir = crate::scanner::class_dir(conn, class_id)?.join(NOTES_DIR);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let abs = dir.join(&file_name);
    let rel_path = format!("{NOTES_DIR}/{file_name}");
    write_audited(conn, class_id, &abs, &rel_path, content, audit_action)
}

/// Saves over an exact existing file in `Notes/` — the editor's path for a
/// note opened from the listing, where re-deriving the name from the title
/// could route the save to a different file (sanitization flattens `/` and
/// `:`, and the listing also surfaces non-`.md` files). The title plays no
/// part here; `rel_path` must be a plain `Notes/<file>` path.
pub fn overwrite_note(
    conn: &Connection,
    class_id: i64,
    rel_path: &str,
    content: &str,
    audit_action: &str,
) -> Result<WrittenNote> {
    let file_name = rel_path
        .strip_prefix(&format!("{NOTES_DIR}/"))
        .with_context(|| format!("notes live under {NOTES_DIR}/ — got '{rel_path}'"))?;
    if file_name.is_empty() || file_name.contains('/') || file_name.starts_with('.') {
        bail!("not a note file name: '{file_name}'");
    }
    let abs = crate::scanner::class_dir(conn, class_id)?
        .join(NOTES_DIR)
        .join(file_name);
    write_audited(conn, class_id, &abs, rel_path, content, audit_action)
}

/// The shared tail of every note save: read the previous version (an
/// unreadable file aborts — overwriting content the audit log cannot recover
/// would defeat the undo), commit the audit row, then write the file. The
/// ordering is deliberate: a committed audit row for a write that then fails
/// is a harmless stray, while the reverse — an overwrite whose previous
/// version was never parked — is unrecoverable.
fn write_audited(
    conn: &Connection,
    class_id: i64,
    abs: &Path,
    rel_path: &str,
    content: &str,
    audit_action: &str,
) -> Result<WrittenNote> {
    if content.trim().is_empty() {
        bail!("the note has no content");
    }
    if content.len() > MAX_NOTE_BYTES {
        bail!("note content is too large (1 MB max)");
    }
    let previous = match fs::read_to_string(abs) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => bail!("cannot read the existing {rel_path} ({e}) — not overwriting it"),
    };
    let created = previous.is_none();
    let mut text = content.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    // The hash of what this save writes rides the row, so an undo can tell
    // that the note still holds it and refuse to overwrite a later edit.
    let tx = conn.unchecked_transaction()?;
    let audit_id = audit(
        &tx,
        audit_action,
        json!({ "classId": class_id, "relPath": rel_path,
                "created": created, "previousContent": previous,
                "contentSha256": content_hash(&text) }),
    )?;
    tx.commit()?;
    crate::db::write_atomic(abs, &text)?;
    Ok(WrittenNote {
        rel_path: rel_path.to_string(),
        created,
        audit_id,
    })
}

fn content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The inverse of `ui.write_note` and `chat.write_note` (SPEC §6): the
/// replaced content goes back, or a note the save created goes. Refused
/// when the note no longer holds what the save wrote — a later edit is not
/// this row's to undo — or when the row predates the hash that says so.
pub(crate) fn undo_write(
    conn: &Connection,
    payload: &serde_json::Value,
) -> Result<crate::deadlines::Undone> {
    let class_id = payload["classId"].as_i64().context("the row names no class")?;
    let rel_path = payload["relPath"].as_str().context("the row names no note")?;
    let Some(written) = payload["contentSha256"].as_str() else {
        bail!("this save was recorded before undo existed — edit the note instead");
    };
    let file_name = rel_path
        .strip_prefix(&format!("{NOTES_DIR}/"))
        .with_context(|| format!("notes live under {NOTES_DIR}/ — got '{rel_path}'"))?;
    if file_name.is_empty() || file_name.contains('/') || file_name.starts_with('.') {
        bail!("not a note file name: '{file_name}'");
    }
    let abs = crate::scanner::class_dir(conn, class_id)?.join(NOTES_DIR).join(file_name);
    let current = fs::read_to_string(&abs)
        .with_context(|| format!("{rel_path} cannot be read — it may have been removed"))?;
    if content_hash(&current) != written {
        bail!("{rel_path} has been edited since — its current text is not this save's");
    }
    let name = note_title(rel_path).to_string();
    match payload["previousContent"].as_str() {
        Some(previous) => crate::db::write_atomic(&abs, previous)?,
        None => fs::remove_file(&abs).with_context(|| format!("removing {rel_path}"))?,
    }
    let what = if payload["previousContent"].is_null() {
        format!("Removed {name}")
    } else {
        format!("Restored {name}")
    };
    Ok(crate::deadlines::Undone { what, class_id: Some(class_id), forgotten: Vec::new() })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedNote {
    pub rel_path: String,
    pub created: bool,
}

/// The editor's save — same write path and audit shape as the chat tool, so
/// an overwrite can never silently destroy a note from either surface. With
/// `rel_path` set, the save targets that exact file; otherwise the title
/// names a (new) file.
pub fn save_from_ui(
    app: &AppHandle,
    class_id: i64,
    title: &str,
    content: &str,
    rel_path: Option<&str>,
) -> Result<SavedNote> {
    let written = with_conn(app, |conn| match rel_path {
        Some(rel) => overwrite_note(conn, class_id, rel, content, "ui.write_note"),
        None => write_note(conn, class_id, title, content, "ui.write_note"),
    })?;
    emit_hub_change(app, "notes");
    notify(
        app,
        format!("Saved {}", note_title(&written.rel_path)),
        vec![written.audit_id],
        Some(class_id),
    );
    Ok(SavedNote {
        rel_path: written.rel_path,
        created: written.created,
    })
}

/// A note's name as the listing shows it: the file name without `.md`.
pub(crate) fn note_title(rel_path: &str) -> &str {
    let name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    // One suffix, not every trailing one: a note titled `Week 3.md` is the
    // file `Week 3.md.md`, and its name is `Week 3.md`.
    name.strip_suffix(".md").unwrap_or(name)
}

pub fn list_notes(conn: &Connection, class_id: i64) -> Result<Vec<NoteFile>> {
    let dir = crate::scanner::class_dir(conn, class_id)?.join(NOTES_DIR);
    Ok(list_dir_files(&dir, NOTES_DIR))
}

/// Files in an app-managed directory, newest first. Shared with the practice
/// listing — both are flat, disk-is-truth listings outside the file index.
pub fn list_dir_files(dir: &Path, rel_prefix: &str) -> Vec<NoteFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<NoteFile> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let modified_at = e
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            Some(NoteFile {
                rel_path: format!("{rel_prefix}/{name}"),
                name,
                modified_at,
            })
        })
        .collect();
    files.sort_by(|a, b| b.modified_at.cmp(&a.modified_at).then(a.name.cmp(&b.name)));
    files
}
