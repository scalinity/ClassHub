//! SPEC §5/§7.2 — a course's own divisions, from whichever source knows them.
//!
//! The four courses do not share a shape: 14 weekly topics, 15 weekly topics,
//! 3 Parts across 16 weeks, and one that publishes no structure at all. Any
//! layout the app imposed would be wrong for at least two of them, so it reads
//! each course's own and stores the course's own words.
//!
//! Two sources declare them, in precedence order **canvas > syllabus**:
//!
//! - **canvas** — the course's published modules. Ground truth when it exists,
//!   which today it does not: none of the four courses uses Canvas Modules.
//! - **syllabus** — the weekly schedule inside the syllabus, read by the same
//!   `syllabus_scan` that already reads it for deadlines.
//!
//! `source` is stored rather than resolved away, because "Canvas says Module 1
//! runs from the 20th" and "a syllabus PDF seemed to say so" are different
//! claims, and the workspace names which one it is showing.
//!
//! A division's identity is the course's own label for it. A model does not
//! spell a name the same way twice — a rescan named a Part without its
//! `(Weeks 1-8)` suffix and forked a second row beside the first — so a row is
//! matched on its Canvas id where it has one, then on its exact name, then on
//! `(source, kind, number)`, the number being what the name opens with:
//! `Week 7 — …` is 7, `Part II:` is 2. The ordinal is not the identity: it is
//! the position in the list a reader reported, and a rescan that inserts a
//! `No class` row mid-list moves every ordinal after it. See `upsert`.
//!
//! A folder is not a third source. It is where material sits, not something the
//! course declared — listing the top-level folders as divisions put a second
//! numbering sequence under the course's own and labelled it a guess. What the
//! folder does know is recorded on the declared unit's own row: see
//! `attach_folder_paths`.
//!
//! Nothing here ever deletes. A unit that stops appearing in Canvas is kept
//! (SPEC §7.2): a mid-semester reshuffle must not silently orphan a guide.

use std::fs;
use std::path::PathBuf;

use anyhow::{bail, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

/// The kinds SPEC §5 allows, i.e. the words a course uses for its divisions.
pub const UNIT_KINDS: &[&str] = &["module", "week", "part"];

/// Model- and Canvas-supplied names enter storage here and are shown in the
/// workspace, so they are capped at something a heading can hold.
pub const MAX_UNIT_NAME: usize = 120;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnitInfo {
    pub id: i64,
    pub ordinal: i64,
    pub kind: String,
    pub name: String,
    /// The course's own number for it, read from the label its name opens
    /// with — 7 for `Week 7 — …`, 2 for `Part II:` — and, with the source and
    /// the kind, the identity a rescan matches on. None where the name opens
    /// with no such label (`Reading Days — No Class`).
    pub number: Option<i64>,
    pub rel_path: Option<String>,
    pub starts_on: Option<String>,
    pub ends_on: Option<String>,
    /// The weeks it spans where the course said so (SPEC §8.5): a Part's
    /// `(Weeks 1-8)` as data on the row rather than a suffix a rescan may drop.
    pub first_week: Option<i64>,
    pub last_week: Option<i64>,
    /// canvas | syllabus — which reader declared it.
    pub source: String,
}

/// One division on its way into the table, from either source.
pub struct NewUnit {
    pub ordinal: i64,
    pub kind: String,
    pub name: String,
    pub canvas_id: Option<String>,
    pub rel_path: Option<String>,
    pub starts_on: Option<String>,
    pub ends_on: Option<String>,
    /// The weeks the division spans, when the reader knows. `None` keeps what
    /// the row already holds — unlike a date, which a refresh from the same
    /// source clears — because a Part without its range files no lectures at
    /// all, and a rescan that happens not to state it must not cost that.
    pub weeks: Option<(i64, i64)>,
    pub source: &'static str,
}

/// canvas > syllabus. Higher wins; an equal source refreshes its own rows,
/// which is what makes a re-sync an update rather than a duplicate.
///
/// `list_units`' `ORDER BY` encodes the same order in SQL. Two hand-kept
/// orderings, because binding it once would mean building the clause with
/// `format!` — so a third source means changing both, and this is the note
/// saying so.
fn rank(source: &str) -> u8 {
    match source {
        "canvas" => 3,
        "syllabus" => 2,
        _ => 1,
    }
}

/// The columns every whole-row read here selects, in `read_unit`'s order.
const UNIT_COLUMNS: &str =
    "id, ordinal, kind, name, number, rel_path, starts_on, ends_on, first_week, last_week, source";

/// Every division for a class, the declared ones first.
///
/// Ordering by ordinal alone interleaves the sources, and the result reads as
/// nonsense: a Canvas "Module 1" lands between Week 1 and Week 2 because both
/// call themselves first. Sorting by source before ordinal keeps each source's
/// own sequence intact and puts what the course actually declared first. The
/// `CASE` repeats `rank()`'s order in SQL — see the note there.
pub fn list_units(conn: &Connection, class_id: i64) -> Result<Vec<UnitInfo>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {UNIT_COLUMNS} FROM units WHERE class_id = ?1
         ORDER BY CASE source WHEN 'canvas' THEN 0 WHEN 'syllabus' THEN 1 ELSE 2 END,
                  ordinal, id"
    ))?;
    let rows = stmt
        .query_map([class_id], read_unit)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One row selected as `UNIT_COLUMNS`.
fn read_unit(row: &rusqlite::Row<'_>) -> rusqlite::Result<UnitInfo> {
    Ok(UnitInfo {
        id: row.get(0)?,
        ordinal: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        number: row.get(4)?,
        rel_path: row.get(5)?,
        starts_on: row.get(6)?,
        ends_on: row.get(7)?,
        first_week: row.get(8)?,
        last_week: row.get(9)?,
        source: row.get(10)?,
    })
}

/// The course's word for this division, taken from how it named it.
///
/// Only the three SPEC §5 kinds exist, so an unrecognized name falls back to
/// `module` — the generic one. `kind` groups and labels; `name` is what the
/// reader actually sees, and it is never normalized.
pub fn kind_for_name(name: &str) -> &'static str {
    match split_label(name).0.as_str() {
        "week" | "wk" => "week",
        "part" => "part",
        _ => "module",
    }
}

/// The word a name opens with, lower-cased, and everything after it: `week`
/// and ` 7 — …` for `Week 7 — …`, `weekly` for `Weekly Readings`. The whole
/// first word, so "Weekly" is not "Week"; a digit run glued to the word
/// (`Week7`) is split off, so the spaced and unspaced spellings read alike.
/// The split is measured on the original text, because lower-casing can
/// change a character's byte length and an offset taken from the lower-cased
/// copy would land mid-character.
fn split_label(name: &str) -> (String, &str) {
    let trimmed = name.trim();
    let end = trimmed
        .find(|c: char| !c.is_alphanumeric())
        .unwrap_or(trimmed.len());
    let first = &trimmed[..end];
    let digits = first
        .find(|c: char| c.is_ascii_digit())
        .unwrap_or(first.len());
    (first[..digits].to_lowercase(), &trimmed[digits..])
}

/// The course's own number for a division, read from its label: `Week 7 —
/// Tree-Based Models` is 7, `Module 3` is 3, `Part II: Alignment` is 2, and
/// `Reading Days — No Class` is nothing. Only the words `kind_for_name` knows
/// count as labels, so a year in `Notes 2026` is not a number.
pub fn label_number(name: &str) -> Option<i64> {
    let (word, rest) = split_label(name);
    if !matches!(word.as_str(), "week" | "wk" | "module" | "part") {
        return None;
    }
    let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '#' || c == '.');
    match leading_number(rest) {
        // `Week 10/11` and `Weeks 1-8` both open with the first number.
        Some((n, _)) => (n >= 1).then_some(n),
        None => roman_number(rest),
    }
}

/// `II` → 2, `IX` → 9, read up to a non-letter; nothing for a run that is not
/// a numeral in its standard form. `Part Civil` scores as a number letter by
/// letter, and the round trip through `roman` is what tells a word from a
/// numeral: only a run that writes back as itself counts.
fn roman_number(s: &str) -> Option<i64> {
    let end = s.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(s.len());
    let run = s[..end].to_ascii_uppercase();
    if run.is_empty() || run.len() > 8 {
        return None;
    }
    let value = |c: char| match c {
        'I' => Some(1),
        'V' => Some(5),
        'X' => Some(10),
        'L' => Some(50),
        'C' => Some(100),
        _ => None,
    };
    let digits = run.chars().map(value).collect::<Option<Vec<i64>>>()?;
    let mut total = 0;
    for (i, &d) in digits.iter().enumerate() {
        if digits.get(i + 1).is_some_and(|&next| next > d) {
            total -= d;
        } else {
            total += d;
        }
    }
    (total >= 1 && roman(total) == run).then_some(total)
}

/// The standard numeral for `n`, for the round trip in `roman_number`.
fn roman(mut n: i64) -> String {
    const GLYPHS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (value, glyph) in GLYPHS {
        while n >= value {
            out.push_str(glyph);
            n -= value;
        }
    }
    out
}

