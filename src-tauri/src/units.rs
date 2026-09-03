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
//! A folder is not a third source. It is where material sits, not something the
//! course declared — listing the top-level folders as divisions put a second
//! numbering sequence under the course's own and labelled it a guess. What the
//! folder does know is recorded on the declared unit's own row: see
//! `attach_folder_paths`.
//!
//! Nothing here ever deletes. A unit that stops appearing in Canvas is kept
//! (SPEC §7.2): a mid-semester reshuffle must not silently orphan a guide.

use anyhow::{bail, Result};
use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
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
    pub rel_path: Option<String>,
    pub starts_on: Option<String>,
    pub ends_on: Option<String>,
    /// canvas | syllabus | folder — the workspace marks `folder` as inferred.
    pub source: String,
}

/// One division on its way into the table, from any of the three sources.
pub struct NewUnit {
    pub ordinal: i64,
    pub kind: String,
    pub name: String,
    pub canvas_id: Option<String>,
    pub rel_path: Option<String>,
    pub starts_on: Option<String>,
    pub ends_on: Option<String>,
    pub source: &'static str,
}

/// canvas > syllabus > folder. Higher wins; an equal source refreshes its own
/// rows, which is what makes a re-sync an update rather than a duplicate.
///
/// `list_units`' `ORDER BY` encodes the same order in SQL. Two hand-kept
/// orderings, because binding it once would mean building the clause with
/// `format!` — so a fourth source means changing both, and this is the note
/// saying so.
fn rank(source: &str) -> u8 {
    match source {
        "canvas" => 3,
        "syllabus" => 2,
        _ => 1,
    }
}

/// Every division for a class, the declared ones first.
///
/// Ordering by ordinal alone interleaves the sources, and the result reads as
/// nonsense: a folder called "Module 1" lands between Week 1 and Week 2 because
/// both call themselves first. Sorting by source before ordinal keeps each
/// source's own sequence intact and puts what the course actually declared
/// first. The `CASE` repeats `rank()`'s order in SQL — see the note there.
pub fn list_units(conn: &Connection, class_id: i64) -> Result<Vec<UnitInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, kind, name, rel_path, starts_on, ends_on, source
         FROM units WHERE class_id = ?1
         ORDER BY CASE source WHEN 'canvas' THEN 0 WHEN 'syllabus' THEN 1 ELSE 2 END,
                  ordinal, id",
    )?;
    let rows = stmt
        .query_map([class_id], read_unit)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One row selected as `id, ordinal, kind, name, rel_path, starts_on, ends_on,
/// source` — the column order every `UnitInfo` read here uses.
fn read_unit(row: &rusqlite::Row<'_>) -> rusqlite::Result<UnitInfo> {
    Ok(UnitInfo {
        id: row.get(0)?,
        ordinal: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        rel_path: row.get(4)?,
        starts_on: row.get(5)?,
        ends_on: row.get(6)?,
        source: row.get(7)?,
    })
}

fn unit_by_id(conn: &Connection, id: i64) -> Result<Option<UnitInfo>> {
    Ok(conn
        .query_row(
            "SELECT id, ordinal, kind, name, rel_path, starts_on, ends_on, source
             FROM units WHERE id = ?1",
            [id],
            read_unit,
        )
        .optional()?)
}

/// The course's word for this division, taken from how it named it.
///
/// Only the three SPEC §5 kinds exist, so an unrecognized name falls back to
/// `module` — the generic one. `kind` groups and labels; `name` is what the
/// reader actually sees, and it is never normalized.
pub fn kind_for_name(name: &str) -> &'static str {
    let lower = name.trim().to_lowercase();
    let first = lower.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("");
    match first {
        "week" | "wk" => "week",
        "part" => "part",
        _ => "module",
    }
}

