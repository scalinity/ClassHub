//! SPEC §11 — per-class markdown notes in `<Class>/Notes/`. M8 gives chat
//! `write_note` and the workspace a read-only listing; the editor lands in M11.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use serde::Serialize;

pub const NOTES_DIR: &str = "Notes";
/// Notes are prose; anything bigger than this is not a note.
const MAX_NOTE_BYTES: usize = 1024 * 1024;

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
    /// The content replaced by an overwrite — kept in the audit log so a
    /// chat write can never silently destroy a note.
    pub previous: Option<String>,
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

pub fn write_note(
    conn: &Connection,
    class_id: i64,
    title: &str,
    content: &str,
) -> Result<WrittenNote> {
    if content.trim().is_empty() {
        bail!("the note has no content");
    }
    if content.len() > MAX_NOTE_BYTES {
        bail!("note content is too large (1 MB max)");
    }
    let file_name = note_file_name(title)?;
    let dir = crate::scanner::class_dir(conn, class_id)?.join(NOTES_DIR);
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    let abs = dir.join(&file_name);
    let previous = fs::read_to_string(&abs).ok();
    let mut text = content.to_string();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    fs::write(&abs, text).with_context(|| format!("writing {}", abs.display()))?;
    Ok(WrittenNote {
        rel_path: format!("{NOTES_DIR}/{file_name}"),
        created: previous.is_none(),
        previous,
    })
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
