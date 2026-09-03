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
    /// The file's extract, when the index holds one made from the file as it
    /// is now — what a notebook opens as in the viewer (SPEC §12). Absent for
    /// a file the pipeline has not reached yet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extract_rel_path: Option<String>,
    /// A folder named the way a division is — a kind and a number, `Module
    /// 1` — read by `units::label_number`, the same read SPEC §7.2 matches a
    /// folder to a division by. A guide over such a folder is a module guide
    /// (SPEC §8.1); a folder named for a kind of file or for the calendar
    /// (`Slides`, `Weeks`) is storage, and what it holds reaches a guide
    /// through the division that reads it, so the Materials tree offers a
    /// guide only here (SPEC §8.3).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub labelled: bool,
    pub children: Vec<TreeNode>,
}

struct ScannedFile {
    rel_path: String,
    sha256: String,
    size: i64,
    mtime: i64,
    kind: String,
}

/// What the index already knows about a file, so an unchanged one is not
/// re-hashed and a current extract can be named on its tree node.
struct Indexed {
    size: i64,
    mtime: i64,
    sha256: String,
    extract_rel_path: Option<String>,
    extracted_sha256: Option<String>,
}

type ExistingIndex = HashMap<String, Indexed>;

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

/// What a scan found, and whether it changed anything beyond the tree.
pub struct Scan {
    pub tree: Vec<TreeNode>,
    /// The index changed — a file added, changed or removed, or a lecture
    /// forgotten or refiled (SPEC §8.5) — so staleness, the divisions' counts
    /// and the lectures read rows the scan just rewrote, which the tree's own
    /// refetch does not reach.
    pub changed: bool,
}

