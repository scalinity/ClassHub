//! SPEC §9 — ranked search over everything the pipeline writes.
//!
//! Anthropic publishes no embeddings API (SPEC §1), so retrieval stays
//! agentic: the agent searches text the extract pipeline already wrote and
//! then reads bounded windows of what it found. What this module changes is
//! the order the hits come back in. Ripgrep answers in directory-then-line
//! order and the tool cuts the answer at eighty lines, so the model reads
//! whichever files sort first — measured on the very question M7 was accepted
//! on, the file that answered it was line 188 of 262 with thirty-four files
//! ranked ahead of it, and never reached the model at all. FTS5 with `bm25()`
//! ranks a hit by how much the term explains the document, and `snippet()`
//! returns the matched stretch rather than a whole line.
//!
//! The index is derived. Losing it costs a rebuild and never data, which is
//! what lets `sync_class` read the disk and correct itself instead of
//! depending on all nine of the pipeline's writers to remember it: a stat of
//! each file under the four searched folders, a read of only the ones whose
//! modification time or length moved, and a drop of the rows whose files are
//! gone. An unchanged tree opens no write transaction at all.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::db::{CORPUS_DIR, EXTRACTS_DIR, GUIDES_DIR, NOTES_DIR};

/// How many ranked documents a search reports.
pub const MAX_HITS: usize = 20;
/// Extra ranked rows fetched beyond `MAX_HITS`, so the duplicate drop (SPEC §7
/// step 1) has something to fall back on rather than shortening the page.
const DROP_HEADROOM: usize = 30;
/// Characters of context around the match, as `snippet()` counts tokens.
const SNIPPET_TOKENS: i64 = 18;
/// Longest document the index reads whole. The largest thing the pipeline
/// writes is a semester master of about 300 KB; past this a file is a data
/// dump that would swamp `bm25`'s length normalization, and it is skipped
/// with its path on stderr rather than indexed badly.
const MAX_DOC_BYTES: u64 = 4 * 1024 * 1024;

/// The four folders `search_material` covers, and what a hit in each is
/// called. `Study Guides/` holds the session documents, the practice exams
/// and the small documents beside the division guides; they are all the
/// app's own writing, and the model is told the path either way.
const SEARCHED: [(&str, &str); 4] = [
    (EXTRACTS_DIR, "extract"),
    (CORPUS_DIR, "corpus"),
    (NOTES_DIR, "note"),
    (GUIDES_DIR, "guide"),
];

/// One document on disk, as the reconcile found it.
struct Found {
    /// Class-relative, the way `material_index` stores it.
    rel_path: String,
    kind: &'static str,
    mtime: i64,
    size: i64,
    abs: PathBuf,
}

/// One ranked hit.
pub struct Hit {
    /// AIBHS-root-relative — a path the model can hand to `read_material`.
    pub rel_path: String,
    pub kind: String,
    pub snippet: String,
    /// The matched line, where one could be resolved; documents are indexed
    /// with their lines intact, so this is nearly always there.
    pub line: Option<usize>,
}

// ---------------------------------------------------------------------------
// What is indexed

/// Whether a file under one of the searched folders is one of the documents
/// the index holds.
///
/// Markdown everywhere; under `Study Guides/` also the HTML of a document
/// that has no markdown twin, since a division guide, the semester master, a
/// practice exam and a presentation kit are written as HTML alone (SPEC §8.1,
/// §8.2, §8.3, §8.6) and indexing twins only would drop every study guide out
/// of search. A session document, a brief, the workbook and a pre-read do have
/// twins, so their HTML is skipped — the same rule ripgrep applies today, and
/// the reason it applies it: the same prose twice, at twice the tokens, with
/// markup through the match.
///
/// `.classhub/extracts/` also holds LibreOffice's DOCX twin (`<name>.docx.html`,
/// SPEC §4) beside the extract made from it — the same prose plus every figure
/// as a base64 line. Skipped for the same reason.
fn indexed_name(dir_kind: &str, rel_path: &str) -> bool {
    let lower = rel_path.to_ascii_lowercase();
    if lower.ends_with(".md") {
        return true;
    }
    if dir_kind != "guide" || !lower.ends_with(".html") {
        return false;
    }
    // A twin beside it means the markdown is the copy to index.
    !lower.starts_with("sessions/")
        && !lower.starts_with("briefs/")
        && !lower.starts_with("workbook/")
}

/// Whether the HTML at this path has a markdown twin already indexed, checked
/// on disk rather than by folder alone — a kit or an exam that ever gains a
/// twin then stops being indexed twice with no code change.
fn has_twin(abs: &Path) -> bool {
    let Some(stem) = abs.to_str().and_then(|s| s.strip_suffix(".html")) else {
        return false;
    };
    Path::new(&format!("{stem}.md")).is_file()
}

