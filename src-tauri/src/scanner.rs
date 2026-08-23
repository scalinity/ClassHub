use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use sha2::{Digest, Sha256};

/// App-managed directories excluded from scanning (SPEC §4), at class-folder top level.
/// Hidden entries (incl. `.classhub`) are excluded at every depth. Shared with the
/// drop-to-sort and chat move validators so all three enforce one policy.
pub const APP_MANAGED_DIRS: &[&str] = &["Study Guides", "Notes", "_Inbox"];

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNode {
    pub name: String,
    pub rel_path: String,
    pub dir: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    pub children: Vec<TreeNode>,
}

struct ScannedFile {
    rel_path: String,
    sha256: String,
    size: i64,
    mtime: i64,
    kind: String,
}

/// Previously indexed (size, mtime, sha256) per rel_path, to skip re-hashing unchanged files.
type ExistingIndex = HashMap<String, (i64, i64, String)>;

pub fn class_dir(conn: &Connection, class_id: i64) -> Result<PathBuf> {
    let folder: String = conn
        .query_row(
            "SELECT folder_name FROM classes WHERE id = ?1",
            [class_id],
            |row| row.get(0),
        )
        .context("looking up class")?;
    Ok(crate::db::aibhs_root(conn)?.join(folder))
}

/// Walks the class folder, syncs the `files` table (upsert added/changed, delete removed),
/// and returns the module/file tree.
pub fn scan_class(conn: &mut Connection, class_id: i64) -> Result<Vec<TreeNode>> {
    let dir = class_dir(conn, class_id)?;
    if !dir.is_dir() {
        bail!("class folder not found: {}", dir.display());
    }

    let existing = load_existing(conn, class_id)?;
    let mut files = Vec::new();
    let tree = walk_dir(&dir, &dir, 0, &existing, &mut files)
        .with_context(|| format!("scanning {}", dir.display()))?;

    let seen: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
    let tx = conn.transaction()?;
    {
        let mut upsert = tx.prepare(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(class_id, rel_path) DO UPDATE SET
               sha256 = excluded.sha256, size = excluded.size,
               mtime = excluded.mtime, kind = excluded.kind",
        )?;
        for f in &files {
            upsert.execute(params![class_id, f.rel_path, f.sha256, f.size, f.mtime, f.kind])?;
        }
        let mut delete = tx.prepare("DELETE FROM files WHERE class_id = ?1 AND rel_path = ?2")?;
        for rel_path in existing.keys() {
            if !seen.contains(rel_path.as_str()) {
                delete.execute(params![class_id, rel_path])?;
            }
        }
    }
    tx.commit()?;
    Ok(tree)
}

/// Resolves a scanner-issued rel_path against the class folder, rejecting traversal.
/// An empty rel_path resolves to the class folder itself.
pub fn resolve_rel(conn: &Connection, class_id: i64, rel_path: &str) -> Result<PathBuf> {
    let rel = Path::new(rel_path);
    if rel.components().any(|c| !matches!(c, Component::Normal(_))) {
        bail!("invalid path: {rel_path}");
    }
    let abs = class_dir(conn, class_id)?.join(rel);
    if fs::symlink_metadata(&abs).is_err() {
        bail!("no longer on disk (rescan the class): {rel_path}");
    }
    Ok(abs)
}

fn load_existing(conn: &Connection, class_id: i64) -> Result<ExistingIndex> {
    let mut stmt =
        conn.prepare("SELECT rel_path, size, mtime, sha256 FROM files WHERE class_id = ?1")?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                (row.get::<_, i64>(1)?, row.get::<_, i64>(2)?, row.get::<_, String>(3)?),
            ))
        })?
        .collect::<rusqlite::Result<ExistingIndex>>()?;
    Ok(rows)
}

fn walk_dir(
    dir: &Path,
    class_dir: &Path,
    depth: usize,
    existing: &ExistingIndex,
    out: &mut Vec<ScannedFile>,
) -> Result<Vec<TreeNode>> {
    let mut dirs = Vec::new();
    let mut files = Vec::new();

    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if depth == 0 && APP_MANAGED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let rel_path = path
            .strip_prefix(class_dir)
            .expect("walked path is under class dir")
            .to_string_lossy()
            .into_owned();

        if file_type.is_dir() {
            let children = walk_dir(&path, class_dir, depth + 1, existing, out)?;
            dirs.push(TreeNode {
                name,
                rel_path,
                dir: true,
                kind: None,
                size: None,
                children,
            });
        } else {
            let meta = entry.metadata()?;
            let size = meta.len() as i64;
            let mtime = meta
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let kind = kind_for(&path).to_string();
            let sha256 = match existing.get(&rel_path) {
                Some((s, m, hash)) if *s == size && *m == mtime => hash.clone(),
                _ => hash_file(&path)
                    .with_context(|| format!("hashing {}", path.display()))?,
            };
            out.push(ScannedFile {
                rel_path: rel_path.clone(),
                sha256,
                size,
                mtime,
                kind: kind.clone(),
            });
            files.push(TreeNode {
                name,
                rel_path,
                dir: false,
                kind: Some(kind),
                size: Some(size),
                children: Vec::new(),
            });
        }
    }

    let by_name = |a: &TreeNode, b: &TreeNode| a.name.to_lowercase().cmp(&b.name.to_lowercase());
    dirs.sort_by(by_name);
    files.sort_by(by_name);
    dirs.extend(files);
    Ok(dirs)
}

pub fn kind_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pptx" => "pptx",
        "pdf" => "pdf",
        "rmd" => "rmd",
        "r" => "r",
        "html" | "htm" => "html",
        "md" => "md",
        _ => "other",
    }
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
