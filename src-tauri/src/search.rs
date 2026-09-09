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

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::db::{CORPUS_DIR, EXTRACTS_DIR, GUIDES_DIR, NOTES_DIR};

/// How many ranked documents a search reports.
pub const MAX_HITS: usize = 20;
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
type Indexed = BTreeMap<String, (String, i64, i64, i64)>;

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
/// The reads happen before the transaction and the transaction is IMMEDIATE:
/// a deferred one that had read first is refused outright, past the busy
/// handler, when the other build commits in between — M37's cards index
/// learned that. A failure here is never fatal to the caller: the index is
/// derived, and a search over a stale index is worse than a fresh one but
/// better than no answer.
pub fn sync_class(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<(usize, usize)> {
    let known = indexed_rows(conn, class_id)?;
    let disk = walk_class(class_dir);

    let mut writes: Vec<(Found, String)> = Vec::new();
    let mut seen = HashSet::new();
    for found in disk {
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
    let gone: Vec<(String, i64)> = known
        .iter()
        .filter(|(path, _)| !seen.contains(*path))
        .map(|(path, (_, _, _, rowid))| (path.clone(), *rowid))
        .collect();

    if writes.is_empty() && gone.is_empty() {
        return Ok((0, 0));
    }

    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    for (path, rowid) in &gone {
        tx.execute("DELETE FROM material_fts WHERE rowid = ?1", [rowid])?;
        tx.execute(
            "DELETE FROM material_index WHERE class_id = ?1 AND rel_path = ?2",
            params![class_id, path],
        )?;
    }
    for (found, text) in &writes {
        if let Some((_, _, _, rowid)) = known.get(&found.rel_path) {
            tx.execute("DELETE FROM material_fts WHERE rowid = ?1", [rowid])?;
        }
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
    Ok((writes.len(), gone.len()))
}

/// Throws one class's index away and builds it again, for the Settings
/// rebuild — the way back from an index that a crash mid-write, or a tree
/// edited under a build that was not running, left disagreeing with the disk.
pub fn rebuild_class(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<usize> {
    {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM material_fts WHERE rowid IN
               (SELECT fts_rowid FROM material_index WHERE class_id = ?1)",
            [class_id],
        )?;
        tx.execute("DELETE FROM material_index WHERE class_id = ?1", [class_id])?;
        tx.commit()?;
    }
    let (written, _) = sync_class(conn, class_id, class_dir)?;
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
fn line_of(content: &str, terms: &[String]) -> Option<usize> {
    if terms.is_empty() {
        return None;
    }
    let lowered: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();
    content
        .lines()
        .enumerate()
        .find(|(_, line)| {
            let line = line.to_lowercase();
            lowered.iter().any(|t| line.contains(t.as_str()))
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
    let Some(expression) = fts_query(raw) else {
        return Ok((Vec::new(), 0));
    };
    let terms = all_terms(raw);
    let placeholders = class_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    // The class list is built from ids read out of the database, never from
    // the model's text, so it is inlined rather than bound — `IN (?)` cannot
    // take a list, and a bound array would need a temp table for four rows.
    let sql = format!(
        "SELECT class_id, rel_path, kind,
                snippet(material_fts, 3, '', '', '…', {SNIPPET_TOKENS}), content
         FROM material_fts
         WHERE material_fts MATCH ?1 AND class_id IN ({placeholders})
         ORDER BY bm25(material_fts) LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    // One over the reported count, so the header can say there are more.
    let rows = stmt.query_map(params![expression, (MAX_HITS as i64) + 30], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;

    let mut hits = Vec::new();
    let mut total = 0usize;
    for row in rows {
        let (class_id, rel_path, kind, snippet, content) = row?;
        let folder = folder_name(conn, class_id)?;
        let full = format!("{folder}/{rel_path}");
        if dropped.contains(&full) {
            continue;
        }
        total += 1;
        if hits.len() < MAX_HITS {
            hits.push(Hit {
                rel_path: full,
                kind,
                snippet: snippet.split_whitespace().collect::<Vec<_>>().join(" "),
                line: line_of(&content, &terms),
            });
        }
    }
    Ok((hits, total))
}

fn folder_name(conn: &Connection, class_id: i64) -> Result<String> {
    Ok(conn
        .query_row(
            "SELECT folder_name FROM classes WHERE id = ?1",
            [class_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
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

    /// The reported line is the first one carrying any term, so a citation
    /// opens where the match is rather than at the top of the file.
    #[test]
    fn the_line_is_the_first_one_a_term_is_on() {
        let doc = "# Title\n\nIntro paragraph.\nThe median is the middle value.\nMore.";
        assert_eq!(line_of(doc, &["median".into()]), Some(4));
        assert_eq!(line_of(doc, &["Median".into()]), Some(4));
        assert_eq!(line_of(doc, &["nowhere".into()]), None);
        assert_eq!(line_of(doc, &[]), None);
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
