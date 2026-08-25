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
    let root = class_dir(conn, class_id)?;
    let abs = root.join(rel);
    let Ok(meta) = fs::symlink_metadata(&abs) else {
        bail!("no longer on disk (rescan the class): {rel_path}");
    };
    // The component check above stops `..`, but a symlink reaches outside the
    // class folder without one — and callers here read the file, or hand the
    // path to the OS to open. The walk already skips symlinks, so one that
    // resolves is not something the index put there. Ancestors count too: the
    // leaf can be an ordinary file inside a linked directory.
    if meta.file_type().is_symlink() {
        bail!("not a regular file: {rel_path}");
    }
    let mut walked = root;
    for component in rel.components() {
        walked.push(component);
        if fs::symlink_metadata(&walked)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            bail!("path crosses a symlink: {rel_path}");
        }
    }
    Ok(abs)
}

/// Every file in the class folder a synthesis job is NOT contracted to write,
/// fingerprinted by (size, mtime) — the same pair `scan_class` already trusts
/// to decide a file changed.
///
/// A job gets `Write`/`Edit` over the whole class folder, because `--add-dir`
/// grants read and write together and the sources have to be readable. Until
/// now the only thing keeping those tools on the contracted output path was a
/// line of prose in the prompt, which is precisely what a poisoned source
/// document can talk the model out of. Comparing this map across the run is
/// what makes SPEC §4's "the app never destroys source material" checkable
/// rather than merely stated.
pub fn fingerprint_sources(class_dir: &Path) -> HashMap<String, (u64, i64)> {
    let mut out = HashMap::new();
    fingerprint_walk(class_dir, class_dir, 0, &mut out);
    out
}

fn fingerprint_walk(dir: &Path, class_dir: &Path, depth: usize, out: &mut HashMap<String, (u64, i64)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // `.classhub` holds the extracts a job legitimately writes, and the
        // guides folder is the other contracted destination.
        if depth == 0 && (name == crate::db::GUIDES_DIR || name.starts_with('.')) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            fingerprint_walk(&path, class_dir, depth + 1, out);
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        if let Ok(rel) = path.strip_prefix(class_dir) {
            out.insert(rel.to_string_lossy().into_owned(), (meta.len(), mtime));
        }
    }
}

/// Paths present in `before` that the run changed or removed, plus anything it
/// added outside the contract. Sorted, so the audit row reads the same way twice.
pub fn diff_fingerprints(
    before: &HashMap<String, (u64, i64)>,
    after: &HashMap<String, (u64, i64)>,
) -> Vec<String> {
    let mut touched: Vec<String> = before
        .iter()
        .filter(|(rel, sig)| after.get(*rel).map_or(true, |now| now != *sig))
        .map(|(rel, _)| rel.clone())
        .chain(
            after
                .keys()
                .filter(|rel| !before.contains_key(*rel))
                .cloned(),
        )
        .collect();
    touched.sort();
    touched.dedup();
    touched
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

/// Cap on file lines in a prompt's tree listing — generous for a class
/// folder, bounded if one ever grows huge (folders are always all listed).
const MAX_TREE_FILES: usize = 200;

/// Depth-first prompt listing (`dir/` lines, then files) applying this
/// module's exclusions: app-managed dirs at the top level, hidden entries and
/// symlinks everywhere. File lines stop at MAX_TREE_FILES; folders are always
/// listed. Feeds the sort and syllabus-scan job prompts.
pub(crate) fn walk_tree(
    dir: &Path,
    class_dir: &Path,
    depth: usize,
    out: &mut Vec<String>,
    file_count: &mut usize,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        if depth == 0 && APP_MANAGED_DIRS.contains(&name.as_str()) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            dirs.push(entry.path());
        } else {
            files.push(entry.path());
        }
    }
    let by_name = |a: &PathBuf, b: &PathBuf| {
        a.to_string_lossy()
            .to_lowercase()
            .cmp(&b.to_string_lossy().to_lowercase())
    };
    dirs.sort_by(by_name);
    files.sort_by(by_name);

    for path in dirs {
        if let Ok(rel) = path.strip_prefix(class_dir) {
            out.push(format!("{}/", rel.to_string_lossy()));
        }
        walk_tree(&path, class_dir, depth + 1, out, file_count);
    }
    for path in files {
        *file_count += 1;
        if *file_count == MAX_TREE_FILES + 1 {
            out.push("… (more files omitted)".to_string());
        }
        if *file_count > MAX_TREE_FILES {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(class_dir) {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
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