/// What `upsert` did with a division.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Inserted,
    /// Refreshed in place — a rename, a moved date, a new ordinal.
    Updated,
    /// Already recorded exactly so, or outranked by what a higher source said.
    Unchanged,
}

/// The row a division landed on, what happened to it, and what the rename —
/// if it was one — leaves for the filesystem.
#[must_use = "a rename's effects move nothing until applied"]
#[derive(Debug)]
pub struct Upserted {
    pub id: i64,
    pub outcome: Outcome,
    pub effects: RenameEffects,
}

/// Everything a renamed division leaves for the filesystem, decided inside the
/// write's transaction and performed once it has committed — a refile's rule
/// (`lectures::RefileEffects`). Two things are named for a division on disk:
/// its corpus folder under `.classhub/corpus/` and its guide under
/// `Study Guides/`, and both follow the name, so that what a guide drew on can
/// still be read by the division's own name.
#[must_use = "nothing on disk moves until this is applied"]
#[derive(Debug, Default)]
pub struct RenameEffects {
    moves: Vec<(PathBuf, PathBuf)>,
}

impl RenameEffects {
    /// Folds another division's effects in, for a caller writing a whole list.
    pub fn extend(&mut self, other: RenameEffects) {
        self.moves.extend(other.moves);
    }

    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Applied after the commit, so a failure is logged rather than
    /// propagated: the rows already name the new paths, and a note or a guide
    /// left at the old one reads as undistilled or unreadable — which a
    /// redistill or a regeneration repairs — while the division itself is
    /// recorded as the course now names it.
    pub fn apply(self) {
        for (from, to) in self.moves {
            if let Some(parent) = to.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = fs::rename(&from, &to) {
                eprintln!("units: rename failed ({} → {}): {e}", from.display(), to.display());
            }
        }
    }
}

/// One row as held, read for a match.
struct Held {
    id: i64,
    source: String,
    ordinal: i64,
    kind: String,
    name: String,
    canvas_id: Option<String>,
    rel_path: Option<String>,
    starts_on: Option<String>,
    ends_on: Option<String>,
    number: Option<i64>,
    first_week: Option<i64>,
    last_week: Option<i64>,
}

const HELD_COLUMNS: &str = "id, source, ordinal, kind, name, canvas_id, rel_path, starts_on, ends_on, \
                            number, first_week, last_week";

fn read_held(row: &rusqlite::Row<'_>) -> rusqlite::Result<Held> {
    Ok(Held {
        id: row.get(0)?,
        source: row.get(1)?,
        ordinal: row.get(2)?,
        kind: row.get(3)?,
        name: row.get(4)?,
        canvas_id: row.get(5)?,
        rel_path: row.get(6)?,
        starts_on: row.get(7)?,
        ends_on: row.get(8)?,
        number: row.get(9)?,
        first_week: row.get(10)?,
        last_week: row.get(11)?,
    })
}

/// A division as it will be stored: the name capped, the kind settled, the
/// number read from the label.
struct Incoming {
    name: String,
    kind: String,
    number: Option<i64>,
}

fn incoming(unit: &NewUnit) -> Result<Incoming> {
    let name = crate::db::truncate(unit.name.trim(), MAX_UNIT_NAME);
    if name.is_empty() {
        bail!("a unit needs a name");
    }
    let kind = if UNIT_KINDS.contains(&unit.kind.as_str()) {
        unit.kind.clone()
    } else {
        kind_for_name(&name).to_string()
    };
    let number = label_number(&name);
    Ok(Incoming { name, kind, number })
}

/// The row a division matches, by the identity rule in the module doc: the
/// Canvas id, then the exact name, then `(source, kind, number)`.
///
/// The name comes before the number so that a row an earlier rescan forked —
/// two rows under one label, the second holding no number — is still found
/// as itself rather than renaming the first onto it.
///
/// `.optional()?` rather than `.ok()`: swallowing every error as "no such
/// row" turned a locked database into an INSERT that then failed on the
/// uniqueness constraint, reporting a cause that had nothing to do with it.
fn find_held(conn: &Connection, class_id: i64, unit: &NewUnit, incoming: &Incoming) -> Result<Option<Held>> {
    if let Some(canvas_id) = unit.canvas_id.as_deref() {
        let held = conn
            .query_row(
                &format!("SELECT {HELD_COLUMNS} FROM units WHERE class_id = ?1 AND canvas_id = ?2"),
                params![class_id, canvas_id],
                read_held,
            )
            .optional()?;
        if held.is_some() {
            return Ok(held);
        }
    }
    let held = conn
        .query_row(
            &format!("SELECT {HELD_COLUMNS} FROM units WHERE class_id = ?1 AND name = ?2"),
            params![class_id, incoming.name],
            read_held,
        )
        .optional()?;
    if held.is_some() {
        return Ok(held);
    }
    let Some(number) = incoming.number else {
        return Ok(None);
    };
    Ok(conn
        .query_row(
            &format!(
                "SELECT {HELD_COLUMNS} FROM units
                 WHERE class_id = ?1 AND source = ?2 AND kind = ?3 AND number = ?4"
            ),
            params![class_id, unit.source, incoming.kind, number],
            read_held,
        )
        .optional()?)
}

/// The row `upsert` would write this division to, if one exists — what lets a
/// scan claim each row once, before anything is written.
pub fn find(conn: &Connection, class_id: i64, unit: &NewUnit) -> Result<Option<i64>> {
    let incoming = incoming(unit)?;
    Ok(find_held(conn, class_id, unit, &incoming)?.map(|held| held.id))
}

/// The number a row may carry: its own, unless another row of the same class,
/// source and kind already holds it — the label index is unique — in which
/// case none, said on stderr. A clean table never gets here; a table holding
/// a fork from before labels existed does, and one row without a number beats
/// a launch or a rescan that fails.
fn free_number(
    conn: &Connection,
    class_id: i64,
    source: &str,
    kind: &str,
    number: Option<i64>,
    except: Option<i64>,
) -> Result<Option<i64>> {
    let Some(number) = number else {
        return Ok(None);
    };
    let holder: Option<i64> = conn
        .query_row(
            "SELECT id FROM units
             WHERE class_id = ?1 AND source = ?2 AND kind = ?3 AND number = ?4 AND id IS NOT ?5",
            params![class_id, source, kind, number, except],
            |row| row.get(0),
        )
        .optional()?;
    match holder {
        Some(id) => {
            eprintln!("units: {kind} {number} of class {class_id} is already row {id}; left unnumbered");
            Ok(None)
        }
        None => Ok(Some(number)),
    }
}