/// Walks the class folder, syncs the `files` table (upsert added/changed, delete removed),
/// settles what a vanished transcript leaves behind, and returns the module/file tree.
/// Takes the connection itself, in two short windows rather than one long one:
/// the walk in between recurses the whole class folder and SHA-256s everything
/// that changed, and holding the app's single connection across it blocked
/// every other command, every chat tool, and the job runner for the duration.
pub fn scan_class(
    db: &std::sync::Mutex<Connection>,
    class_id: i64,
) -> Result<Scan> {
    let (dir, existing) = {
        let conn = crate::db::lock(db);
        let dir = class_dir(&conn, class_id)?;
        if !dir.is_dir() {
            bail!("class folder not found: {}", dir.display());
        }
        let existing = load_existing(&conn, class_id)?;
        (dir, existing)
    };

    let mut files = Vec::new();
    let tree = walk_dir(&dir, &dir, 0, &existing, &mut files)
        .with_context(|| format!("scanning {}", dir.display()))?;

    let seen: HashSet<&str> = files.iter().map(|f| f.rel_path.as_str()).collect();
    let mut vanished: Vec<&String> = existing
        .keys()
        .filter(|rel_path| !seen.contains(rel_path.as_str()))
        .collect();
    vanished.sort();
    let mut changed = !vanished.is_empty();
    let mut conn = crate::db::lock(db);
    let mut tx = conn.transaction()?;
    {
        let mut upsert = tx.prepare(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(class_id, rel_path) DO UPDATE SET
               sha256 = excluded.sha256, size = excluded.size,
               mtime = excluded.mtime, kind = excluded.kind",
        )?;
        for f in &files {
            changed |= existing.get(&f.rel_path).is_none_or(|i| i.sha256 != f.sha256);
            upsert.execute(params![class_id, f.rel_path, f.sha256, f.size, f.mtime, f.kind])?;
        }
    }
    // A transcript the index held and the walk did not is a lecture moved in
    // Finder — the same content somewhere the walk found — or one deleted
    // there (SPEC §8.5). Each is settled on its own savepoint, the index row
    // included, so one that cannot follow — a refile onto a held note — is
    // logged and left as it was, path still in the index, and the next scan
    // sees it vanish again and tries again. A path the index already holds
    // counts as where it went when nothing is keyed by it, which is what the
    // destination of a refused refile looks like on that next scan. What each
    // leaves for the filesystem is applied once the whole scan has committed.
    let mut by_hash: HashMap<&str, Vec<&str>> = HashMap::new();
    for f in &files {
        by_hash.entry(f.sha256.as_str()).or_default().push(f.rel_path.as_str());
    }
    // A move is one lecture's content turning up in one place. Two vanished
    // lectures with one content, or one content found twice, are not a move
    // the index can name, and are settled as gone rather than onto a guess.
    let mut left_by_hash: HashMap<&str, usize> = HashMap::new();
    for rel_path in &vanished {
        if let Some(indexed) = existing.get(*rel_path) {
            if crate::lectures::keyed_by(&tx, class_id, rel_path)? {
                *left_by_hash.entry(indexed.sha256.as_str()).or_default() += 1;
            }
        }
    }
    let mut effects = Vec::new();
    // A row that goes takes its mirror entries with it (SPEC §7 step 1): an
    // extract nothing points at would go on answering chat's search for a
    // file that is not there. Whether the file was deleted, moved — the walk
    // found its new path as new material, and the pipeline extracts it again
    // — or parked under an app-managed folder, which the sorter starts fresh,
    // the entries under the old path are dead weight. Removed after the
    // commit, and only for a row that stayed deleted.
    let mut cleared: Vec<&str> = Vec::new();
    for rel_path in &vanished {
        let savepoint = tx.savepoint()?;
        savepoint.execute(
            "DELETE FROM files WHERE class_id = ?1 AND rel_path = ?2",
            params![class_id, rel_path],
        )?;
        if !crate::lectures::keyed_by(&savepoint, class_id, rel_path)? {
            savepoint.commit()?;
            cleared.push(rel_path);
            continue;
        }
        let indexed = existing.get(*rel_path);
        let mut candidates = Vec::new();
        for candidate in indexed.and_then(|i| by_hash.get(i.sha256.as_str())).into_iter().flatten() {
            if !crate::lectures::keyed_by(&savepoint, class_id, candidate)? {
                candidates.push(*candidate);
            }
        }
        let alone = indexed.is_some_and(|i| left_by_hash.get(i.sha256.as_str()) == Some(&1));
        let moved_to = match candidates.as_slice() {
            [only] if alone => Some(*only),
            _ => None,
        };
        // Dragged into `_Inbox/` to be sorted again, most likely: the walk
        // skips the app-managed folders, so the transcript vanishes from the
        // index while still on disk. Its rows and its note wait for the
        // sorter to bring it back rather than being forgotten.
        if moved_to.is_none()
            && indexed.is_some_and(|i| parked_in_app_managed(&dir, i.size, &i.sha256))
        {
            eprintln!("scan: {rel_path} is resting under an app-managed folder; its lecture waits");
            savepoint.commit()?;
            cleared.push(rel_path);
            continue;
        }
        match crate::lectures::lecture_left(&savepoint, class_id, &dir, rel_path, moved_to) {
            Ok(effect) => {
                savepoint.commit()?;
                cleared.push(rel_path);
                effects.extend(effect);
            }
            Err(e) => eprintln!(
                "scan: {rel_path} left the tree and its lecture could not follow; it stays in \
                 the index for the next scan to try again: {e:#}"
            ),
        }
    }
    // A declared division knows its name and its dates; only the tree knows
    // where its material sits. Joined on every scan so it stays current without
    // a second thing to press (SPEC §8.1). The folders themselves are not
    // divisions — a course declares those, and the Materials tree above already
    // shows what is on disk.
    let folders: Vec<String> = tree
        .iter()
        .filter(|node| node.dir)
        .map(|node| node.name.clone())
        .collect();
    changed |= crate::units::attach_folder_paths(&tx, class_id, &folders)?;
    tx.commit()?;
    // The connection is the app's only one; what follows is filesystem work
    // and needs none of it.
    drop(conn);
    for effect in effects {
        effect.apply();
    }
    for rel_path in cleared {
        crate::extract::remove_mirror(&dir, rel_path);
    }
    Ok(Scan { tree, changed })
}

/// Whether content the index held — a vanished transcript's size and hash —
/// is resting under one of the app-managed folders the walk skips. Read flat,
/// the way the sorter reads the inbox, and hashed only where the size already
/// matches, so a scan that lost nothing keyed pays nothing here.
fn parked_in_app_managed(class_dir: &Path, size: i64, sha256: &str) -> bool {
    APP_MANAGED_DIRS.iter().any(|folder| {
        fs::read_dir(class_dir.join(folder))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| {
                entry
                    .metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.len() as i64 == size)
            })
            .any(|entry| hash_file(&entry.path()).is_ok_and(|hash| hash == sha256))
    })
}

/// Cap on vocabulary entries in a prompt — the shared names are the point, and
/// a long tail of one-off folders would drown them.
const MAX_VOCABULARY: usize = 30;