// ---------------------------------------------------------------------------
// The reconcile

/// Every indexable document under a class's four folders, with what it looks
/// like on disk right now. A folder that is not there contributes nothing —
/// a class whose material has yet to be extracted is empty, not broken.
fn walk_class(class_dir: &Path) -> Vec<Found> {
    let mut found = Vec::new();
    for (sub, kind) in SEARCHED {
        let base = class_dir.join(sub);
        if !base.is_dir() {
            continue;
        }
        collect(&base, &base, sub, kind, &mut found);
    }
    found
}

fn collect(dir: &Path, base: &Path, sub: &str, kind: &'static str, out: &mut Vec<Found>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect(&path, base, sub, kind, out);
            continue;
        }
        if !meta.is_file() {
            continue;
        }
        let Ok(inner) = path.strip_prefix(base) else {
            continue;
        };
        let inner = inner.to_string_lossy().replace('\\', "/");
        if !indexed_name(kind, &inner) {
            continue;
        }
        if inner.to_ascii_lowercase().ends_with(".html") && has_twin(&path) {
            continue;
        }
        if meta.len() > MAX_DOC_BYTES {
            eprintln!(
                "search index: {sub}/{inner} is {} bytes — too large to index",
                meta.len()
            );
            continue;
        }
        out.push(Found {
            rel_path: format!("{sub}/{inner}"),
            kind,
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            size: meta.len() as i64,
            abs: path,
        });
    }
}

/// The document's searchable text: markdown as written, HTML through the
/// extractor's own stripper (SPEC §7 step 3), so a guide is indexed as the
/// prose a reader sees and not as its markup.
fn text_of(found: &Found) -> Result<String> {
    let raw = fs::read_to_string(&found.abs)
        .with_context(|| format!("reading {} for the search index", found.rel_path))?;
    Ok(if found.rel_path.to_ascii_lowercase().ends_with(".html") {
        crate::extract::strip_html(&raw)
    } else {
        raw
    })
}

/// What the index already holds for a class: path → (kind, mtime, size, rowid).
pub type Indexed = BTreeMap<String, (String, i64, i64, i64)>;

/// The reconcile in three steps, so the disk work happens with no connection
/// held: `known_rows` reads the index, `plan` walks and reads the tree, and
/// `apply` writes. `db::with_conn` holds the one process-wide connection for
/// the whole of its closure, and doing the walk and the reads inside it blocks
/// every other command, every other chat tool and the job runner for as long
/// as it takes — the arrangement `search_material` was restructured away from
/// when ripgrep was spawned under the lock.
pub fn known_rows(conn: &Connection, class_id: i64) -> Result<Indexed> {
    indexed_rows(conn, class_id)
}