/// Inserts or refreshes one division, honouring source precedence.
///
/// Reports what it did, so a sync can say "3 new, 11 unchanged" rather than
/// claiming to have written what was already there, and hands back the moves
/// a rename leaves for the caller to apply once its own work is committed.
pub fn upsert(conn: &Connection, class_id: i64, unit: &NewUnit) -> Result<Upserted> {
    let incoming = incoming(unit)?;
    let held = find_held(conn, class_id, unit, &incoming)?;
    let Incoming { name, kind, number } = incoming;

    let Some(held) = held else {
        let number = free_number(conn, class_id, unit.source, &kind, number, None)?;
        conn.execute(
            "INSERT INTO units
             (class_id, ordinal, kind, name, number, canvas_id, rel_path, starts_on, ends_on,
              first_week, last_week, source)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                class_id,
                unit.ordinal,
                kind,
                name,
                number,
                unit.canvas_id,
                unit.rel_path,
                unit.starts_on,
                unit.ends_on,
                unit.weeks.map(|(first, _)| first),
                unit.weeks.map(|(_, last)| last),
                unit.source
            ],
        )?;
        return Ok(Upserted {
            id: conn.last_insert_rowid(),
            outcome: Outcome::Inserted,
            effects: RenameEffects::default(),
        });
    };

    // A syllabus "Module 1" must not overwrite what Canvas says Module 1 is —
    // including its dates.
    if rank(unit.source) < rank(&held.source) {
        return Ok(Upserted {
            id: held.id,
            outcome: Outcome::Unchanged,
            effects: RenameEffects::default(),
        });
    }
    // Found by name, and the name belongs to a different Canvas module. Two
    // divisions really can share a topic name — a term with four "Project
    // Presentations" weeks has four — and `UNIQUE(class_id, name)` holds only
    // one of them. Refused loudly rather than letting the second quietly
    // overwrite the first and counting it as "already recorded".
    if let (Some(incoming_id), Some(held_id)) = (unit.canvas_id.as_deref(), held.canvas_id.as_deref()) {
        if incoming_id != held_id {
            bail!("another division of this course is already called '{name}'");
        }
    }
    // Found by id or by label under a new name: the rename must not land on a
    // name another row holds.
    if held.name != name {
        let holder: Option<i64> = conn
            .query_row(
                "SELECT id FROM units WHERE class_id = ?1 AND name = ?2 AND id != ?3",
                params![class_id, name, held.id],
                |row| row.get(0),
            )
            .optional()?;
        if holder.is_some() {
            bail!("another division of this course is already called '{name}'");
        }
    }

    // A higher source knows the division's name, order and dates; it does not
    // know where the material sits on disk, which only the folder ever learns.
    // So a *takeover* fills fields in rather than replacing them — otherwise
    // Canvas superseding the syllabus would blank the path its guide reads.
    //
    // Within one source there is nothing to preserve: the incoming row simply
    // is the current state, and carrying an old value forward would make a
    // date the course removed impossible to clear.
    let takeover = rank(unit.source) > rank(&held.source);
    let keep = |incoming: &Option<String>, held: &Option<String>| match (takeover, incoming) {
        (true, None) => held.clone(),
        _ => incoming.clone(),
    };
    let merged_canvas_id = keep(&unit.canvas_id, &held.canvas_id);
    let merged_starts = keep(&unit.starts_on, &held.starts_on);
    let merged_ends = keep(&unit.ends_on, &held.ends_on);
    // `rel_path` is the exception in both directions: no source ever supplies
    // one, so an incoming None is silence rather than a correction.
    let merged_rel_path = unit.rel_path.clone().or_else(|| held.rel_path.clone());
    // The range too: see `NewUnit::weeks`.
    let (first_week, last_week) = match unit.weeks {
        Some((first, last)) => (Some(first), Some(last)),
        None => (held.first_week, held.last_week),
    };
    let number = free_number(conn, class_id, unit.source, &kind, number, Some(held.id))?;
    let unchanged = held.ordinal == unit.ordinal
        && held.kind == kind
        && held.source == unit.source
        && held.name == name
        && held.number == number
        && held.canvas_id == merged_canvas_id
        && held.rel_path == merged_rel_path
        && held.starts_on == merged_starts
        && held.ends_on == merged_ends
        && held.first_week == first_week
        && held.last_week == last_week;
    if unchanged {
        return Ok(Upserted {
            id: held.id,
            outcome: Outcome::Unchanged,
            effects: RenameEffects::default(),
        });
    }

    // One transaction for the row and everything a rename rewrites beside it.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let effects = if held.name != name {
        rename_effects(&tx, class_id, held.id, &held.name, &name)?
    } else {
        RenameEffects::default()
    };
    tx.execute(
        "UPDATE units
         SET ordinal = ?1, kind = ?2, name = ?3, number = ?4, canvas_id = ?5, rel_path = ?6,
             starts_on = ?7, ends_on = ?8, first_week = ?9, last_week = ?10, source = ?11
         WHERE id = ?12",
        params![
            unit.ordinal,
            kind,
            name,
            number,
            merged_canvas_id,
            merged_rel_path,
            merged_starts,
            merged_ends,
            first_week,
            last_week,
            unit.source,
            held.id
        ],
    )?;
    tx.commit()?;
    Ok(Upserted {
        id: held.id,
        outcome: Outcome::Updated,
        effects,
    })
}

/// What a rename carries with it: every contribution row's corpus path and the
/// corpus folder itself, and the guide row's file. Rows are rewritten here,
/// inside the caller's transaction; the moves are returned for after it.
///
/// A target already on disk refuses the rename rather than overwriting — the
/// refile's rule for a note — so a folder or a guide left behind by an earlier
/// fork is never silently replaced. The class folder is resolved only when
/// there is something to move, so a division with neither notes nor a guide
/// renames without touching the tree.
fn rename_effects(
    conn: &Connection,
    class_id: i64,
    unit_id: i64,
    old_name: &str,
    new_name: &str,
) -> Result<RenameEffects> {
    let mut effects = RenameEffects::default();
    let mut stmt = conn.prepare(
        "SELECT id, rel_path FROM lecture_contributions WHERE class_id = ?1 AND unit_id = ?2",
    )?;
    let contributions = stmt
        .query_map(params![class_id, unit_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let guide: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, rel_path FROM guides WHERE class_id = ?1 AND scope = ?2",
            params![class_id, crate::db::unit_scope(unit_id)],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if contributions.is_empty() && guide.is_none() {
        return Ok(effects);
    }
    let class_dir = crate::scanner::class_dir(conn, class_id)?;

    let old_folder = crate::lectures::corpus_folder(old_name);
    let new_folder = crate::lectures::corpus_folder(new_name);
    if old_folder != new_folder {
        for (id, transcript_rel) in &contributions {
            conn.execute(
                "UPDATE lecture_contributions SET corpus_rel_path = ?1 WHERE id = ?2",
                params![crate::lectures::corpus_rel_path(new_name, transcript_rel), id],
            )?;
        }
        let from = class_dir.join(&old_folder);
        if from.is_dir() {
            let to = class_dir.join(&new_folder);
            if to.exists() {
                bail!("a corpus folder already exists at {new_folder} — renaming would overwrite it");
            }
            effects.moves.push((from, to));
        }
    }

    if let Some((guide_id, rel_path)) = guide {
        let new_rel = crate::guides::unit_guide_rel_path(new_name);
        if rel_path != new_rel {
            let from = class_dir.join(&rel_path);
            if from.is_file() {
                let to = class_dir.join(&new_rel);
                if to.exists() {
                    bail!("a guide already exists at {new_rel} — renaming would overwrite it");
                }
                effects.moves.push((from, to));
            }
            conn.execute(
                "UPDATE guides SET rel_path = ?1 WHERE id = ?2",
                params![new_rel, guide_id],
            )?;
        }
    }
    Ok(effects)
}

/// Fills `number`, `first_week` and `last_week` for every row from its name —
/// run once by migration 0012, inside its transaction, for the rows that
/// predate the columns. A number another row of the same class, source and
/// kind already carries is left empty rather than failing the launch.
pub fn backfill_labels(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id, class_id, source, kind, name FROM units ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, class_id, source, kind, name) in rows {
        let number = free_number(conn, class_id, &source, &kind, label_number(&name), Some(id))?;
        let weeks = declared_weeks(&kind, &name, None);
        conn.execute(
            "UPDATE units SET number = ?1, first_week = ?2, last_week = ?3 WHERE id = ?4",
            params![number, weeks.map(|(f, _)| f), weeks.map(|(_, l)| l), id],
        )?;
    }
    Ok(())
}

/// The weeks a division spans, as a reader states them or as its name does:
/// a Part or Module named `… (Weeks 1-8)` carries its range in the name, and
/// a week names only itself, which its number already says.
pub fn declared_weeks(kind: &str, name: &str, stated: Option<(i64, i64)>) -> Option<(i64, i64)> {
    match stated {
        Some((first, last)) if (1..=MAX_WEEK).contains(&first) && last >= first && last <= MAX_WEEK => {
            Some((first, last))
        }
        _ if kind != "week" => parse_week_range(name),
        _ => None,
    }
}

/// Points each declared division at the folder that holds its material.
///
/// Runs on every scan, which is what keeps it current without a second place to
/// press. A folder is not itself one of a course's divisions: recording it as
/// one added a name, an ordinal and a kind restating `files.rel_path`, which
/// the Materials tree renders better, and it cost the Structure list a second
/// numbering sequence with a label explaining that the app was showing
/// something the course never said.
///
/// The join is what the folder was ever needed for, and it never depended on
/// that row. A Canvas or syllabus unit knows its name and dates but not where
/// its material lives; this is what tells it, so §8.1's guide has something to
/// read.
pub fn attach_folder_paths(conn: &Connection, class_id: i64, folders: &[String]) -> Result<()> {
    for name in folders {
        // One odd folder name must not cost the rest of the tree.
        if let Err(e) = attach_rel_path(conn, class_id, name, name) {
            eprintln!("units: could not attach '{name}' for class {class_id}: {e:#}");
        }
    }
    Ok(())
}

/// Attaches a folder to a unit that was declared without one.
///
/// A Canvas or syllabus unit knows its name and dates but not where its
/// material lives; the folder knows only the path. When both describe the
/// same division this is what joins them, so §8.1's guide can find the
/// unit's files.
pub fn attach_rel_path(conn: &Connection, class_id: i64, name: &str, rel_path: &str) -> Result<()> {
    conn.execute(
        "UPDATE units SET rel_path = ?1
         WHERE class_id = ?2 AND name = ?3 AND rel_path IS NULL",
        params![rel_path, class_id, name],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Weeks (SPEC §8.5) — the calendar side of a division

/// The widest a folder segment built from a course's own words gets. Nothing
/// technical: a path that stays readable in Finder and in a prompt listing.
const MAX_FOLDER_SEGMENT: usize = 90;

/// Past this, the digits are a year or a room number rather than a week.
const MAX_WEEK: i64 = 60;

/// One week a lecture can be filed into, and the division it feeds.
///
/// Built from `units`, never from arithmetic (SPEC §1): Fundamentals runs Week
/// 13 on Nov 17 and Week 14 on Dec 1, so `(date − start) / 7` names the wrong
/// week for the rest of the term.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct WeekSlot {
    pub week: i64,
    /// The folder a transcript for this week is filed in (SPEC §4).
    pub folder: String,
    /// The division this week's lecture contributes to: the week's own unit
    /// where the course numbers its weeks, or the Part whose range contains it.
    pub unit_id: i64,
    pub unit_name: String,
    /// The date the course itself published for this week, where it published
    /// one — what the Add lecture form's default is measured against.
    pub meets_on: Option<String>,
}

/// Every week this course can file a lecture into.
///
/// A course that numbers its weeks supplies them directly: a week row's week is
/// its own number — the course's, not the list position, which a `No class`
/// row inserted mid-list would shift — and a row named without one (`Reading
/// Days — No Class`) takes its ordinal, unless a numbered row already holds
/// that week, since the course's own numbering wins. Applied Generative AI
/// numbers none — it declares three Parts spanning week ranges — so its weeks
/// are read out of those ranges, and each maps to the Part that contains it. A
/// course that declares neither gets no weeks, and a lecture for it routes
/// through `_Inbox/` for the sorter to place (SPEC §7.1).
pub fn week_slots(conn: &Connection, class_id: i64) -> Result<Vec<WeekSlot>> {
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, number, name, starts_on FROM units
         WHERE class_id = ?1 AND kind = 'week'
         ORDER BY number IS NULL, ordinal, id",
    )?;
    let weeks = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut slots: Vec<WeekSlot> = Vec::new();
    for (unit_id, ordinal, number, unit_name, meets_on) in weeks {
        let week = number.unwrap_or(ordinal);
        if slots.iter().any(|s| s.week == week) {
            continue;
        }
        slots.push(WeekSlot {
            week,
            folder: week_folder(week, Some(&unit_name)),
            unit_id,
            unit_name,
            meets_on,
        });
    }
    if !slots.is_empty() {
        slots.sort_by_key(|s| s.week);
        return Ok(slots);
    }

    let mut stmt = conn.prepare(
        "SELECT id, name, first_week, last_week FROM units
         WHERE class_id = ?1 AND first_week IS NOT NULL AND last_week IS NOT NULL
         ORDER BY ordinal, id",
    )?;
    let ranges = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (unit_id, unit_name, first, last) in ranges {
        for week in first..=last {
            // Ranges should not overlap, but if a syllabus says they do, the
            // earlier Part keeps the week rather than the later one silently
            // taking it.
            if slots.iter().any(|s| s.week == week) {
                continue;
            }
            slots.push(WeekSlot {
                week,
                folder: week_folder(week, None),
                unit_id,
                unit_name: unit_name.clone(),
                meets_on: None,
            });
        }
    }
    slots.sort_by_key(|s| s.week);
    Ok(slots)
}

/// The division a lecture filed into `week` contributes to, if the course
/// declares one for it.
pub fn slot_for_week(conn: &Connection, class_id: i64, week: i64) -> Result<Option<WeekSlot>> {
    Ok(week_slots(conn, class_id)?
        .into_iter()
        .find(|slot| slot.week == week))
}

/// The week whose published meeting date sits closest to `date` — the default
/// the Add lecture form offers, and correctable there.
///
/// `None` where the course published no dates, in which case the week is asked
/// for outright rather than guessed at.
pub fn nearest_week(slots: &[WeekSlot], date: &str) -> Option<i64> {
    let day = |iso: &str| NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok();
    let target = day(date)?;
    slots
        .iter()
        .filter_map(|slot| {
            let meets = day(slot.meets_on.as_deref()?)?;
            Some((slot.week, (meets - target).num_days().abs()))
        })
        // Ties go to the earlier week: a lecture equidistant between two
        // meetings is the later of the two sessions, not the earlier one's
        // sequel.
        .min_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
        .map(|(week, _)| week)
}

/// The division a course is in on `today` (SPEC §8.5): the dated division
/// whose published start is the latest on or before it — a division runs
/// from its start to the next one's, so that is the one containing today —
/// whatever the course calls it. A syllabus row the scan stored as `module`
/// because its name opens with neither "Week" nor "Part" (`Reading Days — No
/// Class`) is no less a division than one it called a week, so the read is
/// over every dated row rather than over `week_slots`.
///
/// `None` before the first published date, `None` for a string that is not a
/// date, and `None` throughout for a course that published none: Applied
/// Generative AI's Parts name week ranges and no days, and the alternative is
/// a week number from arithmetic, which SPEC §1 forbids. Past the last
/// published date the last division stays current, because nothing the course
/// published says it has ended. Two divisions starting on one day: a week wins
/// over a coarser division it sits inside, then the later ordinal, then the
/// later row — a rule so the answer is one row, not a claim about the syllabus.
pub fn current_unit(conn: &Connection, class_id: i64, today: &str) -> Result<Option<UnitInfo>> {
    let day = |iso: &str| NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok();
    let Some(target) = day(today) else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {UNIT_COLUMNS} FROM units WHERE class_id = ?1 AND starts_on IS NOT NULL"
    ))?;
    let dated = stmt
        .query_map([class_id], read_unit)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(dated
        .into_iter()
        .filter_map(|unit| {
            let starts = day(unit.starts_on.as_deref()?)?;
            (starts <= target).then_some((starts, unit))
        })
        .max_by_key(|(starts, unit)| (*starts, unit.kind == "week", unit.ordinal, unit.id))
        .map(|(_, unit)| unit))
}

