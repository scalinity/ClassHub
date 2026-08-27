//! SPEC §5/§7.2 — a course's own divisions, from whichever source knows them.
//!
//! The four courses do not share a shape: 14 weekly topics, 15 weekly topics,
//! 3 Parts across 16 weeks, and one that publishes no structure at all. Any
//! layout the app imposed would be wrong for at least two of them, so it reads
//! each course's own and stores the course's own words.
//!
//! Three sources can supply them, in precedence order **canvas > syllabus >
//! folder**:
//!
//! - **canvas** — the course's published modules. Ground truth when it exists,
//!   which today it does not: none of the four courses uses Canvas Modules.
//! - **syllabus** — the weekly schedule inside the syllabus, read by the same
//!   `syllabus_scan` that already reads it for deadlines.
//! - **folder** — the top-level folders in the class tree, which is what the
//!   app inferred before this table existed.
//!
//! `source` is stored rather than resolved away, because "Module 1 exists
//! because a folder is called that" and "Module 1 exists because the course
//! says so" are different claims and the UI marks them differently.
//!
//! Nothing here ever deletes. A unit that stops appearing in Canvas is kept
//! (SPEC §7.2): a mid-semester reshuffle must not silently orphan a guide.

use anyhow::{bail, Result};
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
/// above what the app merely inferred.
pub fn list_units(conn: &Connection, class_id: i64) -> Result<Vec<UnitInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, ordinal, kind, name, rel_path, starts_on, ends_on, source
         FROM units WHERE class_id = ?1
         ORDER BY CASE source WHEN 'canvas' THEN 0 WHEN 'syllabus' THEN 1 ELSE 2 END,
                  ordinal, id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
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
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
}