fn indexed_rows(conn: &Connection, class_id: i64) -> Result<Indexed> {
    let mut stmt = conn.prepare(
        "SELECT rel_path, kind, mtime, size, fts_rowid FROM material_index WHERE class_id = ?1",
    )?;
    let rows = stmt.query_map([class_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            (
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ),
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Indexed>>()?)
}

/// Brings one class's index up to what is on disk, and says how many
/// documents it wrote and dropped.
///
/// The three steps in one call, for a caller that can hold the connection
/// across the disk work — the tests. Every caller in the app takes
/// `known_rows`, `plan` and `apply` separately so the reads happen with
/// nothing held.
///
/// The reads happen before the transaction and the transaction is IMMEDIATE:
/// a deferred one that had read first is refused outright, past the busy
/// handler, when the other build commits in between — M37's cards index
/// learned that. A failure here is never fatal to the caller: the index is
/// derived, and a search over a stale index is worse than a fresh one but
/// better than no answer.
#[cfg(test)]
pub fn sync_class(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<(usize, usize)> {
    let known = known_rows(conn, class_id)?;
    let work = plan(class_dir, &known)?;
    apply(conn, class_id, &work)
}

/// What one reconcile has to write and drop. The texts of the changed
/// documents are held whole until the transaction opens, which is what keeps
/// the reads out of it; the ceiling is `MAX_DOC_BYTES` times the number of
/// files that moved, and at this corpus's size — about two megabytes across
/// four classes — that is a deliberate trade rather than an oversight.
pub struct Work {
    writes: Vec<(Found, String)>,
    gone: Vec<String>,
}

impl Work {
    pub fn is_empty(&self) -> bool {
        self.writes.is_empty() && self.gone.is_empty()
    }
}

/// The disk half of the reconcile: no connection, so nothing else waits on it.
///
/// A class folder that is not a directory is refused rather than read as a
/// class whose documents are all gone. An unmounted volume, an eviction or a
/// rename in Finder would otherwise make one search delete the whole class's
/// index and answer "no documents match" — the failure M37's cards index
/// shipped and its review pass fixed, in the same shape.
pub fn plan(class_dir: &Path, known: &Indexed) -> Result<Work> {
    if !class_dir.is_dir() {
        bail!(
            "{} is not there — the search index is left as it is",
            class_dir.display()
        );
    }
    let mut writes: Vec<(Found, String)> = Vec::new();
    let mut seen = HashSet::new();
    for found in walk_class(class_dir) {
        seen.insert(found.rel_path.clone());
        if let Some((kind, mtime, size, _)) = known.get(&found.rel_path) {
            if *mtime == found.mtime && *size == found.size && kind == found.kind {
                continue;
            }
        }
        match text_of(&found) {
            Ok(text) => writes.push((found, text)),
            // A file that vanished between the walk and the read is the next
            // reconcile's to drop; one that will not decode is not worth the
            // class's whole index.
            Err(e) => eprintln!("search index: {e:#}"),
        }
    }
    let gone = known
        .keys()
        .filter(|path| !seen.contains(*path))
        .cloned()
        .collect();
    Ok(Work { writes, gone })
}

/// The write half: one IMMEDIATE transaction, and nothing read from disk.
///
/// Each document's current FTS rowid is read back **inside** the transaction
/// rather than taken from the plan's snapshot. The two builds share the
/// database, so between the plan and the write the other process may have
/// replaced the row: deleting the rowid the snapshot named would then delete
/// nothing, the insert would add a second row for the same document, and the
/// index would name only the newer one — leaving an FTS row that no later
/// reconcile and no rebuild could reach, answering searches for a document
/// twice, or for a document that is gone.
pub fn apply(conn: &Connection, class_id: i64, work: &Work) -> Result<(usize, usize)> {
    if work.is_empty() {
        return Ok((0, 0));
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for path in &work.gone {
        drop_document(&tx, class_id, path)?;
    }
    for (found, text) in &work.writes {
        drop_document(&tx, class_id, &found.rel_path)?;
        tx.execute(
            "INSERT INTO material_fts (class_id, rel_path, kind, content)
             VALUES (?1, ?2, ?3, ?4)",
            params![class_id, found.rel_path, found.kind, text],
        )?;
        let rowid = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO material_index (class_id, rel_path, kind, mtime, size, fts_rowid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(class_id, rel_path) DO UPDATE SET
               kind = excluded.kind, mtime = excluded.mtime,
               size = excluded.size, fts_rowid = excluded.fts_rowid",
            params![
                class_id,
                found.rel_path,
                found.kind,
                found.mtime,
                found.size,
                rowid
            ],
        )?;
    }
    tx.commit()?;
    Ok((work.writes.len(), work.gone.len()))
}

/// Removes a document from both tables, reading the rowid it holds now.
fn drop_document(tx: &Transaction<'_>, class_id: i64, rel_path: &str) -> Result<()> {
    let rowid: Option<i64> = tx
        .query_row(
            "SELECT fts_rowid FROM material_index WHERE class_id = ?1 AND rel_path = ?2",
            params![class_id, rel_path],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(rowid) = rowid {
        tx.execute("DELETE FROM material_fts WHERE rowid = ?1", [rowid])?;
    }
    tx.execute(
        "DELETE FROM material_index WHERE class_id = ?1 AND rel_path = ?2",
        params![class_id, rel_path],
    )?;
    Ok(())
}

/// Throws one class's index away and builds it again, for the Settings
/// rebuild — the way back from an index that a crash mid-write, or a tree
/// edited under a build that was not running, left disagreeing with the disk.
///
/// The clear is keyed on the FTS table's own `class_id`, not on the rowids
/// `material_index` happens to name: a rebuild that could only reach the rows
/// the index still points at would leave behind exactly the orphans it exists
/// to clear.
#[cfg(test)]
pub fn rebuild_class(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<usize> {
    // Planned before anything is deleted, so a class whose folder is missing
    // keeps the index it has rather than losing it to a rebuild that then
    // finds nothing to put back.
    let work = plan(class_dir, &Indexed::new())?;
    rebuild_with(conn, class_id, &work)
}

/// The write half of a rebuild, for a caller that planned the tree with no
/// connection held.
pub fn rebuild_with(conn: &Connection, class_id: i64, work: &Work) -> Result<usize> {
    {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM material_fts WHERE class_id = ?1", [class_id])?;
        tx.execute("DELETE FROM material_index WHERE class_id = ?1", [class_id])?;
        tx.commit()?;
    }
    let (written, _) = apply(conn, class_id, work)?;
    Ok(written)
}

/// How many documents the index holds, for the Settings row.
pub fn indexed_count(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT COUNT(*) FROM material_index", [], |r| r.get(0))?)
}

// ---------------------------------------------------------------------------
// The query

/// Turns what the model asked for into an FTS5 MATCH expression, or `None`
/// when nothing searchable is left — an empty query, or one that is all
/// punctuation, which is a regular expression and belongs on the fallback.
///
/// The shape follows what the tool has always invited. `|` is alternation, so
/// each alternative is ORed; the words inside one are a phrase, and a phrase
/// of more than one word is offered twice — `("central tendency" OR ("central"
/// AND "tendency"))` — so a document holding the exact phrase outranks one
/// holding both words apart, while neither is missed. Every term is quoted,
/// which is FTS5's own escape: inside a string literal a hyphen, a colon and
/// an asterisk are text rather than syntax, and a `"` is doubled.
pub fn fts_query(raw: &str) -> Option<String> {
    let mut alternatives = Vec::new();
    for part in raw.split('|') {
        let terms = terms_of(part);
        if terms.is_empty() {
            continue;
        }
        alternatives.push(if terms.len() == 1 {
            quote(&terms[0])
        } else {
            let phrase = quote(&terms.join(" "));
            let all = terms.iter().map(|t| quote(t)).collect::<Vec<_>>().join(" AND ");
            format!("({phrase} OR ({all}))")
        });
    }
    if alternatives.is_empty() {
        return None;
    }
    Some(alternatives.join(" OR "))
}

/// The words of one alternative: runs of letters, digits and the marks that
/// live inside a word. Everything else — a regex's metacharacters among it —
/// is a separator, so `mean|median` splits above and `p-values` stays whole.
fn terms_of(part: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut word = String::new();
    for ch in part.chars() {
        if ch.is_alphanumeric() || ch == '\'' || ch == '-' || ch == '_' {
            word.push(ch);
        } else if !word.is_empty() {
            terms.push(std::mem::take(&mut word));
        }
    }
    if !word.is_empty() {
        terms.push(word);
    }
    // A lone `-` or `'` swept up by the rule above carries no term.
    terms.retain(|t| t.chars().any(char::is_alphanumeric));
    terms
}

fn quote(term: &str) -> String {
    format!("\"{}\"", term.replace('"', "\"\""))
}

/// Whether the query is a regular expression the model wrote as one, so the
/// tool can take the pattern path rather than pull words out of the syntax —
/// `^\d{4}` would otherwise search for the word "d".
///
/// Only shapes that cannot be ordinary prose count. A bare `+` or `?` does
/// not: a question mark ends a sentence and a plus sits in a version number,
/// and reading either as a pattern would send half the model's queries down
/// the slow path.
pub fn looks_like_regex(raw: &str) -> bool {
    let escape = raw
        .match_indices('\\')
        .any(|(i, _)| matches!(raw[i + 1..].chars().next(), Some('d' | 'w' | 's' | 'b' | 'S' | 'W' | 'D')));
    let class = raw.find('[').is_some_and(|open| raw[open..].contains(']'));
    let quantifier = raw
        .find('{')
        .is_some_and(|open| raw[open..].contains('}') && raw[open + 1..].starts_with(|c: char| c.is_ascii_digit()));
    escape || class || quantifier || raw.contains(".*") || raw.contains(".+") || raw.contains(".?")
}

/// The line the snippet came from, 1-based: the first line of the document
/// holding any of the query's terms. `snippet()` returns a stretch of text
/// and not a position, and a citation the reader can open at the right place
/// is worth one scan of a document already in memory.
///
/// Matched on whole words rather than on substrings, and on a word that
/// *starts with* the term as well as one that equals it. Both matter: the
/// index tokenizes with `porter`, so a document can rank on the stem alone —
/// "imputation" matching a file that only ever writes "imputed" — and a
/// substring test would cite the first line holding "meaning" for a query of
/// "mean". A term no line carries gives no number rather than a wrong one.
fn line_of(content: &str, terms: &[String]) -> Option<usize> {
    if terms.is_empty() {
        return None;
    }
    let lowered: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    // The stem a `porter` match could have come from, for a term long enough
    // that suffix stripping is what happened: "imputation" and "imputed"
    // share five characters. A short term is matched whole instead, so "mode"
    // does not cite the line that says "model".
    let stems: Vec<&str> = lowered
        .iter()
        .filter(|t| t.chars().count() >= 6)
        .map(|t| &t[..t.char_indices().nth(5).map_or(t.len(), |(i, _)| i)])
        .collect();
    content
        .lines()
        .enumerate()
        .find(|(_, line)| {
            let line = line.to_lowercase();
            line.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-')
                .filter(|word| !word.is_empty())
                .any(|word| {
                    lowered.iter().any(|t| word == t)
                        || stems.iter().any(|s| word.starts_with(s))
                })
        })
        .map(|(i, _)| i + 1)
}

/// Every term the query names, in one flat list, for `line_of`.
fn all_terms(raw: &str) -> Vec<String> {
    raw.split('|').flat_map(terms_of).collect()
}

/// Runs the ranked search over the classes given, best document first.
///
/// One row per document rather than per line: `bm25()` scores a document, and
/// twenty documents each with the stretch that matched tells the model more
/// about where to read than eighty lines from whichever files sorted first.
/// A duplicate is dropped here rather than left out of the index (SPEC §7
/// step 1), so a mark that changes needs no reindex.
pub fn run(
    conn: &Connection,
    class_ids: &[i64],
    raw: &str,
    dropped: &HashSet<String>,
) -> Result<(Vec<Hit>, usize)> {
    // An empty scope is no rows, not `IN ()`, which SQLite rejects as a
    // syntax error rather than matching nothing.
    if class_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }
    let Some(expression) = fts_query(raw) else {
        return Ok((Vec::new(), 0));
    };
    let terms = all_terms(raw);
    let folders = folder_names(conn)?;
    let placeholders = class_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // The class list is built from ids read out of the database, never from
    // the model's text, so it is inlined rather than bound — `IN (?)` cannot
    // take a list, and a bound array would need a temp table for four rows.
    let scope = format!("material_fts MATCH ?1 AND class_id IN ({placeholders})");

    // Counted separately, so the header states how many documents match
    // rather than how many rows the limit let through. The model is told to
    // narrow a query on this number, and a count that saturates at the limit
    // would tell it the same thing whether fifty documents matched or five
    // hundred.
    let total: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM material_fts WHERE {scope}"),
        params![expression],
        |row| row.get(0),
    )?;

    // Ranked rows carry no content: a document's whole text is read back only
    // for the hits that survive the duplicate drop, at most `MAX_HITS` of
    // them, where selecting it here would deserialize every candidate whole.
    // The headroom above `MAX_HITS` is what the drop may consume.
    let sql = format!(
        "SELECT rowid, class_id, rel_path, kind,
                snippet(material_fts, 3, '', '', '…', {SNIPPET_TOKENS})
         FROM material_fts WHERE {scope}
         ORDER BY bm25(material_fts) LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![expression, (MAX_HITS + DROP_HEADROOM) as i64], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut kept = Vec::new();
    let mut skipped = 0usize;
    for row in rows {
        let (rowid, class_id, rel_path, kind, snippet) = row?;
        // A class the row names and `classes` does not is a row nothing can
        // open; it is left out rather than emitted under an empty folder.
        let Some(folder) = folders.get(&class_id) else {
            eprintln!("search index: row {rowid} names class {class_id}, which is not there");
            skipped += 1;
            continue;
        };
        let full = format!("{folder}/{rel_path}");
        if dropped.contains(&full) {
            skipped += 1;
            continue;
        }
        if kept.len() == MAX_HITS {
            break;
        }
        kept.push((rowid, full, kind, snippet));
    }

    let mut hits = Vec::with_capacity(kept.len());
    for (rowid, full, kind, snippet) in kept {
        let content: String = conn.query_row(
            "SELECT content FROM material_fts WHERE rowid = ?1",
            [rowid],
            |row| row.get(0),
        )?;
        hits.push(Hit {
            rel_path: full,
            kind,
            snippet: snippet.split_whitespace().collect::<Vec<_>>().join(" "),
            line: line_of(&content, &terms),
        });
    }
    // The count is over every matching row; what the duplicate drop removed
    // from the ranked page is taken off it so the two agree.
    let total = (total as usize).saturating_sub(skipped);
    Ok((hits, total))
}

/// Every class's folder, read once per search rather than once per hit.
fn folder_names(conn: &Connection) -> Result<BTreeMap<i64, String>> {
    let mut stmt = conn.prepare("SELECT id, folder_name FROM classes")?;
    let rows = stmt.query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A phrase stays a phrase and is offered beside its terms; alternation
    /// splits; a term with punctuation inside it survives quoted; a query
    /// with no words at all falls through to the caller's fallback.
    #[test]
    fn the_query_builder_reads_phrases_alternation_and_punctuation() {
        assert_eq!(
            fts_query("central tendency").as_deref(),
            Some("(\"central tendency\" OR (\"central\" AND \"tendency\"))")
        );
        assert_eq!(fts_query("bimodal").as_deref(), Some("\"bimodal\""));
        assert_eq!(
            fts_query("mean|median|mode").as_deref(),
            Some("\"mean\" OR \"median\" OR \"mode\"")
        );
        assert_eq!(
            fts_query("central tendency|mode").as_deref(),
            Some("(\"central tendency\" OR (\"central\" AND \"tendency\")) OR \"mode\"")
        );
        // FTS5 syntax inside a term is text, not syntax.
        assert_eq!(fts_query("p-values").as_deref(), Some("\"p-values\""));
        assert_eq!(fts_query("NOT*").as_deref(), Some("\"NOT\""));
        // A quote in the query is a separator like any other punctuation, so
        // it never reaches the expression; `quote` doubles one anyway, which
        // is what makes that true rather than merely likely.
        assert_eq!(
            fts_query("say \"this\"").as_deref(),
            Some("(\"say this\" OR (\"say\" AND \"this\"))")
        );
        assert_eq!(quote("a\"b"), "\"a\"\"b\"");
        // Nothing searchable: the caller falls back to ripgrep.
        assert_eq!(fts_query(""), None);
        assert_eq!(fts_query("^$"), None);
        assert_eq!(fts_query("  |  "), None);
    }

    /// A pattern is told from words before any term is pulled out of it, or
    /// `^\d{4}` would search for the word "d". Only shapes that cannot be
    /// prose count: a question mark and a plus are ordinary text.
    #[test]
    fn a_regular_expression_is_told_from_words() {
        assert!(looks_like_regex("^\\d{4}-\\d{2}"));
        assert!(looks_like_regex("\\bHIPAA\\b"));
        assert!(looks_like_regex("mean.*median"));
        assert!(looks_like_regex("[Ss]kew"));
        assert!(looks_like_regex("p{2,3}"));
        assert!(!looks_like_regex("central tendency"));
        assert!(!looks_like_regex("mean|median|mode"));
        assert!(!looks_like_regex("p-values"));
        assert!(!looks_like_regex("what counts as missing?"));
        assert!(!looks_like_regex("R 4.3+ install"));
        // A brace with no number after it is prose, not a quantifier.
        assert!(!looks_like_regex("the set {a, b}"));
    }

    /// The reported line is the first one carrying any term as a whole word,
    /// so a citation opens where the match is. A substring would cite
    /// "meaning" for "mean"; the index stems, so a document can rank on
    /// "imputed" for a query of "imputation" and still needs a line.
    #[test]
    fn the_line_is_the_first_one_a_term_is_on() {
        let doc = "# Title\n\nIntro paragraph.\nThe median is the middle value.\nMore.";
        assert_eq!(line_of(doc, &["median".into()]), Some(4));
        assert_eq!(line_of(doc, &["Median".into()]), Some(4));
        assert_eq!(line_of(doc, &["nowhere".into()]), None);
        assert_eq!(line_of(doc, &[]), None);

        // A whole word, not a substring.
        let meaning = "The meaning of it.\nThe mean is 4.\n";
        assert_eq!(line_of(meaning, &["mean".into()]), Some(2));

        // The stem the tokenizer would have matched on.
        let stemmed = "Nothing here.\nThe values were imputed twice.\n";
        assert_eq!(line_of(stemmed, &["imputation".into()]), Some(2));

        // A short term is matched whole, so "mode" does not cite "model".
        let short = "The model is fitted.\nThe mode is 7.\n";
        assert_eq!(line_of(short, &["mode".into()]), Some(2));
    }

    /// A class folder that is not there is refused, so one search cannot
    /// empty the class's index and then answer that nothing matches — the
    /// failure M37's cards index shipped, in the same shape.
    #[test]
    fn a_missing_class_folder_is_refused_rather_than_read_as_empty() {
        let conn = crate::db::memory_db();
        let dir = std::env::temp_dir().join(format!("classhub-fts-gone-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(NOTES_DIR)).expect("notes dir");
        fs::write(dir.join(NOTES_DIR).join("Kept.md"), "winsorization\n").expect("note");
        assert_eq!(sync_class(&conn, 3, &dir).expect("sync").0, 1);

        // The folder goes; the index must not follow it.
        let _ = fs::remove_dir_all(&dir);
        let refused = sync_class(&conn, 3, &dir).expect_err("refused");
        assert!(refused.to_string().contains("is not there"), "{refused}");
        assert_eq!(indexed_count(&conn).expect("count"), 1);
        let none = HashSet::new();
        assert_eq!(run(&conn, &[3], "winsorization", &none).expect("search").1, 1);

        // A rebuild refuses on the same reading rather than clearing first.
        assert!(rebuild_class(&conn, 3, &dir).is_err());
        assert_eq!(indexed_count(&conn).expect("count"), 1);
    }

    /// The reconcile and the rebuild leave no FTS row that nothing points at.
    /// A row written by the other build between the plan and the write is the
    /// real case: the rowid the plan saw is stale, and deleting it would
    /// leave the newer row behind for every later search.
    #[test]
    fn a_row_the_index_no_longer_names_is_still_reclaimed() {
        let conn = crate::db::memory_db();
        let dir = std::env::temp_dir().join(format!("classhub-fts-orphan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(NOTES_DIR)).expect("notes dir");
        fs::write(dir.join(NOTES_DIR).join("A.md"), "winsorization\n").expect("note");
        sync_class(&conn, 3, &dir).expect("sync");

        // What the other build's commit leaves behind: a second FTS row for
        // the same document, with the index naming only the newer one.
        conn.execute(
            "INSERT INTO material_fts (class_id, rel_path, kind, content)
             VALUES (3, 'Notes/A.md', 'note', 'winsorization')",
            [],
        )
        .expect("orphan");
        let none = HashSet::new();
        assert_eq!(
            run(&conn, &[3], "winsorization", &none).expect("search").1,
            2,
            "the orphan answers alongside the row the index names"
        );

        // The rebuild clears by class, so it reaches the orphan.
        assert_eq!(rebuild_class(&conn, 3, &dir).expect("rebuild"), 1);
        assert_eq!(run(&conn, &[3], "winsorization", &none).expect("search").1, 1);

        // And the reconcile reads the rowid it is replacing under the lock,
        // so a stale plan cannot leave one behind.
        let known = known_rows(&conn, 3).expect("known");
        fs::write(dir.join(NOTES_DIR).join("A.md"), "winsorization again\n").expect("rewrite");
        let work = plan(&dir, &known).expect("plan");
        // The other build gets there first, moving the rowid the plan saw.
        rebuild_class(&conn, 3, &dir).expect("other build");
        apply(&conn, 3, &work).expect("apply");
        assert_eq!(run(&conn, &[3], "winsorization", &none).expect("search").1, 1);
        assert_eq!(indexed_count(&conn).expect("count"), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    /// An empty scope is no rows rather than `IN ()`, which SQLite rejects.
    #[test]
    fn an_empty_class_list_matches_nothing() {
        let conn = crate::db::memory_db();
        let none = HashSet::new();
        assert_eq!(run(&conn, &[], "anything", &none).expect("search").1, 0);
    }

    /// Markdown everywhere; under `Study Guides/` the HTML only of the
    /// documents written without a twin, so a guide is in and a session
    /// document's HTML is its markdown's job.
    #[test]
    fn only_the_documents_without_a_twin_are_indexed_as_html() {
        assert!(indexed_name("extract", "Module 1/Slides/deck.pptx.md"));
        assert!(indexed_name("corpus", "Week 3 — Data/2026-09-03 — Lecture.md"));
        assert!(indexed_name("note", "Central tendency.md"));
        assert!(indexed_name("guide", "Week 3 — Data Exploration.html"));
        assert!(indexed_name("guide", "Semester Master.html"));
        assert!(indexed_name("guide", "Practice/Week 3 — 2026-09-08.html"));
        assert!(indexed_name("guide", "Presentations/Reinhold.html"));
        assert!(indexed_name("guide", "Sessions/2026-09-03 — Lecture.md"));
        // Written with a twin: the markdown is the copy that is indexed.
        assert!(!indexed_name("guide", "Sessions/2026-09-03 — Lecture.html"));
        assert!(!indexed_name("guide", "Briefs/2026-09-14 — Homework 1.html"));
        assert!(!indexed_name("guide", "Workbook/Project Workbook.html"));
        // Not a document at all.
        assert!(!indexed_name("extract", "Module 1/Slides/deck.pptx.pdf"));
        assert!(!indexed_name("extract", "Module 1/paper.docx.html"));
        assert!(!indexed_name("corpus", "Week 3/2026-09-03 — Lecture.hints.json"));
        assert!(!indexed_name("note", "scratch.txt"));
    }

    /// The whole of it against a real tree: a phrase in one document ranks
    /// that document first with its stretch, a term the tree does not hold
    /// answers empty, an edit is picked up and a deletion drops the row.
    #[test]
    fn the_index_ranks_the_document_that_holds_the_phrase() {
        let conn = crate::db::memory_db();
        let dir = std::env::temp_dir().join(format!("classhub-fts-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let corpus = dir.join(CORPUS_DIR).join("Week 3");
        fs::create_dir_all(&corpus).expect("corpus dir");
        fs::create_dir_all(dir.join(NOTES_DIR)).expect("notes dir");
        fs::write(
            corpus.join("2026-09-03 — Lecture.md"),
            "## 00:12\n\nHe said the missing data mechanism matters more than the imputation.\n",
        )
        .expect("note");
        fs::write(
            dir.join(NOTES_DIR).join("Scratch.md"),
            "Imputation is mentioned here and nothing else is.\n",
        )
        .expect("scratch");

        let (written, dropped) = sync_class(&conn, 3, &dir).expect("sync");
        assert_eq!((written, dropped), (2, 0));
        // An unchanged tree writes nothing at all.
        assert_eq!(sync_class(&conn, 3, &dir).expect("resync"), (0, 0));

        let none = HashSet::new();
        let (hits, total) = run(&conn, &[3], "missing data mechanism", &none).expect("search");
        assert_eq!(total, 1, "only the corpus note holds the phrase");
        assert!(
            hits[0].rel_path.ends_with("2026-09-03 — Lecture.md"),
            "{}",
            hits[0].rel_path
        );
        assert_eq!(hits[0].kind, "corpus");
        assert_eq!(hits[0].line, Some(3));
        assert!(hits[0].snippet.contains("missing"), "{}", hits[0].snippet);

        // Both documents hold the word, and the search says so.
        let (_, both) = run(&conn, &[3], "imputation", &none).expect("search");
        assert_eq!(both, 2);

        // A duplicate's hit is dropped without touching the index.
        let mut drop = HashSet::new();
        drop.insert("Biostatistics for AI/Notes/Scratch.md".to_string());
        let (_, one) = run(&conn, &[3], "imputation", &drop).expect("search");
        assert_eq!(one, 1);

        // An edit is read again; a deletion drops the row and the hit.
        fs::write(
            dir.join(NOTES_DIR).join("Scratch.md"),
            "Rewritten with no mention of the word.\n",
        )
        .expect("rewrite");
        // The stat has one-second resolution, so move the length too.
        let (written, _) = sync_class(&conn, 3, &dir).expect("resync");
        assert_eq!(written, 1);
        let (_, after) = run(&conn, &[3], "imputation", &none).expect("search");
        assert_eq!(after, 1);

        fs::remove_file(corpus.join("2026-09-03 — Lecture.md")).expect("remove");
        let (_, gone) = sync_class(&conn, 3, &dir).expect("resync");
        assert_eq!(gone, 1);
        let (_, empty) = run(&conn, &[3], "missing data mechanism", &none).expect("search");
        assert_eq!(empty, 0);
        assert_eq!(indexed_count(&conn).expect("count"), 1);

        // A rebuild throws it away and finds the same one document.
        assert_eq!(rebuild_class(&conn, 3, &dir).expect("rebuild"), 1);
        assert_eq!(indexed_count(&conn).expect("count"), 1);

        let _ = fs::remove_dir_all(&dir);
    }

    /// A guide written as HTML alone is indexed as its prose, so a study
    /// guide stays findable where indexing twins only would have lost it.
    #[test]
    fn a_guide_without_a_twin_is_indexed_as_its_text() {
        let conn = crate::db::memory_db();
        let dir = std::env::temp_dir().join(format!("classhub-fts-html-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(GUIDES_DIR)).expect("guides dir");
        fs::write(
            dir.join(GUIDES_DIR).join("Week 3 — Data.html"),
            "<html><head><style>p{color:red}</style></head><body>\
             <h1>Week 3</h1><p>Winsorization trims the tail.</p></body></html>",
        )
        .expect("guide");
        assert_eq!(sync_class(&conn, 3, &dir).expect("sync").0, 1);

        let none = HashSet::new();
        let (hits, total) = run(&conn, &[3], "winsorization", &none).expect("search");
        assert_eq!(total, 1);
        assert_eq!(hits[0].kind, "guide");
        // The stylesheet is not part of the document's text.
        let (_, styled) = run(&conn, &[3], "color", &none).expect("search");
        assert_eq!(styled, 0);

        // A twin arriving means the markdown is what is indexed from then on.
        fs::write(
            dir.join(GUIDES_DIR).join("Week 3 — Data.md"),
            "# Week 3\n\nWinsorization trims the tail.\n",
        )
        .expect("twin");
        sync_class(&conn, 3, &dir).expect("resync");
        let (hits, total) = run(&conn, &[3], "winsorization", &none).expect("search");
        assert_eq!(total, 1, "one copy, not two");
        assert!(hits[0].rel_path.ends_with(".md"), "{}", hits[0].rel_path);

        let _ = fs::remove_dir_all(&dir);
    }
}