/// `Part I: … (Weeks 1-8)` → `Some((1, 8))`.
///
/// How a range written into a name is read — into `first_week`/`last_week`
/// when a Part is recorded, and back out of a `Weeks/Week 05 — …` folder name
/// when a filed transcript is mapped. A name with no range is not an error —
/// most unit names have none.
pub fn parse_week_range(name: &str) -> Option<(i64, i64)> {
    let lower = name.to_lowercase();
    let mut cursor = 0usize;
    while let Some(at) = lower[cursor..].find("week") {
        cursor += at + "week".len();
        let rest = lower[cursor..].trim_start_matches('s').trim_start();
        let Some((first, after)) = leading_number(rest) else {
            continue;
        };
        if first < 1 || first > MAX_WEEK {
            continue;
        }
        let after = after.trim_start();
        // A bare "Week 5" stands for itself; a hyphen, en/em dash or "to"
        // opens a range.
        let tail = after
            .strip_prefix(['-', '\u{2013}', '\u{2014}'])
            .or_else(|| after.strip_prefix("to "))
            .or_else(|| after.strip_prefix("through "));
        let Some(tail) = tail else {
            return Some((first, first));
        };
        let last = leading_number(tail.trim_start())
            .map(|(n, _)| n)
            .filter(|n| (1..=MAX_WEEK).contains(n))
            .unwrap_or(first);
        return Some((first.min(last), first.max(last)));
    }
    None
}

/// The number a string opens with, and what follows it.
fn leading_number(s: &str) -> Option<(i64, &str)> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    // Long enough to be an id rather than a week; parse would also overflow.
    if end == 0 || end > 4 {
        return None;
    }
    s[..end].parse().ok().map(|n| (n, &s[end..]))
}

/// `Week 03 — Data Exploration`, the folder one week's material lives in.
///
/// The topic comes from the week unit's own name with that name's own "Week N"
/// prefix removed, so the folder does not say it twice; a course that supplies
/// no name for the week gets a bare `Week 03`.
pub fn week_folder(week: i64, unit_name: Option<&str>) -> String {
    let stem = format!("Week {week:02}");
    let topic = unit_name.map(week_topic).unwrap_or_default();
    let topic = folder_segment(&topic);
    if topic.is_empty() {
        stem
    } else {
        format!("{stem} — {topic}")
    }
}

/// The week a filed transcript's path names — `Weeks/Week 05 — …/…md` → 5.
///
/// The sorter files a transcript by approving a move, which never passes
/// through the Add lecture form, so the path is the only place the decision was
/// recorded.
pub fn week_from_rel_path(rel_path: &str) -> Option<i64> {
    let rest = rel_path.strip_prefix(crate::db::WEEKS_DIR)?.strip_prefix('/')?;
    let folder = rest.split('/').next()?;
    parse_week_range(folder).map(|(first, _)| first)
}

/// What a unit's name says beyond its own week number.
fn week_topic(name: &str) -> String {
    let trimmed = name.trim();
    // `get` returns None on a non-boundary, so a name opening with a multi-byte
    // character cannot slice mid-char here.
    let after_word = if trimmed.get(..4).is_some_and(|s| s.eq_ignore_ascii_case("week")) {
        4
    } else if trimmed.get(..2).is_some_and(|s| s.eq_ignore_ascii_case("wk")) {
        2
    } else {
        return trimmed.to_string();
    };
    let rest = trimmed[after_word..].trim_start();
    let Some((_, rest)) = leading_number(rest) else {
        return trimmed.to_string();
    };
    rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, '\u{2014}' | '\u{2013}' | '-' | ':' | '.' | '|')
    })
    .trim()
    .to_string()
}