/// Inserts or refreshes one division, honouring source precedence.
///
/// Returns whether the row changed, so a sync can report "3 new, 11 unchanged"
/// rather than claiming to have written what was already there.
pub fn upsert(conn: &Connection, class_id: i64, unit: &NewUnit) -> Result<bool> {
    let name = crate::db::truncate(unit.name.trim(), MAX_UNIT_NAME);
    if name.is_empty() {
        bail!("a unit needs a name");
    }
    let kind = if UNIT_KINDS.contains(&unit.kind.as_str()) {
        unit.kind.clone()
    } else {
        kind_for_name(&name).to_string()
    };

    type Row = (
        i64,
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    );
    let map = |row: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ))
    };
    // Identity is the Canvas id wherever there is one. A professor renaming a
    // published module is the same division under a new name, and matching on
    // name alone would insert a second row and go on presenting the old name
    // as something the course still declares.
    let by_canvas_id: Option<Row> = match unit.canvas_id.as_deref() {
        Some(canvas_id) => conn
            .query_row(
                "SELECT id, source, ordinal, kind, canvas_id, rel_path, starts_on, ends_on, name
                 FROM units WHERE class_id = ?1 AND canvas_id = ?2",
                params![class_id, canvas_id],
                map,
            )
            .optional()?,
        None => None,
    };
    // `.optional()?` rather than `.ok()`: swallowing every error as "no such
    // row" turned a locked database into an INSERT that then failed on the
    // uniqueness constraint, reporting a cause that had nothing to do with it.
    let existing = match by_canvas_id {
        Some(row) => Some(row),
        None => conn
            .query_row(
                "SELECT id, source, ordinal, kind, canvas_id, rel_path, starts_on, ends_on, name
                 FROM units WHERE class_id = ?1 AND name = ?2",
                params![class_id, name],
                map,
            )
            .optional()?,
    };

    match existing {
        // A folder called "Module 1" must not overwrite what Canvas says
        // Module 1 is — including its dates, which a folder never has.
        Some((_, current, ..)) if rank(unit.source) < rank(&current) => Ok(false),
        Some((
            id,
            current,
            ordinal,
            current_kind,
            canvas_id,
            rel_path,
            starts_on,
            ends_on,
            current_name,
        )) => {
            // A higher source knows the division's name, order and dates; it
            // does not know where the material sits on disk, which only the
            // folder source ever learns. So a *takeover* fills fields in rather
            // than replacing them — otherwise Canvas superseding a folder would
            // blank the path its guide reads from.
            //
            // Within one source there is nothing to preserve: the incoming row
            // simply is the current state, and carrying an old value forward
            // would make a date the course removed impossible to clear.
            // Fell through to the name lookup and found a row belonging to a
            // different Canvas module. Two divisions really can share a topic
            // name — a term with four "Project Presentations" weeks has four —
            // and `UNIQUE(class_id, name)` holds only one of them. Refused
            // loudly rather than letting the second quietly overwrite the
            // first and counting it as "already recorded".
            if let (Some(incoming), Some(held)) = (unit.canvas_id.as_deref(), canvas_id.as_deref())
            {
                if incoming != held {
                    bail!("another division of this course is already called '{name}'");
                }
            }
            let takeover = rank(unit.source) > rank(&current);
            let keep = |incoming: &Option<String>, held: &Option<String>| match (takeover, incoming)
            {
                (true, None) => held.clone(),
                _ => incoming.clone(),
            };
            let merged_canvas_id = keep(&unit.canvas_id, &canvas_id);
            let merged_starts = keep(&unit.starts_on, &starts_on);
            let merged_ends = keep(&unit.ends_on, &ends_on);
            // `rel_path` is the exception in both directions: no source above
            // `folder` ever supplies one, so an incoming None is silence rather
            // than a correction.
            let merged_rel_path = unit.rel_path.clone().or_else(|| rel_path.clone());
            let unchanged = ordinal == unit.ordinal
                && current_kind == kind
                && current == unit.source
                && current_name == name
                && canvas_id == merged_canvas_id
                && rel_path == merged_rel_path
                && starts_on == merged_starts
                && ends_on == merged_ends;
            if unchanged {
                return Ok(false);
            }
            conn.execute(
                "UPDATE units
                 SET ordinal = ?1, kind = ?2, name = ?3, canvas_id = ?4, rel_path = ?5,
                     starts_on = ?6, ends_on = ?7, source = ?8
                 WHERE id = ?9",
                params![
                    unit.ordinal,
                    kind,
                    name,
                    merged_canvas_id,
                    merged_rel_path,
                    merged_starts,
                    merged_ends,
                    unit.source,
                    id
                ],
            )?;
            Ok(true)
        }
        None => {
            conn.execute(
                "INSERT INTO units
                 (class_id, ordinal, kind, name, canvas_id, rel_path, starts_on, ends_on, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    class_id,
                    unit.ordinal,
                    kind,
                    name,
                    unit.canvas_id,
                    unit.rel_path,
                    unit.starts_on,
                    unit.ends_on,
                    unit.source
                ],
            )?;
            Ok(true)
        }
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
/// material lives; the folder source knows only the path. When both describe
/// the same division this is what joins them, so §8.1's guide can find the
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
/// A course that numbers its weeks supplies them directly. Applied Generative
/// AI numbers none — it declares three Parts whose names carry the week ranges
/// they span — so its weeks are read out of those ranges, and each maps to the
/// Part that contains it. A course that declares neither gets no weeks, and a
/// lecture for it routes through `_Inbox/` for the sorter to place (SPEC §7.1).
pub fn week_slots(conn: &Connection, class_id: i64) -> Result<Vec<WeekSlot>> {
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, name, starts_on FROM units
         WHERE class_id = ?1 AND kind = 'week' ORDER BY ordinal, id",
    )?;
    let weeks = stmt
        .query_map([class_id], |row| {
            let unit_id: i64 = row.get(0)?;
            let week: i64 = row.get(1)?;
            let unit_name: String = row.get(2)?;
            Ok(WeekSlot {
                week,
                folder: week_folder(week, Some(&unit_name)),
                unit_id,
                unit_name,
                meets_on: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !weeks.is_empty() {
        return Ok(weeks);
    }

    let mut stmt = conn.prepare(
        "SELECT id, name FROM units WHERE class_id = ?1 AND kind = 'part' ORDER BY ordinal, id",
    )?;
    let parts = stmt
        .query_map([class_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut slots: Vec<WeekSlot> = Vec::new();
    for (unit_id, unit_name) in parts {
        let Some((first, last)) = parse_week_range(&unit_name) else {
            continue;
        };
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

/// The week a course is in on `today`: the one whose published meeting date
/// is the latest on or before it — a week runs from its meeting to the next
/// one's, so that is the slot containing today.
///
/// `None` before the first published date, and `None` throughout for a course
/// that published none: the alternative is a week number from arithmetic,
/// which SPEC §1 forbids. Past the last published date the last week stays
/// current, because nothing the course published says it has ended.
pub fn current_week(slots: &[WeekSlot], today: &str) -> Option<i64> {
    let day = |iso: &str| NaiveDate::parse_from_str(iso, "%Y-%m-%d").ok();
    let target = day(today)?;
    slots
        .iter()
        .filter_map(|slot| {
            let meets = day(slot.meets_on.as_deref()?)?;
            (meets <= target).then_some((meets, slot.week))
        })
        // Two weeks published for one date: the later is where the course is.
        .max()
        .map(|(_, week)| week)
}

/// The division a course is in on `today` — SPEC §8.5's join run against the
/// calendar instead of a filed lecture: the current week's own unit where the
/// course numbers its weeks, or the Part whose range contains that week.
///
/// A Part-numbered course resolves through the same slots, and its slots
/// carry no dates (the ranges name weeks, not days), so today it has no
/// current division and the UI says nothing.
pub fn current_unit(conn: &Connection, class_id: i64, today: &str) -> Result<Option<UnitInfo>> {
    let slots = week_slots(conn, class_id)?;
    let Some(week) = current_week(&slots, today) else {
        return Ok(None);
    };
    let Some(slot) = slots.iter().find(|slot| slot.week == week) else {
        return Ok(None);
    };
    unit_by_id(conn, slot.unit_id)
}

/// `Part I: … (Weeks 1-8)` → `Some((1, 8))`.
///
/// The one non-trivial mapping in SPEC §8.5. Applied Generative AI declares no
/// weeks of its own, only three Parts whose names carry the week ranges they
/// span, so a lecture's week finds its division by reading that range back out.
/// A name with no range is not an error — most unit names have none.
pub fn parse_week_range(name: &str) -> Option<(i64, i64)> {
    /// Past this, the digits are a year or a room number rather than a week.
    const MAX_WEEK: i64 = 60;
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

    /// The real migration against the tables it alters, so the schema these
    /// tests run on is the schema the app runs on.
    fn db() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(
            "CREATE TABLE classes (id INTEGER PRIMARY KEY);
             INSERT INTO classes (id) VALUES (1);
             CREATE TABLE deadline_proposals (id INTEGER PRIMARY KEY);",
        )
        .expect("fixture");
        conn.execute_batch(include_str!("../migrations/0007_units.sql"))
            .expect("migration");
        conn.execute_batch(include_str!("../migrations/0008_folder_units.sql"))
            .expect("migration");
        conn
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

    /// The precedence rule, which is the whole reason `source` is stored.
    #[test]
    fn a_syllabus_week_never_overwrites_what_canvas_declared() {
        let conn = db();
        let canvas = NewUnit {
            ordinal: 3,
            kind: "module".into(),
            name: "Module 1".into(),
            canvas_id: Some("55".into()),
            rel_path: None,
            starts_on: Some("2026-08-20".into()),
            ends_on: None,
            source: "canvas",
        };
        assert!(upsert(&conn, 1, &canvas).expect("insert"));

        let syllabus = NewUnit {
            ordinal: 1,
            kind: "module".into(),
            name: "Module 1".into(),
            canvas_id: None,
            rel_path: None,
            starts_on: Some("2026-09-01".into()),
            ends_on: None,
            source: "syllabus",
        };
        assert!(!upsert(&conn, 1, &syllabus).expect("upsert"), "the syllabus won");

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
        let declared = NewUnit {
            ordinal: 1,
            kind: "module".into(),
            name: "Module 1".into(),
            canvas_id: None,
            rel_path: None,
            starts_on: None,
            ends_on: None,
            source: "syllabus",
        };
        assert!(upsert(&conn, 1, &declared).expect("insert"));
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
            ordinal: 1,
            kind: "week".into(),
            name: "Week 1".into(),
            canvas_id: Some("9".into()),
            rel_path: None,
            starts_on: Some("2026-08-20".into()),
            ends_on: None,
            source: "canvas",
        };
        assert!(upsert(&conn, 1, &with_date).expect("insert"));

        let cleared = NewUnit {
            starts_on: None,
            ..with_date
        };
        assert!(upsert(&conn, 1, &cleared).expect("update"), "reported no change");
        assert_eq!(list_units(&conn, 1).expect("list")[0].starts_on, None);
    }

    /// Renaming a published module is the same division under a new name.
    /// Matched on name alone it would become a second one, and the old name
    /// would go on being presented as something the course declares.
    #[test]
    fn a_renamed_canvas_module_keeps_its_row() {
        let conn = db();
        let before = NewUnit {
            ordinal: 1,
            kind: "module".into(),
            name: "Module 1".into(),
            canvas_id: Some("55".into()),
            rel_path: Some("Module 1".into()),
            starts_on: None,
            ends_on: None,
            source: "canvas",
        };
        assert!(upsert(&conn, 1, &before).expect("insert"));
        let after = NewUnit {
            name: "Module 1 — Foundations".into(),
            ..before
        };
        assert!(upsert(&conn, 1, &after).expect("rename"));

        let units = list_units(&conn, 1).expect("list");
        assert_eq!(units.len(), 1, "the rename forked the division");
        assert_eq!(units[0].name, "Module 1 — Foundations");
        assert_eq!(units[0].rel_path.as_deref(), Some("Module 1"));
    }

    /// Re-syncing changes nothing (M13 acceptance): no duplicates, and the
    /// second pass reports no writes.
    #[test]
    fn re_syncing_is_a_no_op() {
        let conn = db();
        let unit = || NewUnit {
            ordinal: 1,
            kind: "week".into(),
            name: "Week 1 — Intro".into(),
            canvas_id: Some("9".into()),
            rel_path: None,
            starts_on: Some("2026-08-20".into()),
            ends_on: None,
            source: "canvas",
        };
        assert!(upsert(&conn, 1, &unit()).expect("first"));
        assert!(!upsert(&conn, 1, &unit()).expect("second"), "reported a change");
        assert_eq!(list_units(&conn, 1).expect("list").len(), 1);
    }

    /// A unit that vanishes from Canvas is kept (SPEC §7.2) — a mid-semester
    /// reshuffle must not orphan the guide built from it.
    #[test]
    fn a_unit_that_disappears_from_canvas_is_retained() {
        let conn = db();
        for (ordinal, name) in [(1, "Module 1"), (2, "Module 2")] {
            upsert(
                &conn,
                1,
                &NewUnit {
                    ordinal,
                    kind: "module".into(),
                    name: name.into(),
                    canvas_id: Some(ordinal.to_string()),
                    rel_path: None,
                    starts_on: None,
                    ends_on: None,
                    source: "canvas",
                },
            )
            .expect("insert");
        }
        // A later sync sees only Module 1. Nothing here deletes, so there is
        // no call to make — the assertion is that Module 2 is still listed.
        upsert(
            &conn,
            1,
            &NewUnit {
                ordinal: 1,
                kind: "module".into(),
                name: "Module 1".into(),
                canvas_id: Some("1".into()),
                rel_path: None,
                starts_on: None,
                ends_on: None,
                source: "canvas",
            },
        )
        .expect("resync");
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
        upsert(
            &conn,
            1,
            &NewUnit {
                ordinal: 1,
                kind: "week".into(),
                name: "W".repeat(400),
                canvas_id: None,
                rel_path: None,
                starts_on: None,
                ends_on: None,
                source: "syllabus",
            },
        )
        .expect("insert");
        let units = list_units(&conn, 1).expect("list");
        assert!(units[0].name.chars().count() <= MAX_UNIT_NAME + 1, "{}", units[0].name);
    }

    // -----------------------------------------------------------------------
    // Weeks (SPEC §8.5)

    /// The one non-trivial mapping in M14: Applied Generative AI declares no
    /// weeks, only Parts carrying the ranges they span.
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
            upsert(
                &conn,
                1,
                &NewUnit {
                    ordinal,
                    kind: "week".into(),
                    name: name.into(),
                    canvas_id: None,
                    rel_path: None,
                    starts_on: Some(starts_on.into()),
                    ends_on: None,
                    source: "syllabus",
                },
            )
            .expect("insert");
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
    /// one landing in the Part whose range contains it.
    #[test]
    fn a_part_numbered_course_gets_its_weeks_from_the_ranges() {
        let conn = db();
        for (ordinal, name) in [
            (1, "Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            (2, "Part II: Reinforcement Learning and Alignment (Weeks 9-12)"),
            (3, "Part III: Agentic AI in Medicine (Weeks 13-16)"),
        ] {
            upsert(
                &conn,
                1,
                &NewUnit {
                    ordinal,
                    kind: "part".into(),
                    name: name.into(),
                    canvas_id: None,
                    rel_path: None,
                    starts_on: None,
                    ends_on: None,
                    source: "syllabus",
                },
            )
            .expect("insert");
        }
        let slots = week_slots(&conn, 1).expect("slots");
        assert_eq!(slots.len(), 16, "the three ranges cover weeks 1–16");
        assert!(slot_for_week(&conn, 1, 1).unwrap().unwrap().unit_name.starts_with("Part I:"));
        assert!(slot_for_week(&conn, 1, 9).unwrap().unwrap().unit_name.starts_with("Part II:"));
        assert!(slot_for_week(&conn, 1, 16).unwrap().unwrap().unit_name.starts_with("Part III:"));
        assert!(slot_for_week(&conn, 1, 17).unwrap().is_none(), "no Part covers week 17");
        // No dates anywhere, so nothing is defaulted — the form asks.
        assert_eq!(nearest_week(&slots, "2026-09-10"), None);
        // The folder carries no topic, because the Part's name is not the
        // week's name.
        assert_eq!(slots[2].folder, "Week 03");
    }

    /// The week's own unit wins over any Part that also spans it: it is the
    /// finer division, and a meeting sits inside exactly one.
    #[test]
    fn a_week_unit_outranks_a_part_that_spans_it() {
        let conn = db();
        upsert(
            &conn,
            1,
            &NewUnit {
                ordinal: 1,
                kind: "part".into(),
                name: "Part I (Weeks 1-8)".into(),
                canvas_id: None,
                rel_path: None,
                starts_on: None,
                ends_on: None,
                source: "syllabus",
            },
        )
        .expect("part");
        upsert(
            &conn,
            1,
            &NewUnit {
                ordinal: 3,
                kind: "week".into(),
                name: "Week 3 \u{2014} Transformers".into(),
                canvas_id: None,
                rel_path: None,
                starts_on: Some("2026-09-10".into()),
                ends_on: None,
                source: "syllabus",
            },
        )
        .expect("week");

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
        let conn = crate::db::memory_db();
        let unit = |ordinal: i64, kind: &str, name: &str, starts_on: Option<&str>| NewUnit {
            ordinal,
            kind: kind.into(),
            name: name.into(),
            canvas_id: None,
            rel_path: None,
            starts_on: starts_on.map(Into::into),
            ends_on: None,
            source: "syllabus",
        };
        // Fundamentals: Tuesdays, Week 13 on Nov 17 and Week 14 on Dec 1.
        for (ordinal, name, on) in [
            (1, "Week 1 \u{2014} Introduction to AI in Medicine", "2026-08-25"),
            (2, "Week 2 \u{2014} Responsible AI, Ethics, and Governance", "2026-09-01"),
            (3, "Week 3 \u{2014} Biomedical Data Foundations", "2026-09-08"),
            (13, "Week 13 \u{2014} Model Lifecycle, MLOps, and Reproducibility", "2026-11-17"),
            (14, "Week 14 \u{2014} Introduction to Deep Learning and Course Synthesis", "2026-12-01"),
        ] {
            upsert(&conn, 1, &unit(ordinal, "week", name, Some(on))).expect("insert");
        }
        // Biostatistics: Thursdays, with a week the syllabus declares as no class
        // and one it named without numbering.
        for (ordinal, name, on) in [
            (1, "Week 1 \u{2014} Introduction to Biostatistics", "2026-08-20"),
            (2, "Week 2 \u{2014} Study Designs", "2026-08-27"),
            (3, "Week 3 \u{2014} Data Exploration, Processing, and Quality", "2026-09-03"),
            (14, "Week 14 \u{2014} Project preparation", "2026-11-19"),
            (15, "Week 15 \u{2014} No Class (Thanksgiving Week)", "2026-11-26"),
            (16, "Reading Days \u{2014} No Class (Reading Days)", "2026-12-03"),
        ] {
            upsert(&conn, 3, &unit(ordinal, "week", name, Some(on))).expect("insert");
        }
        // Applied Generative AI: three Parts naming their week ranges, no dates.
        for (ordinal, name) in [
            (1, "Part I: Deep Learning to Large Language Models (Weeks 1-8)"),
            (2, "Part II: Reinforcement Learning and Alignment (Weeks 9-12)"),
            (3, "Part III: Agentic AI in Medicine (Weeks 13-16)"),
        ] {
            upsert(&conn, 4, &unit(ordinal, "part", name, None)).expect("insert");
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
        // The card labels the line with the course's own word.
        let week = current_unit(&conn, 1, "2026-09-02").expect("resolve").expect("a week");
        assert_eq!(week.kind, "week");
        assert_eq!(week.ordinal, 2);
        // A class with no divisions at all has no answer either.
        assert_eq!(now(2, "2026-09-02"), None);
    }
}
