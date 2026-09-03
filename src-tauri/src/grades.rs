//! SPEC §11 — the weighted grade tracker. The math lives here (weighted over
//! categories that have graded items, renormalized) so the chat tools, the UI
//! commands, and the class card all compute the same number.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

/// Weights are compared to 100 within this much. The UI applies the same
/// value, so a set summing to 100.03 cannot read clean in one place and
/// flagged in the other.
pub const WEIGHT_EPSILON: f64 = 0.01;

/// The weighted-grade formula, in one place.
///
/// Two callers accumulate it from different row sources — `list_grades` from
/// rows it already read, `weighted_grade` from a GROUP BY — and the module
/// header promises the class card, the Grades tab and chat all report the same
/// number. Sharing the gate and the step makes that true by construction
/// rather than by comment.
#[derive(Default)]
pub(crate) struct GradeAccumulator {
    weight_sum: f64,
    acc: f64,
}

impl GradeAccumulator {
    /// A category counts only when it has a positive weight and something
    /// graded; the total is renormalized over whatever qualified.
    fn add(&mut self, weight: f64, score_sum: f64, max_sum: f64) {
        if max_sum > 0.0 && weight > 0.0 {
            self.weight_sum += weight;
            self.acc += weight * (score_sum / max_sum);
        }
    }

    fn percent(&self) -> Option<f64> {
        (self.weight_sum > 0.0).then(|| self.acc / self.weight_sum * 100.0)
    }
}

use crate::db::{audit, emit_hub_change, with_conn};