/// A folder name built from a course's own words. Slashes and colons would
/// repoint a write, so they flatten to dashes rather than rejecting a name the
/// course actually uses — the policy `lectures::transcript_file_name` applies
/// to lecture titles.
pub fn folder_segment(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c == '/' || c == ':' { '-' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let mut out: String = cleaned.chars().take(MAX_FOLDER_SEGMENT).collect();
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real schema, so these tests run on what the app runs on.
    fn db() -> Connection {
        crate::db::memory_db()
    }

    fn unit(ordinal: i64, kind: &str, name: &str, starts_on: Option<&str>) -> NewUnit {
        NewUnit {
            ordinal,
            kind: kind.into(),
            name: name.into(),
            canvas_id: None,
            rel_path: None,
            starts_on: starts_on.map(Into::into),
            ends_on: None,
            weeks: None,
            source: "syllabus",
        }
    }

    fn canvas_unit(ordinal: i64, name: &str, canvas_id: &str) -> NewUnit {
        NewUnit {
            canvas_id: Some(canvas_id.into()),
            source: "canvas",
            ..unit(ordinal, "module", name, None)
        }
    }

    /// `upsert` for a test that wants the outcome and nothing on disk.
    fn write(conn: &Connection, class_id: i64, unit: &NewUnit) -> Outcome {
        let written = upsert(conn, class_id, unit).expect("upsert");
        assert!(written.effects.is_empty(), "nothing here should move on disk");
        written.outcome
    }

    #[test]
    fn reads_the_course_s_own_word_for_its_divisions() {
        assert_eq!(kind_for_name("Week 7 — Tree-Based Models"), "week");
        assert_eq!(kind_for_name("week 1"), "week");
        assert_eq!(kind_for_name("Module 3"), "module");
        assert_eq!(kind_for_name("Part II"), "part");
        // Not a recognized word: `name` still carries the truth, and `kind`
        // takes the generic one rather than inventing a fourth.
        assert_eq!(kind_for_name("Foundations"), "module");
        assert_eq!(kind_for_name(""), "module");
        // "Weekly" is not "Week" — the boundary is the whole first word.
        assert_eq!(kind_for_name("Weekly Readings"), "module");
    }

    /// The identity a rescan matches on: the number the course's own label
    /// carries, in Arabic or Roman figures, and nothing for a row named
    /// without one.
    #[test]
    fn reads_the_number_out_of_a_division_s_label() {
        assert_eq!(label_number("Week 7 — Classification Methods I: Tree-Based Models"), Some(7));
        assert_eq!(label_number("week 14"), Some(14));
        assert_eq!(label_number("Wk 2: Study Designs"), Some(2));
        assert_eq!(label_number("Module 3 — Regression"), Some(3));
        assert_eq!(label_number("Part I: Deep Learning to Large Language Models (Weeks 1-8)"), Some(1));
        assert_eq!(label_number("Part II: Reinforcement Learning and Alignment"), Some(2));
        assert_eq!(label_number("Part IV"), Some(4));
        assert_eq!(label_number("Part ix"), Some(9));
        assert_eq!(label_number("Part XIV: Deployment"), Some(14));
        assert_eq!(label_number("Part 3"), Some(3));
        // A word spelled in numeral letters is not a numeral: only the
        // standard form counts, so `Civil` (153 letter by letter) and `IIII`
        // are words.
        assert_eq!(label_number("Part Civil Engineering"), None);
        assert_eq!(label_number("Part IIII"), None);
        assert_eq!(label_number("Part VX"), None);
        assert_eq!(label_number("Module #2"), Some(2));
        // A number glued to the word is the same label as a spaced one, so
        // the two spellings cannot fork a row.
        assert_eq!(label_number("Week7 — Topic"), Some(7));
        assert_eq!(kind_for_name("Week7 — Topic"), "week");
        assert_eq!(label_number("Wk2"), Some(2));
        // Lower-casing can change a character's byte length (the Kelvin sign
        // lowers to a plain k); the split must not slice by the lower-cased
        // length.
        assert_eq!(label_number("WEE\u{212A} 7 — Topic"), Some(7));
        assert_eq!(kind_for_name("WEE\u{212A} 7"), "week");
        assert_eq!(label_number("\u{212A}"), None);
        // The range in a Part's name is not its label.
        assert_eq!(label_number("Weeks 1-8"), None);
        // Named without a label: the identity falls back to the name.
        assert_eq!(label_number("Reading Days — No Class"), None);
        assert_eq!(label_number("No class (Nov. 24) — Thanksgiving Break"), None);
        assert_eq!(label_number("Finals week — Capstone Presentations"), None);
        assert_eq!(label_number("Part Introduction"), None);
        assert_eq!(label_number("Weekly Readings"), None);
        assert_eq!(label_number("Week 0"), None);
        assert_eq!(label_number("Week of 2026"), None);
        assert_eq!(label_number(""), None);
    }

    /// The precedence rule, which is the whole reason `source` is stored.
    #[test]
    fn a_syllabus_week_never_overwrites_what_canvas_declared() {
        let conn = db();
        let canvas = NewUnit {
            ordinal: 3,
            starts_on: Some("2026-08-20".into()),
            ..canvas_unit(3, "Module 1", "55")
        };
        assert_eq!(write(&conn, 1, &canvas), Outcome::Inserted);

        let syllabus = unit(1, "module", "Module 1", Some("2026-09-01"));
        assert_eq!(write(&conn, 1, &syllabus), Outcome::Unchanged, "the syllabus won");

        let units = list_units(&conn, 1).expect("list");
        assert_eq!(units.len(), 1);
        assert_eq!(units[0].source, "canvas");
        assert_eq!(units[0].ordinal, 3, "the syllabus reordered a Canvas unit");
        assert_eq!(units[0].starts_on.as_deref(), Some("2026-08-20"));
    }

    /// A folder is not a division. What it knows is where the material sits,
    /// which the declaring source never learns — so it annotates rather than
    /// adding a row of its own.
    #[test]
    fn a_folder_gives_a_declared_division_its_path() {
        let conn = db();
        assert_eq!(write(&conn, 1, &unit(1, "module", "Module 1", None)), Outcome::Inserted);
        attach_folder_paths(&conn, 1, &["Module 1".into(), "Slides".into()]).expect("attach");

        let units = list_units(&conn, 1).expect("list");
        // "Slides" is a folder and nothing more — listing it as a division
        // would put a second numbering sequence under the course's own.
        assert_eq!(units.len(), 1, "a folder was recorded as a division");
        assert_eq!(units[0].rel_path.as_deref(), Some("Module 1"));
    }

    /// A takeover fills in what the lower source knew; a refresh from the same
    /// source does not, or a date the course removed could never be cleared.
    #[test]
    fn a_source_can_clear_a_date_it_set_itself() {
        let conn = db();
        let with_date = NewUnit {
            starts_on: Some("2026-08-20".into()),
            ..canvas_unit(1, "Week 1", "9")
        };
        assert_eq!(write(&conn, 1, &with_date), Outcome::Inserted);

        let cleared = NewUnit {
            starts_on: None,
            ..with_date
        };
        assert_eq!(write(&conn, 1, &cleared), Outcome::Updated, "reported no change");
        assert_eq!(list_units(&conn, 1).expect("list")[0].starts_on, None);
    }

    /// Renaming a published module is the same division under a new name.
    /// Matched on name alone it would become a second one, and the old name
    /// would go on being presented as something the course declares.
    #[test]
    fn a_renamed_canvas_module_keeps_its_row() {
        let conn = db();
        let before = NewUnit {
            rel_path: Some("Module 1".into()),
            ..canvas_unit(1, "Module 1", "55")
        };
        assert_eq!(write(&conn, 1, &before), Outcome::Inserted);
        let after = NewUnit {
            name: "Module 1 — Foundations".into(),
            ..before
        };
        assert_eq!(write(&conn, 1, &after), Outcome::Updated);

        let units = list_units(&conn, 1).expect("list");
        assert_eq!(units.len(), 1, "the rename forked the division");
        assert_eq!(units[0].name, "Module 1 — Foundations");
        assert_eq!(units[0].rel_path.as_deref(), Some("Module 1"));
    }

    /// The M22 fork: a rescan of Applied Generative AI's syllabus named the
    /// Parts without their `(Weeks 1-8)` suffixes, and three rows landed
    /// beside the three that carried the ranges. Matched on the label, the
    /// same row takes the new name, keeps its range as data, and the
    /// week-to-Part join still resolves every week.
    #[test]
    fn a_rescan_that_drops_a_part_s_suffix_updates_the_row_in_place() {
        let conn = db();
        let parts = [
            "Part I: Deep Learning to Large Language Models (Weeks 1-8)",
            "Part II: Reinforcement Learning and Alignment (Weeks 9-12)",
            "Part III: Agentic AI in Medicine (Weeks 13-16)",
        ];
        for (i, name) in parts.iter().enumerate() {
            let part = NewUnit {
                weeks: declared_weeks("part", name, None),
                ..unit(i as i64 + 1, "part", name, None)
            };
            assert_eq!(write(&conn, 4, &part), Outcome::Inserted);
        }
        let ids: Vec<i64> = list_units(&conn, 4).expect("list").iter().map(|u| u.id).collect();

        for (i, name) in [
            "Part I: Deep Learning to Large Language Models",
            "Part II: Reinforcement Learning and Alignment",
            "Part III: Agentic AI in Medicine",
        ]
        .iter()
        .enumerate()
        {
            // The rescan states no range: the one held stands.
            assert_eq!(write(&conn, 4, &unit(i as i64 + 1, "part", name, None)), Outcome::Updated);
        }
        let after = list_units(&conn, 4).expect("list");
        assert_eq!(after.len(), 3, "the rescan forked the Parts");
        assert_eq!(after.iter().map(|u| u.id).collect::<Vec<_>>(), ids);
        assert_eq!(after[0].name, "Part I: Deep Learning to Large Language Models");
        assert_eq!(after[0].number, Some(1));
        assert_eq!((after[0].first_week, after[0].last_week), (Some(1), Some(8)));
        assert_eq!((after[2].first_week, after[2].last_week), (Some(13), Some(16)));

        let slots = week_slots(&conn, 4).expect("slots");
        assert_eq!(slots.len(), 16, "the ranges no longer cover weeks 1–16");
        assert_eq!(slot_for_week(&conn, 4, 9).unwrap().unwrap().unit_id, ids[1]);

        // A rescan stating a new range replaces the held one.
        let widened = NewUnit {
            weeks: Some((13, 17)),
            ..unit(3, "part", "Part III: Agentic AI in Medicine", None)
        };
        assert_eq!(write(&conn, 4, &widened), Outcome::Updated);
        assert_eq!(week_slots(&conn, 4).expect("slots").len(), 17);
    }

    /// The identity is the label, not the ordinal: a rescan that inserts an
    /// unnumbered row mid-list moves every ordinal after it, and Week 14 must
    /// still be week 14 — which is what the Add lecture form files by.
    #[test]
    fn an_inserted_row_moves_the_ordinals_and_not_the_weeks() {
        let conn = db();
        for (ordinal, name, on) in [
            (13, "Week 13 — Model Lifecycle, MLOps, and Reproducibility", "2026-11-17"),
            (14, "Week 14 — Introduction to Deep Learning and Course Synthesis", "2026-12-01"),
        ] {
            assert_eq!(write(&conn, 1, &unit(ordinal, "week", name, Some(on))), Outcome::Inserted);
        }
        let week14 = list_units(&conn, 1).expect("list")[1].id;

        // The rescan: Thanksgiving at 14, Week 14 pushed to 15, finals at 16.
        for (ordinal, name, on, expected) in [
            (13, "Week 13 — Model Lifecycle, MLOps, and Reproducibility", "2026-11-17", Outcome::Unchanged),
            (14, "No class (Nov. 24) — Thanksgiving Break", "2026-11-24", Outcome::Inserted),
            (15, "Week 14 — Introduction to Deep Learning and Course Synthesis", "2026-12-01", Outcome::Updated),
            (16, "Finals week — Capstone Presentations", "2026-12-05", Outcome::Inserted),
        ] {
            assert_eq!(write(&conn, 1, &unit(ordinal, "week", name, Some(on))), expected, "{name}");
        }
        let units = list_units(&conn, 1).expect("list");
        assert_eq!(units.len(), 4);
        assert_eq!(units[2].id, week14, "Week 14 was forked rather than moved");
        assert_eq!((units[2].ordinal, units[2].number), (15, Some(14)));
        assert_eq!(units[1].number, None);

        let slots = week_slots(&conn, 1).expect("slots");
        let by_week = |week: i64| slots.iter().find(|s| s.week == week).map(|s| s.unit_name.as_str());
        assert_eq!(by_week(13), Some("Week 13 — Model Lifecycle, MLOps, and Reproducibility"));
        // The course's Week 14 is week 14; the Thanksgiving row, which took
        // ordinal 14, gets no slot — no class meets.
        assert_eq!(by_week(14), Some("Week 14 — Introduction to Deep Learning and Course Synthesis"));
        assert_eq!(by_week(15), None);
        // A row named without a label still takes its ordinal where no
        // numbered row holds it, so a session in finals week has a home.
        assert_eq!(by_week(16), Some("Finals week — Capstone Presentations"));
        assert_eq!(nearest_week(&slots, "2026-12-01"), Some(14));
        assert_eq!(
            slots.iter().find(|s| s.week == 14).unwrap().folder,
            "Week 14 — Introduction to Deep Learning and Course Synthesis"
        );
    }

    /// A renamed week keeps its row, and everything named for it on disk
    /// follows: the corpus folder, with every contribution's stored path, and
    /// the guide file, with the row's. The guide's scope names the id, so it
    /// needs nothing.
    #[test]
    fn a_renamed_week_carries_its_corpus_folder_and_its_guide() {
        let conn = db();
        let root = std::env::temp_dir().join(format!("classhub-unit-rename-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let class_dir = root.join("Fundamentals of Artificial Intelligence in Medicine I");
        crate::db::set_setting(&conn, "aibhs_root", &root.to_string_lossy()).expect("root");

        let old_name = "Week 2 — Responsible AI, Ethics, and Governance";
        let new_name = "Week 2 — Responsible AI and Governance";
        let written = upsert(&conn, 1, &unit(2, "week", old_name, Some("2026-09-01"))).expect("insert");
        let unit_id = written.id;
        let transcript = "Weeks/Week 02 — Responsible AI, Ethics, and Governance/2026-09-01 — Lecture.md";
        let old_note = crate::lectures::corpus_rel_path(old_name, transcript);
        conn.execute(
            "INSERT INTO lecture_contributions
             (class_id, unit_id, rel_path, start_ms, end_ms, start_line, end_line,
              corpus_rel_path, summary, confidence, status, created_at)
             VALUES (1, ?1, ?2, 0, 1, 1, 1, ?3, 'Whole session', 'high', 'applied', 1)",
            params![unit_id, transcript, old_note],
        )
        .expect("contribution");
        let old_guide = crate::guides::unit_guide_rel_path(old_name);
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (1, ?1, ?2, 1, '[]')",
            params![crate::db::unit_scope(unit_id), old_guide],
        )
        .expect("guide");
        for rel in [&old_note, &old_guide] {
            let path = class_dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).expect("dir");
            fs::write(&path, "content").expect("file");
        }

        let written = upsert(&conn, 1, &unit(2, "week", new_name, Some("2026-09-01"))).expect("rename");
        assert_eq!((written.id, written.outcome), (unit_id, Outcome::Updated));
        assert!(!written.effects.is_empty(), "nothing was moved");
        written.effects.apply();

        let units = list_units(&conn, 1).expect("list");
        assert_eq!(units.len(), 1, "the rename forked the week");
        assert_eq!(units[0].name, new_name);
        let new_note = crate::lectures::corpus_rel_path(new_name, transcript);
        let stored: String = conn
            .query_row("SELECT corpus_rel_path FROM lecture_contributions WHERE unit_id = ?1", [unit_id], |r| r.get(0))
            .expect("row");
        assert_eq!(stored, new_note);
        assert!(class_dir.join(&new_note).is_file(), "the note did not move");
        assert!(!class_dir.join(&old_note).exists());
        assert!(!class_dir.join(crate::lectures::corpus_folder(old_name)).exists(), "the old folder lingers");
        let (scope, rel): (String, String) = conn
            .query_row("SELECT scope, rel_path FROM guides WHERE class_id = 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .expect("guide");
        assert_eq!(scope, crate::db::unit_scope(unit_id));
        assert_eq!(rel, crate::guides::unit_guide_rel_path(new_name));
        assert!(class_dir.join(&rel).is_file(), "the guide did not move");
        assert!(!class_dir.join(&old_guide).exists());
        // The slot follows the row, so the lecture still maps to the same id.
        assert_eq!(slot_for_week(&conn, 1, 2).unwrap().unwrap().unit_id, unit_id);

        // A target already on disk refuses the rename rather than overwriting.
        let taken = "Week 2 — Ethics";
        fs::create_dir_all(class_dir.join(crate::lectures::corpus_folder(taken))).expect("stray");
        let err = upsert(&conn, 1, &unit(2, "week", taken, Some("2026-09-01"))).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err:#}");
        assert_eq!(list_units(&conn, 1).expect("list")[0].name, new_name, "a refused rename wrote");
    }

    /// A rename must not land on a name another row holds: a Canvas module
    /// found by its id and renamed onto a syllabus row's name is refused
    /// rather than leaving two rows under one name or overwriting one.
    #[test]
    fn a_rename_onto_a_held_name_is_refused() {
        let conn = db();
        assert_eq!(write(&conn, 1, &canvas_unit(1, "Module 1", "55")), Outcome::Inserted);
        assert_eq!(write(&conn, 1, &unit(2, "module", "Module 2", None)), Outcome::Inserted);
        let err = upsert(&conn, 1, &canvas_unit(1, "Module 2", "55")).unwrap_err();
        assert!(err.to_string().contains("already called"), "{err:#}");
        let names: Vec<String> = list_units(&conn, 1).expect("list").into_iter().map(|u| u.name).collect();
        assert_eq!(names, ["Module 1", "Module 2"]);
    }

    /// `find` answers the row a division would write to, which is what lets a
    /// scan claim each row once: two entries under one label go to one row.
    #[test]
    fn two_entries_under_one_label_resolve_to_one_row() {
        let conn = db();
        let written = upsert(&conn, 1, &unit(7, "week", "Week 7 — A", None)).expect("insert");
        assert_eq!(find(&conn, 1, &unit(7, "week", "Week 7 — B", None)).expect("find"), Some(written.id));
        assert_eq!(find(&conn, 1, &unit(8, "week", "Week 8 — C", None)).expect("find"), None);
        // Unlabelled rows resolve by name alone.
        let reading = upsert(&conn, 1, &unit(16, "week", "Reading Days", None)).expect("insert");
        assert_eq!(find(&conn, 1, &unit(17, "week", "Reading Days", None)).expect("find"), Some(reading.id));
        assert_eq!(find(&conn, 1, &unit(17, "week", "Finals week", None)).expect("find"), None);
    }

    /// Re-syncing changes nothing (M13 acceptance): no duplicates, and the
    /// second pass reports no writes.
    #[test]
    fn re_syncing_is_a_no_op() {
        let conn = db();
        let unit = || NewUnit {
            starts_on: Some("2026-08-20".into()),
            ..canvas_unit(1, "Week 1 — Intro", "9")
        };
        assert_eq!(write(&conn, 1, &unit()), Outcome::Inserted);
        assert_eq!(write(&conn, 1, &unit()), Outcome::Unchanged, "reported a change");
        assert_eq!(list_units(&conn, 1).expect("list").len(), 1);
    }

    /// A unit that vanishes from Canvas is kept (SPEC §7.2) — a mid-semester
    /// reshuffle must not orphan the guide built from it.
    #[test]
    fn a_unit_that_disappears_from_canvas_is_retained() {
        let conn = db();
        for (ordinal, name) in [(1, "Module 1"), (2, "Module 2")] {
            write(&conn, 1, &canvas_unit(ordinal, name, &ordinal.to_string()));
        }
        // A later sync sees only Module 1. Nothing here deletes, so there is
        // no call to make — the assertion is that Module 2 is still listed.
        write(&conn, 1, &canvas_unit(1, "Module 1", "1"));
        let names: Vec<String> = list_units(&conn, 1)
            .expect("list")
            .into_iter()
            .map(|u| u.name)
            .collect();
        assert_eq!(names, ["Module 1", "Module 2"]);
    }

    #[test]
    fn caps_a_name_that_would_not_fit_a_heading() {
        let conn = db();
        write(&conn, 1, &unit(1, "week", &"W".repeat(400), None));
        let units = list_units(&conn, 1).expect("list");
        assert!(units[0].name.chars().count() <= MAX_UNIT_NAME + 1, "{}", units[0].name);
    }

    /// The migration's pass over rows from before labels existed: numbers and
    /// ranges from the names, and a number two rows would share left empty.
    #[test]
    fn backfills_labels_and_ranges_from_the_names() {
        let conn = db();
        for (class_id, ordinal, kind, name) in [
            (1, 14, "week", "No class (Nov. 24) — Thanksgiving Break"),
            (1, 15, "week", "Week 14 — Introduction to Deep Learning and Course Synthesis"),
            (4, 1, "part", "Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            (4, 2, "part", "Part I: Deep Learning (again)"),
            (3, 16, "week", "Reading Days — No Class (Reading Days)"),
        ] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, source)
                 VALUES (?1, ?2, ?3, ?4, 'syllabus')",
                params![class_id, ordinal, kind, name],
            )
            .expect("row");
        }
        backfill_labels(&conn).expect("backfill");
        let labels = |class_id: i64| {
            list_units(&conn, class_id)
                .expect("list")
                .into_iter()
                .map(|u| (u.number, u.first_week, u.last_week))
                .collect::<Vec<_>>()
        };
        assert_eq!(labels(1), [(None, None, None), (Some(14), None, None)]);
        assert_eq!(labels(4), [(Some(1), Some(1), Some(8)), (None, None, None)]);
        assert_eq!(labels(3), [(None, None, None)]);
    }

    // -----------------------------------------------------------------------
    // Weeks (SPEC §8.5)

    /// A range written into a name, read back out — what fills a Part's
    /// columns and what maps a `Weeks/` folder to its week.
    #[test]
    fn reads_the_week_range_out_of_a_part_s_name() {
        assert_eq!(
            parse_week_range("Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            Some((1, 8))
        );
        assert_eq!(
            parse_week_range("Part II: Reinforcement Learning and Alignment (Weeks 9-12)"),
            Some((9, 12))
        );
        assert_eq!(
            parse_week_range("Part III: Agentic AI in Medicine (Weeks 13-16)"),
            Some((13, 16))
        );
        // Dashes a syllabus actually uses, and a spelled-out range.
        assert_eq!(parse_week_range("Part I (Weeks 1\u{2013}8)"), Some((1, 8)));
        assert_eq!(parse_week_range("Part I (Weeks 1 \u{2014} 8)"), Some((1, 8)));
        assert_eq!(parse_week_range("Part I (Weeks 1 to 8)"), Some((1, 8)));
        // A single week stands for itself, which is what a week folder's own
        // name parses as.
        assert_eq!(parse_week_range("Week 03 \u{2014} Transformers"), Some((3, 3)));
        assert_eq!(parse_week_range("Week 5"), Some((5, 5)));

        // What a scan records: the model's own range when it states a sane
        // one, the name's otherwise, and nothing for a week.
        assert_eq!(declared_weeks("part", "Part I (Weeks 1-8)", None), Some((1, 8)));
        assert_eq!(declared_weeks("part", "Part I", Some((1, 8))), Some((1, 8)));
        assert_eq!(declared_weeks("part", "Part I (Weeks 1-8)", Some((3, 2))), Some((1, 8)));
        assert_eq!(declared_weeks("module", "Module 2 (Weeks 3-5)", None), Some((3, 5)));
        assert_eq!(declared_weeks("week", "Week 3 \u{2014} Transformers", None), None);
        assert_eq!(declared_weeks("part", "Part IV: Clinical Deployment", None), None);
    }

    /// Most unit names carry no range at all, and that is not an error — it is
    /// how a course that groups its weeks some other way reads.
    #[test]
    fn a_name_with_no_week_range_maps_to_nothing() {
        assert_eq!(parse_week_range("Part IV: Clinical Deployment"), None);
        assert_eq!(parse_week_range("Module 1"), None);
        assert_eq!(parse_week_range(""), None);
        assert_eq!(parse_week_range("Weekly Readings"), None);
        // A year is not a week number, and neither is a room.
        assert_eq!(parse_week_range("Week of 2026"), None);
        assert_eq!(parse_week_range("Week 0"), None);
    }

    /// The week comes from `units`, never from arithmetic (SPEC §1) — this is
    /// the case that proves why: Fundamentals skips Thanksgiving, so Week 14 is
    /// two weeks after Week 13 and `(date − start) / 7` is off by one from
    /// there on.
    #[test]
    fn resolves_a_week_from_the_course_s_own_dates_across_a_break() {
        let conn = db();
        for (ordinal, name, starts_on) in [
            (12, "Week 12 \u{2014} Clinical Evaluation", "2026-11-10"),
            (13, "Week 13 \u{2014} Model Lifecycle", "2026-11-17"),
            (14, "Week 14 \u{2014} Deep Learning", "2026-12-01"),
        ] {
            write(&conn, 1, &unit(ordinal, "week", name, Some(starts_on)));
        }
        let slots = week_slots(&conn, 1).expect("slots");
        assert_eq!(slots.len(), 3);

        assert_eq!(nearest_week(&slots, "2026-11-17"), Some(13));
        assert_eq!(nearest_week(&slots, "2026-12-01"), Some(14));
        // The Thanksgiving gap: arithmetic from Week 12 would call this Week 14.
        assert_eq!(nearest_week(&slots, "2026-11-24"), Some(13));
        // Outside the published range, the nearest published week still wins.
        assert_eq!(nearest_week(&slots, "2026-12-08"), Some(14));
    }

    /// A course with no weeks of its own still files lectures by week — each
    /// one landing in the Part whose range, held as data, contains it.
    #[test]
    fn a_part_numbered_course_gets_its_weeks_from_the_ranges() {
        let conn = db();
        for (ordinal, name) in [
            (1, "Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            (2, "Part II: Reinforcement Learning and Alignment (Weeks 9-12)"),
            (3, "Part III: Agentic AI in Medicine (Weeks 13-16)"),
        ] {
            let part = NewUnit {
                weeks: declared_weeks("part", name, None),
                ..unit(ordinal, "part", name, None)
            };
            write(&conn, 4, &part);
        }
        let slots = week_slots(&conn, 4).expect("slots");
        assert_eq!(slots.len(), 16, "the three ranges cover weeks 1–16");
        assert!(slot_for_week(&conn, 4, 1).unwrap().unwrap().unit_name.starts_with("Part I:"));
        assert!(slot_for_week(&conn, 4, 9).unwrap().unwrap().unit_name.starts_with("Part II:"));
        assert!(slot_for_week(&conn, 4, 16).unwrap().unwrap().unit_name.starts_with("Part III:"));
        assert!(slot_for_week(&conn, 4, 17).unwrap().is_none(), "no Part covers week 17");
        // No dates anywhere, so nothing is defaulted — the form asks.
        assert_eq!(nearest_week(&slots, "2026-09-10"), None);
        // The folder carries no topic, because the Part's name is not the
        // week's name.
        assert_eq!(slots[2].folder, "Week 03");
        // A Part recorded without a range — its name carries none and the
        // reader stated none — covers nothing.
        write(&conn, 2, &unit(1, "part", "Part I: Foundations", None));
        assert!(week_slots(&conn, 2).expect("slots").is_empty());
    }

    /// The week's own unit wins over any Part that also spans it: it is the
    /// finer division, and a meeting sits inside exactly one.
    #[test]
    fn a_week_unit_outranks_a_part_that_spans_it() {
        let conn = db();
        let part = NewUnit {
            weeks: Some((1, 8)),
            ..unit(1, "part", "Part I (Weeks 1-8)", None)
        };
        write(&conn, 1, &part);
        write(&conn, 1, &unit(3, "week", "Week 3 \u{2014} Transformers", Some("2026-09-10")));

        let slots = week_slots(&conn, 1).expect("slots");
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].unit_name, "Week 3 \u{2014} Transformers");
    }

    /// The folder name is what joins a filed transcript back to its week, so
    /// building it and reading it back have to agree.
    #[test]
    fn builds_a_week_folder_and_reads_it_back() {
        assert_eq!(
            week_folder(3, Some("Week 3 \u{2014} Data Exploration, Processing, and Quality")),
            "Week 03 \u{2014} Data Exploration, Processing, and Quality"
        );
        // A week the course named without numbering it keeps its whole name.
        assert_eq!(
            week_folder(16, Some("Reading Days \u{2014} No Class")),
            "Week 16 \u{2014} Reading Days \u{2014} No Class"
        );
        assert_eq!(week_folder(7, Some("Week 7")), "Week 07");
        assert_eq!(week_folder(7, None), "Week 07");
        // A name that would repoint a write flattens rather than being refused.
        assert!(!week_folder(2, Some("Week 2 \u{2014} A/B: testing")).contains('/'));

        for (week, name) in [(1, "Week 1 \u{2014} Intro"), (12, "Week 12"), (16, "Reading Days")] {
            let folder = week_folder(week, Some(name));
            assert_eq!(
                week_from_rel_path(&format!("Weeks/{folder}/2026-09-10 \u{2014} Lecture.md")),
                Some(week),
                "{folder}"
            );
        }
        assert_eq!(week_from_rel_path("Module 1/Slides/deck.pdf"), None);
        assert_eq!(week_from_rel_path("Weeks/Loose Notes/x.md"), None);
    }

    /// Where each course is on a given day, read from its own published dates
    /// (SPEC §8.5) and never from arithmetic, which the Thanksgiving gap
    /// breaks. The rows are the seeded syllabi as the scan stored them, on the
    /// seeded class ids.
    #[test]
    fn names_the_division_a_course_is_in_today() {
        let conn = db();
        // Fundamentals: Tuesdays, Week 13 on Nov 17 and Week 14 on Dec 1.
        for (ordinal, name, on) in [
            (1, "Week 1 \u{2014} Introduction to AI in Medicine", "2026-08-25"),
            (2, "Week 2 \u{2014} Responsible AI, Ethics, and Governance", "2026-09-01"),
            (3, "Week 3 \u{2014} Biomedical Data Foundations", "2026-09-08"),
            (13, "Week 13 \u{2014} Model Lifecycle, MLOps, and Reproducibility", "2026-11-17"),
            (14, "Week 14 \u{2014} Introduction to Deep Learning and Course Synthesis", "2026-12-01"),
        ] {
            write(&conn, 1, &unit(ordinal, "week", name, Some(on)));
        }
        // Biostatistics: Thursdays, with a week the syllabus declares as no class
        // and one it named without numbering.
        for (ordinal, name, on) in [
            (1, "Week 1 \u{2014} Introduction to Biostatistics", "2026-08-20"),
            (2, "Week 2 \u{2014} Study Designs", "2026-08-27"),
            (3, "Week 3 \u{2014} Data Exploration, Processing, and Quality", "2026-09-03"),
            (14, "Week 14 \u{2014} Project preparation", "2026-11-19"),
            (15, "Week 15 \u{2014} No Class (Thanksgiving Week)", "2026-11-26"),
        ] {
            write(&conn, 3, &unit(ordinal, "week", name, Some(on)));
        }
        // A row the scan stores as `module` when the model omits the kind,
        // since its name opens with neither "Week" nor "Part". Dated, so it is
        // still where the course is on that day.
        write(
            &conn,
            3,
            &unit(16, "module", "Reading Days \u{2014} No Class (Reading Days)", Some("2026-12-03")),
        );
        // Applied Generative AI: three Parts naming their week ranges, no dates.
        for (ordinal, name) in [
            (1, "Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            (2, "Part II: Reinforcement Learning and Alignment (Weeks 9-12)"),
            (3, "Part III: Agentic AI in Medicine (Weeks 13-16)"),
        ] {
            write(&conn, 4, &unit(ordinal, "part", name, None));
        }

        let now = |class_id: i64, today: &str| {
            current_unit(&conn, class_id, today)
                .expect("resolve")
                .map(|u| u.name)
        };
        for (class_id, today, expected) in [
            // The semester boundary: nothing before the first published date,
            // and the first week from its own day.
            (3, "2026-08-19", None),
            (3, "2026-08-20", Some("Week 1 \u{2014} Introduction to Biostatistics")),
            // The brief's day: Week 2 for every dated course, with Week 3 of
            // Biostatistics beginning the next morning.
            (1, "2026-09-02", Some("Week 2 \u{2014} Responsible AI, Ethics, and Governance")),
            (3, "2026-09-02", Some("Week 2 \u{2014} Study Designs")),
            (3, "2026-09-03", Some("Week 3 \u{2014} Data Exploration, Processing, and Quality")),
            // The skipped week: Nov 24 is still Week 13, which arithmetic from
            // Week 1 would call Week 14.
            (1, "2026-11-24", Some("Week 13 \u{2014} Model Lifecycle, MLOps, and Reproducibility")),
            (1, "2026-12-01", Some("Week 14 \u{2014} Introduction to Deep Learning and Course Synthesis")),
            // A week the syllabus declared as no class reads as it wrote it.
            (3, "2026-11-26", Some("Week 15 \u{2014} No Class (Thanksgiving Week)")),
            // The `module`-kind row: on its day the course is there, not still
            // in the last row the scan happened to call a week.
            (3, "2026-12-04", Some("Reading Days \u{2014} No Class (Reading Days)")),
            // Past the last published date the last week stays current.
            (1, "2026-12-20", Some("Week 14 \u{2014} Introduction to Deep Learning and Course Synthesis")),
            // A Part course with no dates says nothing, whatever the day.
            (4, "2026-09-02", None),
            (4, "2026-11-26", None),
            // Not a date at all.
            (3, "today", None),
        ] {
            assert_eq!(now(class_id, today).as_deref(), expected, "class {class_id} on {today}");
        }
        // The whole row comes back, so a caller can match on its id.
        let week = current_unit(&conn, 1, "2026-09-02").expect("resolve").expect("a week");
        assert_eq!(week.kind, "week");
        assert_eq!(week.ordinal, 2);
        // A class with no divisions at all has no answer either.
        assert_eq!(now(2, "2026-09-02"), None);
    }

    /// Two rows can share an ordinal — the scan numbers a row by its position
    /// when the model omits the ordinal — and the date decides, not the number
    /// and not which row was inserted first.
    #[test]
    fn a_duplicate_ordinal_does_not_hide_the_row_whose_date_won() {
        let conn = db();
        for (name, on) in [
            ("Reading Days \u{2014} Week 16", "2026-12-03"),
            ("Final Exam \u{2014} Week 16", "2026-12-10"),
        ] {
            write(&conn, 2, &unit(16, "week", name, Some(on)));
        }
        let name = |today: &str| {
            current_unit(&conn, 2, today).expect("resolve").map(|u| u.name)
        };
        assert_eq!(name("2026-12-04").as_deref(), Some("Reading Days \u{2014} Week 16"));
        assert_eq!(name("2026-12-11").as_deref(), Some("Final Exam \u{2014} Week 16"));
    }

    /// Two divisions starting on one day: the week wins over a coarser
    /// division it sits inside, and between two weeks the later one does.
    /// A rule rather than a reading of the syllabus — what matters is that
    /// the answer is one row, the same one every time.
    #[test]
    fn a_shared_start_date_goes_to_the_week_then_to_the_later_one() {
        let conn = db();
        for (ordinal, kind, name, on) in [
            (1, "part", "Part I (Weeks 1-8)", "2026-08-25"),
            (1, "week", "Week 1 \u{2014} Intro", "2026-08-25"),
            (3, "week", "Week 3 \u{2014} Later", "2026-09-01"),
            (2, "week", "Week 2 \u{2014} Earlier", "2026-09-01"),
        ] {
            write(&conn, 2, &unit(ordinal, kind, name, Some(on)));
        }
        let name = |today: &str| {
            current_unit(&conn, 2, today).expect("resolve").map(|u| u.name)
        };
        assert_eq!(name("2026-08-25").as_deref(), Some("Week 1 \u{2014} Intro"));
        // Inserted after Week 3, so the later ordinal wins and not the later row.
        assert_eq!(name("2026-09-01").as_deref(), Some("Week 3 \u{2014} Later"));
    }
}