/// Folder names in use across every class, with how many classes use each.
///
/// This is the shared vocabulary, and it exists because a sorter otherwise sees
/// only its own class's tree. With no sibling context the first sort in each
/// class coins a name in isolation, which is how the four classes ended up
/// holding the same kind of document under two different names. Sorted
/// most-shared first: that is the order convergence should follow.
///
/// Counted case-insensitively and reported in whichever casing is most used:
/// "Slides" in one class and "slides" in another are one convention drifting,
/// not two names, and counting them apart understates exactly the convergence
/// this list exists to measure.
///
/// Read from the file index rather than the disk, so it costs a query rather
/// than four directory walks. The one thing that misses is an empty folder —
/// which is not a convention worth propagating anyway.
pub fn folder_vocabulary(conn: &Connection) -> Result<Vec<(String, usize)>> {
    let mut stmt = conn.prepare("SELECT class_id, rel_path FROM files")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    // lowercased name -> (the classes using it, how often each spelling appears)
    let mut by_name: HashMap<String, (HashSet<i64>, HashMap<String, usize>)> = HashMap::new();
    for row in rows {
        let (class_id, rel_path) = row?;
        let mut segments: Vec<&str> = rel_path.split('/').collect();
        // The last segment is the file itself, never a folder.
        segments.pop();
        for segment in segments {
            if segment.is_empty() {
                continue;
            }
            let entry = by_name.entry(segment.to_lowercase()).or_default();
            entry.0.insert(class_id);
            *entry.1.entry(segment.to_string()).or_default() += 1;
        }
    }
    let mut names: Vec<(String, usize)> = by_name
        .into_values()
        .map(|(classes, spellings)| {
            let mut spellings: Vec<(String, usize)> = spellings.into_iter().collect();
            // Most used first; alphabetical breaks a tie, so the answer does
            // not depend on hash order.
            spellings.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            (spellings.remove(0).0, classes.len())
        })
        .collect();
    names.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    names.truncate(MAX_VOCABULARY);
    Ok(names)
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
    let mut stmt = conn.prepare(
        "SELECT rel_path, size, mtime, sha256, extract_rel_path, extracted_sha256
         FROM files WHERE class_id = ?1",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                Indexed {
                    size: row.get(1)?,
                    mtime: row.get(2)?,
                    sha256: row.get(3)?,
                    extract_rel_path: row.get(4)?,
                    extracted_sha256: row.get(5)?,
                },
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
            let labelled = crate::units::label_number(&name).is_some();
            dirs.push(TreeNode {
                name,
                rel_path,
                dir: true,
                kind: None,
                size: None,
                extract_rel_path: None,
                labelled,
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
            let indexed = existing.get(&rel_path);
            let sha256 = match indexed {
                Some(i) if i.size == size && i.mtime == mtime => i.sha256.clone(),
                _ => hash_file(&path)
                    .with_context(|| format!("hashing {}", path.display()))?,
            };
            // An extract counts only while it was made from this very content;
            // a changed file's extract is the previous version's.
            let extract_rel_path = indexed
                .filter(|i| i.extracted_sha256.as_deref() == Some(sha256.as_str()))
                .and_then(|i| i.extract_rel_path.clone());
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
                extract_rel_path,
                labelled: false,
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
        "docx" => "docx",
        "rmd" => "rmd",
        "r" => "r",
        "py" => "py",
        "ipynb" => "ipynb",
        "html" | "htm" => "html",
        "md" => "md",
        "csv" => "csv",
        // Caption tracks a lecture arrived with, kept alongside the normalized
        // markdown so the original is never the thing that got thrown away.
        "vtt" | "srt" => "caption",
        _ if crate::transcribe::is_media(path) => "media",
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

#[cfg(test)]
mod tests {
    use super::{diff_fingerprints, fingerprint_sources, folder_vocabulary, scan_class};
    use std::collections::HashMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use rusqlite::{params, Connection};

    /// Applied Generative AI's shape in a scratch folder: Parts over week
    /// ranges and no week rows, the class folder where the settings say it is.
    fn part_numbered_class(name: &str) -> (Mutex<Connection>, PathBuf) {
        let root = std::env::temp_dir().join(format!("{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let conn = crate::db::memory_db();
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");
        let folder: String = conn
            .query_row("SELECT folder_name FROM classes WHERE id = 4", [], |row| row.get(0))
            .expect("class");
        let dir = root.join(folder);
        fs::create_dir_all(&dir).expect("class dir");
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (37, 4, 1, 'part', 'Part I: Deep Learning', 1, 1, 8, 'syllabus'),
                    (38, 4, 2, 'part', 'Part II: Alignment', 2, 9, 12, 'syllabus')",
            [],
        )
        .expect("parts");
        (Mutex::new(conn), dir)
    }

    fn write(path: PathBuf, content: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        fs::write(path, content).expect("write");
    }

    const PART_I: (i64, &str) = (37, "Part I: Deep Learning");
    const PART_II: (i64, &str) = (38, "Part II: Alignment");

    /// A transcript on disk with what a filing and a digest leave behind: a
    /// contribution row on the given Part, a note at the Part's corpus path, a
    /// session row and its two documents, named for the transcript's stem.
    /// Returns the note and the documents, class-relative.
    fn filed_and_digested(
        db: &Mutex<Connection>,
        dir: &Path,
        transcript: &str,
        unit: (i64, &str),
    ) -> (String, String, String) {
        let conn = db.lock().expect("db");
        write(dir.join(transcript), &format!("# Lecture {transcript}\n\n## 00:00\n\nHello.\n"));
        let note = crate::lectures::corpus_rel_path(unit.1, transcript);
        write(dir.join(&note), "# note");
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (4, ?1, ?2, 0, 0, 1, 1, ?3, 'Topic', 'high', 'applied', 0)",
            params![unit.0, transcript, note],
        )
        .expect("row");
        // Named for the whole path, so two same-named transcripts in two
        // weeks get documents of their own.
        let stem = transcript.trim_end_matches(".md").replace('/', " ");
        let html = format!("Study Guides/Sessions/{stem} — Topic.html");
        let md = format!("Study Guides/Sessions/{stem} — Topic.md");
        write(dir.join(&html), "<html></html>");
        write(dir.join(&md), "# session");
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (4, ?1, ?2, 1, '[]')",
            params![format!("session:{transcript}"), html],
        )
        .expect("session row");
        (note, html, md)
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table} WHERE class_id = 4"), [], |row| row.get(0))
            .expect("count")
    }

    /// SPEC §8.5: a transcript deleted in Finder leaves nothing keyed by its
    /// path once the scan has run — no contribution row, no note, no session
    /// row and no session document — and a second scan changes nothing.
    #[test]
    fn a_scan_forgets_a_transcript_deleted_in_finder() {
        let (db, dir) = part_numbered_class("classhub-scan-forget");
        let transcript = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let (note, html, md) = filed_and_digested(&db, &dir, transcript, PART_I);

        let first = scan_class(&db, 4).expect("scan");
        assert!(first.changed, "the transcript was indexed");
        assert!(first.tree.iter().any(|node| node.name == "Weeks"));
        assert_eq!(count(&db.lock().expect("db"), "lecture_contributions"), 1);

        fs::remove_file(dir.join(transcript)).expect("delete in Finder");
        let second = scan_class(&db, 4).expect("scan");
        assert!(second.changed, "a deleted transcript is a change");
        {
            let conn = db.lock().expect("db");
            assert_eq!(count(&conn, "files"), 0);
            assert_eq!(count(&conn, "lecture_contributions"), 0, "the row outlived its transcript");
            assert_eq!(count(&conn, "guides"), 0, "the session row outlived its transcript");
        }
        assert!(!dir.join(&note).exists(), "the note outlived its transcript");
        let corpus_folder = dir.join(&note).parent().expect("folder").to_path_buf();
        assert!(!corpus_folder.exists(), "the emptied corpus folder stayed as clutter");
        assert!(!dir.join(&html).exists() && !dir.join(&md).exists(), "the session documents outlived it");

        let third = scan_class(&db, 4).expect("scan");
        assert!(!third.changed, "an unchanged tree is not a change");
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// A deck deleted from a week folder is the index's business alone: the
    /// lectures beside it keep their rows. An edit to a file the walk still
    /// finds — same path, new content — is a change, and an unchanged tree
    /// is not.
    #[test]
    fn a_deleted_deck_and_an_edited_transcript_leave_the_lectures_alone() {
        let (db, dir) = part_numbered_class("classhub-scan-deck");
        let transcript = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let (note, html, _md) = filed_and_digested(&db, &dir, transcript, PART_I);
        write(dir.join("Weeks/Week 04/deck.pdf"), "%PDF-1.4 a deck");
        scan_class(&db, 4).expect("scan");

        fs::remove_file(dir.join("Weeks/Week 04/deck.pdf")).expect("delete the deck");
        let scan = scan_class(&db, 4).expect("scan");
        assert!(scan.changed, "a deleted deck is a change");
        {
            let conn = db.lock().expect("db");
            assert_eq!(count(&conn, "files"), 1);
            assert_eq!(count(&conn, "lecture_contributions"), 1, "the deck took a row with it");
            assert_eq!(count(&conn, "guides"), 1, "the deck took the session row with it");
        }
        assert!(dir.join(&note).is_file() && dir.join(&html).is_file());

        fs::write(dir.join(transcript), "# Lecture\n\n## 00:00\n\nCorrected.\n").expect("edit");
        assert!(scan_class(&db, 4).expect("scan").changed, "a content edit is a change");
        assert!(!scan_class(&db, 4).expect("scan").changed, "an unchanged tree is not");
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// A file the index no longer holds takes its mirror entries with it —
    /// the extract, the conversion and its sidecar — and the mirror folders
    /// it emptied, up to the root, which stays. Left behind, an extract
    /// nothing points at goes on answering chat's search for a file that is
    /// not there.
    #[test]
    fn a_vanished_file_takes_its_mirror_entries_with_it() {
        let (db, dir) = part_numbered_class("classhub-scan-mirror");
        let deck = "Weeks/Week 04/deck.pptx";
        write(dir.join(deck), "PK a deck");
        let mirror = dir.join(".classhub/extracts");
        for suffix in crate::extract::MIRROR_SUFFIXES {
            write(mirror.join(format!("{deck}{suffix}")), "made from the deck");
        }
        scan_class(&db, 4).expect("scan");

        fs::remove_file(dir.join(deck)).expect("delete the deck");
        assert!(scan_class(&db, 4).expect("scan").changed);
        for suffix in crate::extract::MIRROR_SUFFIXES {
            assert!(!mirror.join(format!("{deck}{suffix}")).exists(), "{suffix} outlived its source");
        }
        assert!(!mirror.join("Weeks").exists(), "the emptied mirror folders stayed as clutter");
        assert!(mirror.is_dir(), "the mirror root is never pruned");
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// The tree marks a folder named as a division is — a kind and a number —
    /// and not one named for a kind of file or for the calendar: `Module 1`
    /// is a guide's scope in the Materials tree, `Slides` and `Weeks` are
    /// storage (SPEC §8.3).
    #[test]
    fn a_folder_named_as_a_division_is_labelled_and_a_storage_one_is_not() {
        let (db, dir) = part_numbered_class("classhub-scan-labelled");
        write(dir.join("Module 1/notes.pdf"), "%PDF");
        write(dir.join("Slides/deck.pdf"), "%PDF");
        write(dir.join("Weeks/Week 04/deck.pdf"), "%PDF");
        let tree = scan_class(&db, 4).expect("scan").tree;
        let labelled = |name: &str| tree.iter().find(|n| n.name == name).expect(name).labelled;
        assert!(labelled("Module 1"));
        assert!(!labelled("Slides"));
        assert!(!labelled("Weeks"));
        let file = &tree.iter().find(|n| n.name == "Module 1").expect("folder").children[0];
        assert!(!file.dir && !file.labelled, "a file carries no label");
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// A transcript with a session document and no contribution row — one
    /// the sorter filed and the digest read before any division claimed it —
    /// is keyed by its path all the same, and its session row and documents
    /// go with it.
    #[test]
    fn a_session_row_without_a_contribution_row_goes_with_its_transcript() {
        let (db, dir) = part_numbered_class("classhub-scan-session-only");
        let transcript = "Weeks/Week 04/2026-09-15 — Lecture.md";
        write(dir.join(transcript), "# Lecture\n");
        let html = "Study Guides/Sessions/2026-09-15 — Topic.html";
        let md = "Study Guides/Sessions/2026-09-15 — Topic.md";
        write(dir.join(html), "<html></html>");
        write(dir.join(md), "# session");
        db.lock()
            .expect("db")
            .execute(
                "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
                 VALUES (4, ?1, ?2, 1, '[]')",
                params![format!("session:{transcript}"), html],
            )
            .expect("session row");
        scan_class(&db, 4).expect("scan");

        fs::remove_file(dir.join(transcript)).expect("delete in Finder");
        let scan = scan_class(&db, 4).expect("scan");
        assert!(scan.changed);
        assert_eq!(count(&db.lock().expect("db"), "guides"), 0, "the session row outlived its transcript");
        assert!(!dir.join(html).exists() && !dir.join(md).exists(), "the documents outlived it");
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// A transcript dragged into `_Inbox/` to be sorted again vanishes from
    /// the index — the walk skips the app-managed folders — while still on
    /// disk. Its row, its note and its session document wait for the sorter
    /// to bring it back rather than being forgotten.
    #[test]
    fn a_scan_leaves_a_transcript_parked_in_the_inbox_alone() {
        let (db, dir) = part_numbered_class("classhub-scan-parked");
        let transcript = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let (note, html, _md) = filed_and_digested(&db, &dir, transcript, PART_I);
        let extract = dir.join(format!(".classhub/extracts/{transcript}.md"));
        write(extract.clone(), "# extract");
        scan_class(&db, 4).expect("scan");

        fs::create_dir_all(dir.join("_Inbox")).expect("inbox");
        fs::rename(dir.join(transcript), dir.join("_Inbox/2026-09-15 — Lecture.md")).expect("park");
        let scan = scan_class(&db, 4).expect("scan");
        assert!(scan.changed, "the index lost a file");
        // The row went with the walk, and the mirror with the row: the sorter
        // starts an inbox file fresh, so nothing would have reused the entry.
        assert!(!extract.exists(), "a parked file's extract stayed");
        let conn = db.lock().expect("db");
        assert_eq!(count(&conn, "files"), 0, "the index reflects the walk");
        assert_eq!(count(&conn, "lecture_contributions"), 1, "the row was forgotten");
        assert_eq!(count(&conn, "guides"), 1, "the session row was forgotten");
        assert!(dir.join(&note).is_file() && dir.join(&html).is_file(), "the note or the document went");
        drop(conn);
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// A refile the rules refuse — the moved transcript would take a note
    /// another lecture of the Part already holds — rolls back on its own
    /// savepoint with the path still in the index, so the row, the note and
    /// the session row are exactly as they were and the next scan tries again
    /// and is refused again, rather than forgetting a lecture that is on disk.
    #[test]
    fn a_refused_settle_keeps_the_path_in_the_index_for_the_next_scan() {
        let (db, dir) = part_numbered_class("classhub-scan-refused");
        let held = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let mover = "Weeks/Week 09/2026-09-15 — Lecture.md";
        filed_and_digested(&db, &dir, held, PART_I);
        let (note, html, _md) = filed_and_digested(&db, &dir, mover, PART_II);
        let extract = dir.join(format!(".classhub/extracts/{mover}.md"));
        write(extract.clone(), "# extract");
        scan_class(&db, 4).expect("scan");

        // Into Part I, under the name Part I already holds a note for.
        let dest = "Weeks/Week 05/2026-09-15 — Lecture.md";
        fs::create_dir_all(dir.join("Weeks/Week 05")).expect("week dir");
        fs::rename(dir.join(mover), dir.join(dest)).expect("move in Finder");
        for pass in 1..=2 {
            let scan = scan_class(&db, 4).expect("scan");
            assert!(scan.changed, "pass {pass}: the index lost a path");
            let conn = db.lock().expect("db");
            let (unit_id, corpus): (i64, String) = conn
                .query_row(
                    "SELECT unit_id, corpus_rel_path FROM lecture_contributions WHERE class_id = 4 AND rel_path = ?1",
                    [mover],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .expect("the row was forgotten");
            assert_eq!((unit_id, corpus.as_str()), (38, note.as_str()), "pass {pass}: the row moved");
            assert!(dir.join(&note).is_file() && dir.join(&html).is_file(), "pass {pass}: the note or document went");
            let sessions: String = conn
                .query_row("SELECT scope FROM guides WHERE class_id = 4 AND rel_path = ?1", [&html], |row| row.get(0))
                .expect("the session row");
            assert_eq!(sessions, format!("session:{mover}"), "pass {pass}: the session row was rekeyed");
            let indexed = |rel: &str| -> i64 {
                conn.query_row("SELECT COUNT(*) FROM files WHERE class_id = 4 AND rel_path = ?1", [rel], |row| row.get(0))
                    .expect("files")
            };
            assert_eq!(indexed(mover), 1, "pass {pass}: the path left the index, so nothing will try again");
            assert_eq!(indexed(dest), 1, "pass {pass}: the destination is on disk");
            assert!(extract.is_file(), "pass {pass}: a row that stayed lost its extract");
        }
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// Two lectures with one content that both vanish while one copy turns
    /// up are not a move the index can name: both are settled as gone, and
    /// the copy is a plain new file with no row, rather than one refile
    /// re-keying a session row the other then deletes.
    #[test]
    fn two_lectures_with_one_content_are_not_a_move() {
        let (db, dir) = part_numbered_class("classhub-scan-twins");
        let first = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let second = "Weeks/Week 09/2026-09-22 — Lecture.md";
        let (note_a, html_a, _) = filed_and_digested(&db, &dir, first, PART_I);
        let (note_b, html_b, _) = filed_and_digested(&db, &dir, second, PART_II);
        for path in [first, second] {
            fs::write(dir.join(path), "# the same recording twice\n").expect("twin");
        }
        scan_class(&db, 4).expect("scan");

        let copy = "Weeks/Week 05/2026-09-15 — Lecture.md";
        fs::create_dir_all(dir.join("Weeks/Week 05")).expect("week dir");
        fs::rename(dir.join(first), dir.join(copy)).expect("move one");
        fs::remove_file(dir.join(second)).expect("delete the other");
        let scan = scan_class(&db, 4).expect("scan");
        assert!(scan.changed);
        let conn = db.lock().expect("db");
        assert_eq!(count(&conn, "lecture_contributions"), 0, "a twin was taken for a move");
        assert_eq!(count(&conn, "guides"), 0, "a session row survived");
        for path in [&note_a, &note_b, &html_a, &html_b] {
            assert!(!dir.join(path).exists(), "{path} outlived its lecture");
        }
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE class_id = 4 AND rel_path = ?1", [copy], |row| row.get(0))
            .expect("files");
        assert_eq!(indexed, 1, "the copy is a plain new file");
        drop(conn);
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// The same content at a path the index did not hold is the transcript
    /// moved in Finder, and the scan refiles it as an approved move would: the
    /// row follows to the Part the new week feeds, the note goes with it, and
    /// the session row is keyed by the new path with its documents kept.
    #[test]
    fn a_scan_refiles_a_transcript_moved_in_finder() {
        let (db, dir) = part_numbered_class("classhub-scan-move");
        let from = "Weeks/Week 04/2026-09-15 — Lecture.md";
        let to = "Weeks/Week 09/2026-09-15 — Lecture.md";
        let (note, html, _md) = filed_and_digested(&db, &dir, from, PART_I);
        let old_extract = dir.join(format!(".classhub/extracts/{from}.md"));
        write(old_extract.clone(), "# extract");
        scan_class(&db, 4).expect("scan");

        fs::create_dir_all(dir.join("Weeks/Week 09")).expect("week dir");
        fs::rename(dir.join(from), dir.join(to)).expect("move in Finder");
        let scan = scan_class(&db, 4).expect("scan");
        assert!(scan.changed);
        assert!(!old_extract.exists(), "the old path's extract outlived the move");

        let conn = db.lock().expect("db");
        let (unit_id, corpus): (i64, String) = conn
            .query_row(
                "SELECT unit_id, corpus_rel_path FROM lecture_contributions WHERE class_id = 4 AND rel_path = ?1",
                [to],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the row followed");
        assert_eq!(unit_id, 38, "Week 9 feeds Part II");
        assert_eq!(count(&conn, "lecture_contributions"), 1);
        assert!(!dir.join(&note).exists(), "the note stayed behind");
        assert_eq!(fs::read_to_string(dir.join(&corpus)).expect("moved note"), "# note");
        let scope: String = conn
            .query_row("SELECT scope FROM guides WHERE class_id = 4", [], |row| row.get(0))
            .expect("session row");
        assert_eq!(scope, format!("session:{to}"));
        assert!(dir.join(&html).is_file(), "the session document went with a move");
        let indexed: i64 = conn
            .query_row("SELECT COUNT(*) FROM files WHERE class_id = 4 AND rel_path = ?1", [to], |row| row.get(0))
            .expect("files");
        assert_eq!(indexed, 1);
        drop(conn);
        let _ = fs::remove_dir_all(dir.parent().expect("root"));
    }

    /// The shared vocabulary, which is what stops each class being sorted in
    /// isolation and coining its own word for the same kind of material.
    #[test]
    fn counts_folder_names_by_how_many_classes_use_them() {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE files (class_id INTEGER, rel_path TEXT);
             INSERT INTO files (class_id, rel_path) VALUES
               (1, 'Syllabus/syllabus.pdf'),
               (1, 'Module 1/Slides/week1.pptx'),
               (1, 'Module 1/Reading Material/paper.pdf'),
               (2, 'Syllabus/syllabus.pdf'),
               (2, 'Slides/intro.pptx'),
               (3, 'Course Info/syllabus.pdf'),
               (3, 'slides/lecture.pptx'),
               (3, 'loose-at-the-root.pdf');",
        )
        .expect("fixture");

        let vocabulary = folder_vocabulary(&conn).expect("vocabulary");
        let names: Vec<&str> = vocabulary.iter().map(|(n, _)| n.as_str()).collect();
        let count = |name: &str| {
            vocabulary
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, c)| *c)
                .unwrap_or(0)
        };

        // Counted per class, not per file: the point is how widely a name is
        // shared, and one class with fifty decks does not make "Slides" a
        // convention.
        assert_eq!(count("Syllabus"), 2);
        assert_eq!(count("Reading Material"), 1);
        assert_eq!(count("Course Info"), 1);
        // Casing drift is one convention, not two names — class 3's "slides"
        // is the same folder as the other two's "Slides", and the list reports
        // the spelling most of them use.
        assert_eq!(count("Slides"), 3);
        assert_eq!(count("slides"), 0);

        // Most-shared first — the order convergence should follow.
        assert!(
            names.iter().position(|n| *n == "Syllabus").unwrap()
                < names.iter().position(|n| *n == "Course Info").unwrap(),
            "{names:?}"
        );
        // A file at the class root contributes no folder name, and the file
        // itself is never mistaken for one.
        assert!(!names.contains(&"loose-at-the-root.pdf"), "{names:?}");
        assert!(!names.contains(&"syllabus.pdf"), "{names:?}");
    }

    /// The kind is what the extract routes and the viewer key on; a
    /// misspelt or missing arm here makes a format silently `other`.
    #[test]
    fn names_every_kind_by_extension_case_insensitively() {
        use std::path::Path;
        for (name, kind) in [
            ("deck.pptx", "pptx"),
            ("paper.PDF", "pdf"),
            ("Notes .docx", "docx"),
            ("lab.Rmd", "rmd"),
            ("script.R", "r"),
            ("warmup.py", "py"),
            ("week4.ipynb", "ipynb"),
            ("report.html", "html"),
            ("report.htm", "html"),
            ("readme.md", "md"),
            ("cohort.csv", "csv"),
            ("lecture.vtt", "caption"),
            ("lecture.srt", "caption"),
            ("lecture.mp4", "media"),
            ("transcript.txt", "other"),
            ("Makefile", "other"),
        ] {
            assert_eq!(super::kind_for(Path::new(name)), kind, "{name}");
        }
    }

    fn sig(entries: &[(&str, u64, i64)]) -> HashMap<String, (u64, i64)> {
        entries
            .iter()
            .map(|(p, len, mtime)| (p.to_string(), (*len, *mtime)))
            .collect()
    }

    /// This diff is what demotes a synthesis run that wrote outside its
    /// contracted output path, so it has to notice every kind of change.
    #[test]
    fn reports_nothing_when_the_sources_are_untouched() {
        let before = sig(&[("Module 1/a.pdf", 10, 100), ("b.R", 20, 200)]);
        assert!(diff_fingerprints(&before, &before.clone()).is_empty());
    }

    #[test]
    fn reports_a_rewritten_file() {
        let before = sig(&[("a.pdf", 10, 100)]);
        let after = sig(&[("a.pdf", 11, 100)]); // size changed
        assert_eq!(diff_fingerprints(&before, &after), vec!["a.pdf"]);

        let touched = sig(&[("a.pdf", 10, 101)]); // mtime changed
        assert_eq!(diff_fingerprints(&before, &touched), vec!["a.pdf"]);
    }

    #[test]
    fn reports_a_deleted_file() {
        let before = sig(&[("a.pdf", 10, 100), ("b.R", 20, 200)]);
        let after = sig(&[("b.R", 20, 200)]);
        assert_eq!(diff_fingerprints(&before, &after), vec!["a.pdf"]);
    }

    #[test]
    fn reports_a_file_added_outside_the_contract() {
        let before = sig(&[("a.pdf", 10, 100)]);
        let after = sig(&[("a.pdf", 10, 100), ("Module 1/new.md", 5, 300)]);
        assert_eq!(diff_fingerprints(&before, &after), vec!["Module 1/new.md"]);
    }

    #[test]
    fn returns_a_sorted_deduped_list() {
        let before = sig(&[("z.pdf", 1, 1), ("a.pdf", 1, 1)]);
        let after = sig(&[("m.pdf", 1, 1)]);
        assert_eq!(
            diff_fingerprints(&before, &after),
            vec!["a.pdf", "m.pdf", "z.pdf"]
        );
    }

    /// The contracted output paths are exactly what must NOT be reported —
    /// otherwise every successful run would demote itself.
    #[test]
    fn ignores_the_contracted_output_paths() {
        let dir = std::env::temp_dir().join(format!(
            "classhub-fingerprint-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Study Guides")).unwrap();
        std::fs::create_dir_all(dir.join(".classhub/extracts")).unwrap();
        std::fs::create_dir_all(dir.join("Module 1")).unwrap();
        std::fs::create_dir_all(dir.join("Notes")).unwrap();
        std::fs::write(dir.join("Module 1/source.md"), "x").unwrap();
        // Notes are source material too — a job has no business writing here.
        std::fs::write(dir.join("Notes/mine.md"), "y").unwrap();

        let before = fingerprint_sources(&dir);
        assert!(before.contains_key("Module 1/source.md"));
        assert!(before.contains_key("Notes/mine.md"));

        // Writing the contracted outputs must leave the fingerprint unchanged.
        std::fs::write(dir.join("Study Guides/Module 1.html"), "guide").unwrap();
        std::fs::write(dir.join(".classhub/extracts/source.md"), "extract").unwrap();
        assert!(diff_fingerprints(&before, &fingerprint_sources(&dir)).is_empty());

        // Touching a source is what must be caught.
        std::fs::write(dir.join("Module 1/source.md"), "changed").unwrap();
        assert_eq!(
            diff_fingerprints(&before, &fingerprint_sources(&dir)),
            vec!["Module 1/source.md"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cap is what keeps a long tail of one-off folders — "Week 01 — …",
    /// "Week 02 — …" — from crowding the shared names out of the prompt, which
    /// are the whole reason the list is sent.
    #[test]
    fn the_vocabulary_keeps_the_names_that_are_actually_shared() {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        conn.execute_batch("CREATE TABLE files (class_id INTEGER, rel_path TEXT);")
            .expect("fixture");
        // One class with a long tail of folders only it uses.
        for week in 1..=40 {
            conn.execute(
                "INSERT INTO files (class_id, rel_path) VALUES (1, ?1)",
                [format!("Week {week:02} — Topic/notes.md")],
            )
            .expect("insert");
        }
        // And one name three classes share, added last so position cannot come
        // from insertion order.
        for class_id in 1..=3 {
            conn.execute(
                "INSERT INTO files (class_id, rel_path) VALUES (?1, 'Slides/deck.pptx')",
                [class_id],
            )
            .expect("insert");
        }

        let vocabulary = folder_vocabulary(&conn).expect("vocabulary");
        assert_eq!(vocabulary.len(), super::MAX_VOCABULARY, "the cap did not hold");
        assert_eq!(vocabulary[0], ("Slides".to_string(), 3), "{vocabulary:?}");
    }
}