const MAX_NAME_CHARS: usize = 80;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeItem {
    pub id: i64,
    pub name: String,
    pub score: f64,
    pub max_score: f64,
    pub graded_at: Option<String>,
    /// Set when a Canvas sync recorded it. The next sync overwrites the score
    /// with Canvas's number, so a hand edit is a correction until then.
    pub canvas_assignment_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradeCategory {
    pub id: i64,
    pub name: String,
    pub weight: f64,
    /// Points earned across this category's items, when any exist.
    pub percent: Option<f64>,
    pub items: Vec<GradeItem>,
    /// Set when this category is a Canvas assignment group.
    pub canvas_group_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GradesInfo {
    pub categories: Vec<GradeCategory>,
    pub weight_total: f64,
    /// SPEC §11 math — None until at least one item is recorded.
    pub current_grade: Option<f64>,
}

pub fn list_grades(conn: &Connection, class_id: i64) -> Result<GradesInfo> {
    let mut cat_stmt = conn.prepare(
        "SELECT id, name, weight, canvas_group_id FROM grade_categories
         WHERE class_id = ?1 ORDER BY id",
    )?;
    let mut item_stmt = conn.prepare(
        "SELECT id, name, score, max_score, graded_at, canvas_assignment_id FROM grade_items
         WHERE category_id = ?1 ORDER BY id",
    )?;
    let heads = cat_stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut categories = Vec::with_capacity(heads.len());
    let mut weight_total = 0.0;
    let mut grade = GradeAccumulator::default();
    for (id, name, weight, canvas_group_id) in heads {
        let items = item_stmt
            .query_map([id], |row| {
                Ok(GradeItem {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    score: row.get(2)?,
                    max_score: row.get(3)?,
                    graded_at: row.get(4)?,
                    canvas_assignment_id: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<GradeItem>>>()?;
        let max_sum: f64 = items.iter().map(|i| i.max_score).sum();
        let score_sum: f64 = items.iter().map(|i| i.score).sum();
        let percent = (max_sum > 0.0).then(|| score_sum / max_sum * 100.0);
        // Derived from the rows already in hand rather than re-queried.
        grade.add(weight, score_sum, max_sum);
        weight_total += weight;
        categories.push(GradeCategory {
            id,
            name,
            weight,
            percent,
            items,
            canvas_group_id,
        });
    }
    Ok(GradesInfo {
        current_grade: grade.percent(),
        categories,
        weight_total,
    })
}

fn valid_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a name is required");
    }
    if name.chars().count() > MAX_NAME_CHARS {
        bail!("the name is too long — keep it under {MAX_NAME_CHARS} characters");
    }
    Ok(name)
}

/// Create (id None) or amend a category — same rules as the chat tool, which
/// addresses categories by name: weights are 0–100 and names stay unique
/// within the class (case-insensitive) so "Homework" always means one row.
pub fn save_category(
    app: &AppHandle,
    class_id: i64,
    id: Option<i64>,
    name: &str,
    weight: f64,
) -> Result<()> {
    let name = valid_name(name)?.to_string();
    if !(0.0..=100.0).contains(&weight) {
        bail!("weight is a percentage between 0 and 100");
    }
    with_conn(app, |conn| {
        let taken: Option<i64> = conn
            .query_row(
                "SELECT id FROM grade_categories
                 WHERE class_id = ?1 AND LOWER(name) = LOWER(?2) AND id != ?3",
                params![class_id, name, id.unwrap_or(-1)],
                |row| row.get(0),
            )
            .optional()?;
        if taken.is_some() {
            bail!("a category named '{name}' already exists in this class");
        }
        let tx = conn.unchecked_transaction()?;
        match id {
            None => {
                tx.execute(
                    "INSERT INTO grade_categories (class_id, name, weight) VALUES (?1, ?2, ?3)",
                    params![class_id, name, weight],
                )?;
                audit(
                    &tx,
                    "ui.save_grade_category",
                    json!({ "id": tx.last_insert_rowid(), "classId": class_id,
                            "name": name, "weight": weight, "created": true }),
                )?;
            }
            Some(id) => {
                let before = tx
                    .query_row(
                        "SELECT class_id, name, weight FROM grade_categories WHERE id = ?1",
                        [id],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, f64>(2)?,
                            ))
                        },
                    )
                    .optional()?
                    .with_context(|| format!("no grade category #{id}"))?;
                if before.0 != class_id {
                    bail!("grade category #{id} belongs to a different class");
                }
                tx.execute(
                    "UPDATE grade_categories SET name = ?1, weight = ?2 WHERE id = ?3",
                    params![name, weight, id],
                )?;
                audit(
                    &tx,
                    "ui.save_grade_category",
                    json!({ "id": id, "classId": class_id,
                            "before": { "name": before.1, "weight": before.2 },
                            "after": { "name": name, "weight": weight } }),
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "grades");
    Ok(())
}

/// Deletes a category and its items in one transaction; everything removed
/// rides the audit entry, so the deletion is recoverable — that stands in for
/// a confirmation prompt.
pub fn delete_category(app: &AppHandle, id: i64) -> Result<()> {
    with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let head = tx
            .query_row(
                "SELECT class_id, name, weight, canvas_group_id FROM grade_categories
                 WHERE id = ?1",
                [id],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?
            .with_context(|| format!("no grade category #{id}"))?;
        // The whole row rides along, Canvas ids included: the entry is what a
        // recovery would be rebuilt from.
        let items: Vec<serde_json::Value> = tx
            .prepare(
                "SELECT id, name, score, max_score, graded_at, canvas_assignment_id
                 FROM grade_items WHERE category_id = ?1 ORDER BY id",
            )?
            .query_map([id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "score": r.get::<_, f64>(2)?,
                    "maxScore": r.get::<_, f64>(3)?,
                    "gradedAt": r.get::<_, Option<String>>(4)?,
                    "canvasAssignmentId": r.get::<_, Option<String>>(5)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute("DELETE FROM grade_items WHERE category_id = ?1", [id])?;
        tx.execute("DELETE FROM grade_categories WHERE id = ?1", [id])?;
        audit(
            &tx,
            "ui.delete_grade_category",
            json!({ "id": id, "classId": head.0, "name": head.1,
                    "weight": head.2, "canvasGroupId": head.3, "items": items }),
        )?;
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "grades");
    Ok(())
}

/// Create (id None) or amend an item. Extra credit is legal (score may exceed
/// max); `graded_at` is chat's field and survives an edit untouched.
pub fn save_item(
    app: &AppHandle,
    category_id: i64,
    id: Option<i64>,
    name: &str,
    score: f64,
    max_score: f64,
) -> Result<()> {
    let name = valid_name(name)?.to_string();
    if max_score <= 0.0 {
        bail!("max score must be positive");
    }
    if score < 0.0 {
        bail!("score cannot be negative");
    }
    with_conn(app, |conn| {
        let category: Option<String> = conn
            .query_row(
                "SELECT name FROM grade_categories WHERE id = ?1",
                [category_id],
                |row| row.get(0),
            )
            .optional()?;
        let category = category.with_context(|| format!("no grade category #{category_id}"))?;
        let tx = conn.unchecked_transaction()?;
        match id {
            None => {
                tx.execute(
                    "INSERT INTO grade_items (category_id, name, score, max_score)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![category_id, name, score, max_score],
                )?;
                audit(
                    &tx,
                    "ui.save_grade_item",
                    json!({ "id": tx.last_insert_rowid(), "categoryId": category_id,
                            "category": category, "name": name, "score": score,
                            "maxScore": max_score, "created": true }),
                )?;
            }
            Some(id) => {
                let before = tx
                    .query_row(
                        "SELECT category_id, name, score, max_score FROM grade_items
                         WHERE id = ?1",
                        [id],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, f64>(2)?,
                                r.get::<_, f64>(3)?,
                            ))
                        },
                    )
                    .optional()?
                    .with_context(|| format!("no grade item #{id}"))?;
                if before.0 != category_id {
                    bail!("grade item #{id} belongs to a different category");
                }
                tx.execute(
                    "UPDATE grade_items SET name = ?1, score = ?2, max_score = ?3
                     WHERE id = ?4",
                    params![name, score, max_score, id],
                )?;
                audit(
                    &tx,
                    "ui.save_grade_item",
                    json!({ "id": id, "categoryId": category_id, "category": category,
                            "before": { "name": before.1, "score": before.2,
                                        "maxScore": before.3 },
                            "after": { "name": name, "score": score,
                                       "maxScore": max_score } }),
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "grades");
    Ok(())
}

pub fn delete_item(app: &AppHandle, id: i64) -> Result<()> {
    with_conn(app, |conn| {
        let tx = conn.unchecked_transaction()?;
        let row = tx
            .query_row(
                "SELECT category_id, name, score, max_score, graded_at, canvas_assignment_id
                 FROM grade_items WHERE id = ?1",
                [id],
                |r| {
                    Ok(json!({
                        "id": id,
                        "categoryId": r.get::<_, i64>(0)?,
                        "name": r.get::<_, String>(1)?,
                        "score": r.get::<_, f64>(2)?,
                        "maxScore": r.get::<_, f64>(3)?,
                        "gradedAt": r.get::<_, Option<String>>(4)?,
                        "canvasAssignmentId": r.get::<_, Option<String>>(5)?,
                    }))
                },
            )
            .optional()?
            .with_context(|| format!("no grade item #{id}"))?;
        tx.execute("DELETE FROM grade_items WHERE id = ?1", [id])?;
        audit(&tx, "ui.delete_grade_item", row)?;
        tx.commit()?;
        Ok(())
    })?;
    emit_hub_change(app, "grades");
    Ok(())
}

// ---------------------------------------------------------------------------
// What a Canvas sync writes (SPEC §7.2): categories from assignment groups and
// items from graded, posted submissions — direct, audited, keyed on Canvas ids
// so a re-sync is an update in place. Direct rather than proposed because a
// grade is reversible through the Grades section, which is the app's rule for
// skipping a confirm step.

/// One assignment group as the sync reads it.
pub(crate) struct CanvasGroup<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// Set only when the course applies its group weights; otherwise the
    /// category's weight is the reader's to type, and the ≠100% warning says
    /// when it is missing.
    pub weight: Option<f64>,
}

/// One graded, posted submission as the sync reads it.
pub(crate) struct CanvasScore<'a> {
    pub assignment_id: &'a str,
    pub name: &'a str,
    pub score: f64,
    pub max_score: f64,
    pub graded_at: Option<&'a str>,
}

/// What an upsert did, so the sync can count what changed and say nothing
/// about what did not.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum CanvasWrite {
    Created,
    /// A hand-made category with the same name now carries the Canvas id.
    Claimed,
    Updated,
    Unchanged,
}

/// A name a reader supplied — a Canvas group or assignment, a syllabus
/// component — in the column's own bounds. Truncated rather than refused: the
/// thing is real whatever its name's length, and a sync must not lose a score,
/// nor a scan a weight, over a long title.
fn read_name(name: &str) -> Result<String> {
    let name = crate::db::truncate(name.trim(), MAX_NAME_CHARS);
    if name.is_empty() {
        bail!("a Canvas name is empty");
    }
    Ok(name)
}

/// What upserting a category did: the row, how it landed, and a line for the
/// sync report when Canvas's name could not be taken as it was.
pub(crate) struct CategoryWrite {
    pub id: i64,
    pub write: CanvasWrite,
    pub note: Option<String>,
}

/// Whether another category in the class already carries `name`, ignoring
/// case — the rule `save_category` and the chat tool enforce, which a Canvas
/// write must not break: two rows sharing a name would leave both weights
/// uneditable from the Grades section.
fn name_taken(conn: &Connection, class_id: i64, name: &str, except: Option<i64>) -> Result<bool> {
    let taken: i64 = conn.query_row(
        "SELECT COUNT(*) FROM grade_categories
         WHERE class_id = ?1 AND LOWER(name) = LOWER(?2) AND id != ?3",
        params![class_id, name, except.unwrap_or(-1)],
        |row| row.get(0),
    )?;
    Ok(taken > 0)
}

/// Upserts a category for an assignment group.
///
/// Identity is the group id. A category with no id and the same name
/// (case-insensitive, the chat tool's rule) is claimed rather than duplicated,
/// which is what lets the three categories typed for Biostatistics before this
/// existed become Canvas's own without a second "Quizzes" beside them. A
/// weight arrives only when the course applies group weights; otherwise the
/// existing weight stands and a new category starts at zero.
///
/// Names stay unique within the class. A group renamed on Canvas onto a name
/// another category here holds keeps its current name, and a second group
/// arriving under a name already held is recorded with a numbered suffix;
/// both say so in the sync report rather than landing a row the Grades
/// section could not edit.
pub(crate) fn upsert_canvas_category(
    conn: &Connection,
    class_id: i64,
    group: &CanvasGroup,
) -> Result<CategoryWrite> {
    let name = read_name(group.name)?;
    let weight = group.weight.filter(|w| (0.0..=100.0).contains(w));
    let read = |row: &rusqlite::Row| -> rusqlite::Result<(i64, String, f64)> {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    };
    let owned: Option<(i64, String, f64)> = conn
        .query_row(
            "SELECT id, name, weight FROM grade_categories
             WHERE class_id = ?1 AND canvas_group_id = ?2",
            params![class_id, group.id],
            read,
        )
        .optional()?;
    if let Some((id, current_name, current_weight)) = owned {
        let weight = weight.unwrap_or(current_weight);
        let (name, note) = if current_name != name && name_taken(conn, class_id, &name, Some(id))? {
            let note = format!(
                "Canvas renamed \"{current_name}\" to \"{name}\", which another category here \
                 already uses — kept as \"{current_name}\""
            );
            (current_name.clone(), Some(note))
        } else {
            (name, None)
        };
        // Exact: a REAL round-trips an f64 bit for bit, and the question is
        // whether Canvas's number differs from the stored one at all.
        if current_name == name && current_weight == weight {
            return Ok(CategoryWrite { id, write: CanvasWrite::Unchanged, note });
        }
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "UPDATE grade_categories SET name = ?1, weight = ?2 WHERE id = ?3",
            params![name, weight, id],
        )?;
        audit(
            &tx,
            "canvas.upsert_grade_category",
            json!({ "id": id, "classId": class_id, "canvasGroupId": group.id,
                    "before": { "name": current_name, "weight": current_weight },
                    "after": { "name": name, "weight": weight } }),
        )?;
        tx.commit()?;
        return Ok(CategoryWrite { id, write: CanvasWrite::Updated, note });
    }

    let unclaimed: Option<(i64, String, f64)> = conn
        .query_row(
            "SELECT id, name, weight FROM grade_categories
             WHERE class_id = ?1 AND LOWER(name) = LOWER(?2) AND canvas_group_id IS NULL",
            params![class_id, name],
            read,
        )
        .optional()?;
    let tx = conn.unchecked_transaction()?;
    let outcome = match unclaimed {
        Some((id, current_name, current_weight)) => {
            let weight = weight.unwrap_or(current_weight);
            tx.execute(
                "UPDATE grade_categories SET canvas_group_id = ?1, name = ?2, weight = ?3
                 WHERE id = ?4",
                params![group.id, name, weight, id],
            )?;
            audit(
                &tx,
                "canvas.upsert_grade_category",
                json!({ "id": id, "classId": class_id, "canvasGroupId": group.id,
                        "claimed": true,
                        "before": { "name": current_name, "weight": current_weight },
                        "after": { "name": name, "weight": weight } }),
            )?;
            CategoryWrite { id, write: CanvasWrite::Claimed, note: None }
        }
        None => {
            // Nothing unclaimed carries the name, so a holder is another Canvas
            // group: two groups called "Quizzes" are two categories, and the
            // second takes a suffix so both stay editable.
            let mut landed = name.clone();
            let mut serial = 2;
            while name_taken(&tx, class_id, &landed, None)? {
                landed = format!("{name} ({serial})");
                serial += 1;
            }
            let note = (landed != name).then(|| {
                format!(
                    "Canvas has more than one group called \"{name}\" — this one is recorded \
                     as \"{landed}\""
                )
            });
            let weight = weight.unwrap_or(0.0);
            tx.execute(
                "INSERT INTO grade_categories (class_id, name, weight, canvas_group_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![class_id, landed, weight, group.id],
            )?;
            let id = tx.last_insert_rowid();
            audit(
                &tx,
                "canvas.upsert_grade_category",
                json!({ "id": id, "classId": class_id, "canvasGroupId": group.id,
                        "name": landed, "weight": weight, "created": true }),
            )?;
            CategoryWrite { id, write: CanvasWrite::Created, note }
        }
    };
    tx.commit()?;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// What a syllabus scan writes (SPEC §11): a category's weight, read out of the
// syllabus's grade breakdown. Direct and audited like a Canvas write, for the
// same reason — a weight is reversible in the Grades section.

/// What recording a syllabus weight did, so the scan's summary can say which
/// numbers it set and which it left alone.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum WeightWrite {
    /// A category the class did not track, created with this weight and no
    /// Canvas id.
    Created,
    /// A category whose weight was zero now carries the syllabus's.
    Set,
    /// The stored weight already agrees with the syllabus.
    Unchanged,
    /// The stored weight was already set and disagrees with the syllabus.
    /// Left alone, carrying the stored weight so the caller can name it: a
    /// rescan must never silently change a number that was typed.
    Kept(f64),
}

/// Records the weight the syllabus states for `name`, matched
/// case-insensitively — the rule `save_category` and the chat tool enforce.
/// Fills a weight still at zero, creates a category the class lacks, and never
/// replaces a weight already set. Writes an audit row
/// (`syllabus.set_grade_weight`, before and after) only when something
/// changed, so a rescan of an unchanged syllabus leaves none.
pub(crate) fn set_syllabus_weight(
    conn: &Connection,
    class_id: i64,
    name: &str,
    weight: f64,
) -> Result<WeightWrite> {
    let name = read_name(name)?;
    if !weight.is_finite() || !(0.0..=100.0).contains(&weight) {
        bail!("a weight is a percentage between 0 and 100");
    }
    let existing: Option<(i64, String, f64)> = conn
        .query_row(
            "SELECT id, name, weight FROM grade_categories
             WHERE class_id = ?1 AND LOWER(name) = LOWER(?2)",
            params![class_id, name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let tx = conn.unchecked_transaction()?;
    let write = match existing {
        Some((_, _, current)) if (current - weight).abs() < WEIGHT_EPSILON => {
            WeightWrite::Unchanged
        }
        Some((_, _, current)) if current != 0.0 => WeightWrite::Kept(current),
        Some((id, current_name, current)) => {
            tx.execute(
                "UPDATE grade_categories SET weight = ?1 WHERE id = ?2",
                params![weight, id],
            )?;
            audit(
                &tx,
                "syllabus.set_grade_weight",
                json!({ "id": id, "classId": class_id, "name": current_name,
                        "before": { "weight": current }, "after": { "weight": weight } }),
            )?;
            WeightWrite::Set
        }
        None => {
            tx.execute(
                "INSERT INTO grade_categories (class_id, name, weight) VALUES (?1, ?2, ?3)",
                params![class_id, name, weight],
            )?;
            audit(
                &tx,
                "syllabus.set_grade_weight",
                json!({ "id": tx.last_insert_rowid(), "classId": class_id,
                        "name": name, "weight": weight, "created": true }),
            )?;
            WeightWrite::Created
        }
    };
    tx.commit()?;
    Ok(write)
}

/// Upserts the item for a graded, posted submission under `category_id`, one
/// of `class_id`'s categories.
///
/// Identity is the assignment id, so a regrade updates the row and a hand
/// edit is overwritten with Canvas's number — the item says so in the UI. An
/// assignment moved between groups moves its item, since the category is
/// Canvas's placement too. An item carrying no id and the same name anywhere
/// in the class (case-insensitive, the category rule) is claimed rather than
/// duplicated: a score typed from the returned paper before the professor
/// posted it is the same score, and a second row would count it twice in the
/// weighted grade. The lookup stays inside the class — the index on the
/// assignment id is global, so a row another class holds for it is refused
/// rather than moved.
pub(crate) fn upsert_canvas_item(
    conn: &Connection,
    class_id: i64,
    category_id: i64,
    score: &CanvasScore,
) -> Result<CanvasWrite> {
    let name = read_name(score.name)?;
    if score.max_score <= 0.0 {
        bail!("{name} has no points possible");
    }
    if score.score < 0.0 {
        bail!("{name} has a negative score");
    }
    // id, class, category, name, score, max, graded_at
    type Row = (i64, i64, i64, String, f64, f64, Option<String>);
    let read = |row: &rusqlite::Row| -> rusqlite::Result<Row> {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ))
    };
    let owned: Option<Row> = conn
        .query_row(
            "SELECT i.id, c.class_id, i.category_id, i.name, i.score, i.max_score, i.graded_at
             FROM grade_items i JOIN grade_categories c ON c.id = i.category_id
             WHERE i.canvas_assignment_id = ?1",
            [score.assignment_id],
            read,
        )
        .optional()?;
    if let Some((_, holder, ..)) = owned.as_ref().filter(|row| row.1 != class_id) {
        bail!(
            "assignment {} is already recorded under class #{holder} — two classes matched one \
             Canvas course",
            score.assignment_id
        );
    }
    let claimed: Option<Row> = match owned {
        Some(_) => None,
        None => conn
            .query_row(
                "SELECT i.id, c.class_id, i.category_id, i.name, i.score, i.max_score, i.graded_at
                 FROM grade_items i JOIN grade_categories c ON c.id = i.category_id
                 WHERE c.class_id = ?1 AND LOWER(i.name) = LOWER(?2)
                   AND i.canvas_assignment_id IS NULL
                 ORDER BY (i.category_id = ?3) DESC, i.id LIMIT 1",
                params![class_id, name, category_id],
                read,
            )
            .optional()?,
    };
    // A claim always writes: the id is what changes. An owned row that already
    // says what Canvas says is left before any transaction opens — this is
    // every already-recorded grade on every sync. Exact comparison, as above.
    let claiming = owned.is_none();
    if let Some((_, _, category, current_name, current_score, current_max, current_graded)) =
        owned.as_ref()
    {
        if *category == category_id
            && *current_name == name
            && *current_score == score.score
            && *current_max == score.max_score
            && current_graded.as_deref() == score.graded_at
        {
            return Ok(CanvasWrite::Unchanged);
        }
    }
    let tx = conn.unchecked_transaction()?;
    let outcome = match (owned, claimed) {
        (Some(row), _) | (None, Some(row)) => {
            let (id, _, current_category, current_name, current_score, current_max, current_graded) =
                row;
            tx.execute(
                "UPDATE grade_items
                 SET canvas_assignment_id = ?1, category_id = ?2, name = ?3, score = ?4,
                     max_score = ?5, graded_at = ?6
                 WHERE id = ?7",
                params![
                    score.assignment_id,
                    category_id,
                    name,
                    score.score,
                    score.max_score,
                    score.graded_at,
                    id
                ],
            )?;
            audit(
                &tx,
                "canvas.upsert_grade_item",
                json!({ "id": id, "canvasAssignmentId": score.assignment_id,
                        "claimed": claiming,
                        "before": { "categoryId": current_category, "name": current_name,
                                    "score": current_score, "maxScore": current_max,
                                    "gradedAt": current_graded },
                        "after": { "categoryId": category_id, "name": name,
                                   "score": score.score, "maxScore": score.max_score,
                                   "gradedAt": score.graded_at } }),
            )?;
            if claiming {
                CanvasWrite::Claimed
            } else {
                CanvasWrite::Updated
            }
        }
        (None, None) => {
            tx.execute(
                "INSERT INTO grade_items
                 (category_id, name, score, max_score, graded_at, canvas_assignment_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    category_id,
                    name,
                    score.score,
                    score.max_score,
                    score.graded_at,
                    score.assignment_id
                ],
            )?;
            audit(
                &tx,
                "canvas.upsert_grade_item",
                json!({ "id": tx.last_insert_rowid(), "categoryId": category_id,
                        "canvasAssignmentId": score.assignment_id, "name": name,
                        "score": score.score, "maxScore": score.max_score,
                        "gradedAt": score.graded_at, "created": true }),
            )?;
            CanvasWrite::Created
        }
    };
    tx.commit()?;
    Ok(outcome)
}

// ---------------------------------------------------------------------------
// The math — one implementation for every surface (chat, UI, class card).

/// "Weights now: Homework 30% + Exams 40% = 70% — 30% unassigned."
pub(crate) fn weights_line(conn: &Connection, class_id: i64) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT name, weight FROM grade_categories WHERE class_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total: f64 = rows.iter().map(|(_, w)| w).sum();
    let listed = rows
        .iter()
        .map(|(name, weight)| format!("{name} {}%", trim_num(*weight)))
        .collect::<Vec<_>>()
        .join(" + ");
    Ok(format!(
        "Weights now: {listed} = {}%{}",
        trim_num(total),
        if (total - 100.0).abs() < WEIGHT_EPSILON {
            String::new()
        } else if total < 100.0 {
            format!(" — {}% unassigned", trim_num(100.0 - total))
        } else {
            format!(" — {}% over 100", trim_num(total - 100.0))
        }
    ))
}

/// SPEC §11: current weighted grade over graded items. Categories with no
/// items are excluded and the remaining weights renormalized.
pub(crate) fn weighted_grade(conn: &Connection, class_id: i64) -> Result<Option<f64>> {
    let mut stmt = conn.prepare(
        "SELECT c.weight, SUM(i.score), SUM(i.max_score)
         FROM grade_categories c JOIN grade_items i ON i.category_id = c.id
         WHERE c.class_id = ?1 GROUP BY c.id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, f64>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut grade = GradeAccumulator::default();
    for (weight, score, max) in rows {
        grade.add(weight, score, max);
    }
    Ok(grade.percent())
}

/// Grades summary for the detailed overview (the agent needs category names
/// and current state to record into the right place).
pub(crate) fn grades_line(conn: &Connection, class_id: i64) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT c.name, c.weight, COUNT(i.id), SUM(i.score), SUM(i.max_score)
         FROM grade_categories c LEFT JOIN grade_items i ON i.category_id = c.id
         WHERE c.class_id = ?1 GROUP BY c.id ORDER BY c.id",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, f64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<f64>>(3)?,
                row.get::<_, Option<f64>>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok("Grades: no categories yet\n".to_string());
    }
    let mut weight_total = 0.0;
    let parts = rows
        .iter()
        .map(|(name, weight, count, score, max)| {
            weight_total += weight;
            let detail = match (score, max) {
                (Some(s), Some(m)) if *m > 0.0 => {
                    format!("{count} item(s), {:.1}%", s / m * 100.0)
                }
                _ => "no items yet".to_string(),
            };
            format!("{name} {}% ({detail})", trim_num(*weight))
        })
        .collect::<Vec<_>>()
        .join("; ");
    let grade = match weighted_grade(conn, class_id)? {
        Some(pct) => format!("current weighted grade {pct:.1}%"),
        None => "nothing graded yet".to_string(),
    };
    Ok(format!(
        "Grades: {parts} · weights sum {}%{} · {grade}\n",
        trim_num(weight_total),
        if (weight_total - 100.0).abs() < 0.01 { "" } else { " (≠100!)" }
    ))
}

pub(crate) fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    /// The row and how it landed, for the tests that care about nothing else.
    fn place(conn: &Connection, class_id: i64, group: &CanvasGroup) -> Result<(i64, CanvasWrite)> {
        upsert_canvas_category(conn, class_id, group).map(|c| (c.id, c.write))
    }

    fn category(conn: &Connection, id: i64) -> (String, f64, Option<String>) {
        conn.query_row(
            "SELECT name, weight, canvas_group_id FROM grade_categories WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the category")
    }

    /// The categories typed for Biostatistics before Canvas could supply them
    /// become Canvas's own on the first sync — one row, not a second "Quizzes"
    /// beside the first — and a second sync writes nothing.
    #[test]
    fn a_hand_made_category_is_claimed_by_name_rather_than_duplicated() {
        let conn = crate::db::memory_db();
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight) VALUES
               (3, 'Quizzes', 20), (3, 'Project', 30), (1, 'Project', 40);",
        )
        .expect("fixture");

        let group = CanvasGroup { id: "901", name: "quizzes", weight: None };
        let (id, landed) = place(&conn, 3, &group).expect("claim");
        assert_eq!(landed, CanvasWrite::Claimed);
        // The typed weight stands when the course does not weight its groups;
        // the name takes Canvas's own spelling.
        assert_eq!(category(&conn, id), ("quizzes".to_string(), 20.0, Some("901".to_string())));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_categories WHERE class_id = 3"), 2);

        // Found by id the second time: nothing changes and no audit row lands.
        let audits = count(&conn, "SELECT COUNT(*) FROM audit_log");
        assert_eq!(
            place(&conn, 3, &group).expect("again"),
            (id, CanvasWrite::Unchanged)
        );
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM audit_log"), audits);

        // A course that applies its group weights replaces the typed one.
        let weighted = CanvasGroup { id: "901", name: "Quizzes", weight: Some(25.0) };
        assert_eq!(
            place(&conn, 3, &weighted).expect("reweight"),
            (id, CanvasWrite::Updated)
        );
        assert_eq!(category(&conn, id).1, 25.0);

        // A group nothing was typed for starts at zero, so the ≠100% warning
        // says the weight is missing rather than a number pretending otherwise.
        let fresh = CanvasGroup { id: "902", name: "Homework", weight: None };
        let (new_id, landed) = place(&conn, 3, &fresh).expect("create");
        assert_eq!(landed, CanvasWrite::Created);
        assert_eq!(category(&conn, new_id).1, 0.0);

        // Another class's category with the same name is not this class's.
        let elsewhere = CanvasGroup { id: "903", name: "Project", weight: None };
        let (other, landed) = place(&conn, 1, &elsewhere).expect("other class");
        assert_eq!(landed, CanvasWrite::Claimed);
        assert_eq!(category(&conn, other).1, 40.0, "claimed class 1's row, not class 3's");
        assert_eq!(category(&conn, 2).2, None, "class 3's Project is still unclaimed");
    }

    /// Identity is the assignment id, so a regrade — or a hand edit — is
    /// overwritten in place rather than recorded a second time.
    #[test]
    fn an_item_is_keyed_on_its_assignment_so_a_regrade_updates_in_place() {
        let conn = crate::db::memory_db();
        let group = CanvasGroup { id: "901", name: "Quizzes", weight: Some(20.0) };
        let (category_id, _) = place(&conn, 3, &group).expect("category");
        let score = CanvasScore {
            assignment_id: "5001",
            name: "Quiz 1",
            score: 9.0,
            max_score: 10.0,
            graded_at: Some("2026-09-03"),
        };
        assert_eq!(upsert_canvas_item(&conn, 3, category_id, &score).expect("first"), CanvasWrite::Created);

        let audits = count(&conn, "SELECT COUNT(*) FROM audit_log");
        assert_eq!(upsert_canvas_item(&conn, 3, category_id, &score).expect("second"), CanvasWrite::Unchanged);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM audit_log"), audits, "a no-op left a row");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_items"), 1);

        // A hand edit is a correction until the next sync, which is Canvas's.
        conn.execute("UPDATE grade_items SET score = 10 WHERE canvas_assignment_id = '5001'", [])
            .expect("hand edit");
        assert_eq!(upsert_canvas_item(&conn, 3, category_id, &score).expect("third"), CanvasWrite::Updated);
        let stored: f64 = conn
            .query_row("SELECT score FROM grade_items WHERE canvas_assignment_id = '5001'", [], |r| r.get(0))
            .expect("score");
        assert_eq!(stored, 9.0);
        let grade = weighted_grade(&conn, 3).expect("grade").expect("something graded");
        assert!((grade - 90.0).abs() < 1e-9, "{grade}");

        // What the CHECK would refuse is refused a step earlier, by name.
        let pointless = CanvasScore { max_score: 0.0, ..score };
        assert!(upsert_canvas_item(&conn, 3, category_id, &pointless).is_err());
    }

    /// The category is Canvas's placement too: an assignment moved between
    /// groups moves its item, in one write with one audit row.
    #[test]
    fn an_assignment_moved_between_groups_moves_its_item() {
        let conn = crate::db::memory_db();
        let (quizzes, _) =
            place(&conn, 3, &CanvasGroup { id: "901", name: "Quizzes", weight: None }).expect("category");
        let (homework, _) =
            place(&conn, 3, &CanvasGroup { id: "902", name: "Homework", weight: None }).expect("category");
        let score = CanvasScore {
            assignment_id: "5001",
            name: "Quiz 1",
            score: 9.0,
            max_score: 10.0,
            graded_at: None,
        };
        assert_eq!(upsert_canvas_item(&conn, 3, quizzes, &score).expect("first"), CanvasWrite::Created);
        let audits = count(&conn, "SELECT COUNT(*) FROM audit_log");
        assert_eq!(upsert_canvas_item(&conn, 3, homework, &score).expect("moved"), CanvasWrite::Updated);
        let (rows, category): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), MIN(category_id) FROM grade_items WHERE canvas_assignment_id = '5001'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("row");
        assert_eq!((rows, category), (1, homework));
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM audit_log"), audits + 1);
    }

    /// A score typed from the returned paper before the professor posted it is
    /// the same score: the sync claims that row rather than counting the quiz
    /// twice, and another class's row for the same assignment is refused.
    #[test]
    fn a_hand_entered_score_is_claimed_and_another_class_s_row_is_refused() {
        let conn = crate::db::memory_db();
        let (quizzes, _) = place(
            &conn,
            3,
            &CanvasGroup { id: "901", name: "Quizzes", weight: Some(20.0) },
        )
        .expect("category");
        let (homework, _) = place(
            &conn,
            3,
            &CanvasGroup { id: "902", name: "Homework", weight: Some(30.0) },
        )
        .expect("category");
        conn.execute(
            "INSERT INTO grade_items (category_id, name, score, max_score) VALUES (?1, 'quiz 2', 18, 20)",
            [quizzes],
        )
        .expect("typed by hand");

        let posted = CanvasScore {
            assignment_id: "5002",
            name: "Quiz 2",
            score: 17.0,
            max_score: 20.0,
            graded_at: Some("2026-09-24"),
        };
        assert_eq!(upsert_canvas_item(&conn, 3, quizzes, &posted).expect("claim"), CanvasWrite::Claimed);
        let (rows, name, score, id): (i64, String, f64, Option<String>) = conn
            .query_row(
                "SELECT COUNT(*), MIN(name), MIN(score), MIN(canvas_assignment_id)
                 FROM grade_items WHERE category_id = ?1",
                [quizzes],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("row");
        assert_eq!((rows, name.as_str(), score, id.as_deref()), (1, "Quiz 2", 17.0, Some("5002")));
        let grade = weighted_grade(&conn, 3).expect("grade").expect("something graded");
        assert!((grade - 85.0).abs() < 1e-9, "counted twice: {grade}");
        assert_eq!(
            upsert_canvas_item(&conn, 3, quizzes, &posted).expect("again"),
            CanvasWrite::Unchanged
        );

        // A hand item filed under another category is still the same score,
        // and moves to where Canvas keeps it.
        conn.execute(
            "INSERT INTO grade_items (category_id, name, score, max_score) VALUES (?1, 'Homework 1', 9, 10)",
            [quizzes],
        )
        .expect("misfiled by hand");
        let filed = CanvasScore {
            assignment_id: "5003",
            name: "Homework 1",
            score: 9.0,
            max_score: 10.0,
            graded_at: None,
        };
        assert_eq!(upsert_canvas_item(&conn, 3, homework, &filed).expect("claim"), CanvasWrite::Claimed);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_items WHERE name = 'Homework 1'"), 1);
        let moved: i64 = conn
            .query_row("SELECT category_id FROM grade_items WHERE canvas_assignment_id = '5003'", [], |r| r.get(0))
            .expect("category");
        assert_eq!(moved, homework);

        // Two classes matched to one Canvas course: the second is refused
        // rather than moving the first's row across.
        let (elsewhere, _) = place(
            &conn,
            1,
            &CanvasGroup { id: "801", name: "Quizzes", weight: None },
        )
        .expect("category");
        let err = upsert_canvas_item(&conn, 1, elsewhere, &posted).unwrap_err().to_string();
        assert!(err.contains("another class") || err.contains("class #3"), "{err}");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_items WHERE canvas_assignment_id = '5002'"), 1);
    }

    /// Names stay unique within the class whatever Canvas sends, because two
    /// rows sharing one would leave both weights uneditable from the Grades
    /// section. A second group under a held name takes a suffix; a rename
    /// onto a held name is kept back; both are said in the report.
    #[test]
    fn two_groups_with_one_name_stay_two_editable_categories() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO grade_categories (class_id, name, weight) VALUES (3, 'Homework', 40)",
            [],
        )
        .expect("typed by hand");
        let first = upsert_canvas_category(
            &conn,
            3,
            &CanvasGroup { id: "901", name: "Quizzes", weight: None },
        )
        .expect("first");
        assert!(first.note.is_none());
        let second = upsert_canvas_category(
            &conn,
            3,
            &CanvasGroup { id: "902", name: "quizzes", weight: None },
        )
        .expect("second");
        assert_eq!(second.write, CanvasWrite::Created);
        assert_eq!(category(&conn, second.id).0, "quizzes (2)");
        assert!(second.note.as_deref().unwrap_or("").contains("quizzes (2)"), "{:?}", second.note);
        // Found by id after, still its own row, nothing to say.
        let again = upsert_canvas_category(
            &conn,
            3,
            &CanvasGroup { id: "902", name: "quizzes", weight: None },
        )
        .expect("again");
        assert_eq!((again.id, again.write), (second.id, CanvasWrite::Unchanged));
        // Wait — Canvas says "quizzes", the row says "quizzes (2)": the name
        // differs, but the suffixed name is the row's own and a rename back
        // onto "quizzes" is refused the same way. Recorded as unchanged.
        assert!(again.note.is_some(), "the standing collision is still reported");

        // Group 901 renamed onto the hand-made category's name.
        let renamed = upsert_canvas_category(
            &conn,
            3,
            &CanvasGroup { id: "901", name: "Homework", weight: None },
        )
        .expect("rename");
        assert_eq!(renamed.write, CanvasWrite::Unchanged);
        assert_eq!(category(&conn, renamed.id).0, "Quizzes", "kept its name");
        assert!(renamed.note.as_deref().unwrap_or("").contains("kept as \"Quizzes\""), "{:?}", renamed.note);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_categories WHERE class_id = 3 AND LOWER(name) = 'homework'"), 1);
        // And the hand-made row is still claimable by its own group.
        let claimed = upsert_canvas_category(
            &conn,
            3,
            &CanvasGroup { id: "903", name: "Homework", weight: None },
        )
        .expect("claim");
        assert_eq!(claimed.write, CanvasWrite::Claimed);
    }

    /// A syllabus weight fills a zero and creates what is missing, and never
    /// touches a number already set — a rescan must not change a typed weight
    /// in silence. Audit rows land only for what changed.
    #[test]
    fn a_syllabus_weight_fills_a_zero_and_never_a_typed_one() {
        let conn = crate::db::memory_db();
        conn.execute_batch(
            "INSERT INTO grade_categories (class_id, name, weight, canvas_group_id) VALUES
               (3, 'Quizzes', 0, '901'), (3, 'Project', 30, NULL), (1, 'Quizzes', 0, '801');",
        )
        .expect("fixture");
        let audits = |conn: &Connection| -> Vec<String> {
            let mut stmt = conn
                .prepare("SELECT payload FROM audit_log WHERE action = 'syllabus.set_grade_weight' ORDER BY id")
                .expect("prepare");
            stmt.query_map([], |row| row.get(0))
                .expect("query")
                .collect::<rusqlite::Result<Vec<String>>>()
                .expect("rows")
        };

        // A zero takes the syllabus weight, matched across casing, the Canvas
        // id untouched, with the row's before and after in the audit.
        assert_eq!(set_syllabus_weight(&conn, 3, "quizzes", 20.0).expect("fill"), WeightWrite::Set);
        assert_eq!(category(&conn, 1), ("Quizzes".to_string(), 20.0, Some("901".to_string())));
        let rows = audits(&conn);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].contains(r#""before":{"weight":0.0}"#), "{}", rows[0]);
        assert!(rows[0].contains(r#""after":{"weight":20.0}"#), "{}", rows[0]);
        assert_eq!(category(&conn, 3).1, 0.0, "another class's Quizzes is not this class's");

        // The same weight again: nothing changes and no row lands.
        assert_eq!(set_syllabus_weight(&conn, 3, "Quizzes", 20.0).expect("again"), WeightWrite::Unchanged);
        // A typed weight the syllabus disagrees with is kept, and reported as such.
        assert_eq!(set_syllabus_weight(&conn, 3, "Project", 25.0).expect("kept"), WeightWrite::Kept(30.0));
        assert_eq!(category(&conn, 2).1, 30.0);
        // Agreement is within the UI's own epsilon.
        assert_eq!(set_syllabus_weight(&conn, 3, "Project", 30.004).expect("near"), WeightWrite::Unchanged);
        assert_eq!(audits(&conn).len(), 1);

        // A component the class did not track becomes a category with no Canvas id.
        assert_eq!(
            set_syllabus_weight(&conn, 3, "Peer Design Sessions", 20.0).expect("create"),
            WeightWrite::Created
        );
        assert_eq!(
            category(&conn, 4),
            ("Peer Design Sessions".to_string(), 20.0, None)
        );
        let rows = audits(&conn);
        assert_eq!(rows.len(), 2);
        assert!(rows[1].contains(r#""created":true"#), "{}", rows[1]);

        // The category rules hold at this door too.
        assert!(set_syllabus_weight(&conn, 3, "Quizzes", 120.0).is_err());
        assert!(set_syllabus_weight(&conn, 3, "Quizzes", f64::NAN).is_err());
        assert!(set_syllabus_weight(&conn, 3, "  ", 5.0).is_err());
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_categories WHERE class_id = 3"), 3);
    }
}
