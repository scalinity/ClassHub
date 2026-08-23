//! SPEC §11 — the weighted grade tracker. The math lives here (weighted over
//! categories that have graded items, renormalized) so the chat tools, the UI
//! commands, and the class card all compute the same number.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

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
        "SELECT id, name, weight FROM grade_categories
         WHERE class_id = ?1 ORDER BY id",
    )?;
    let mut item_stmt = conn.prepare(
        "SELECT id, name, score, max_score, graded_at FROM grade_items
         WHERE category_id = ?1 ORDER BY id",
    )?;
    let heads = cat_stmt
        .query_map([class_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut categories = Vec::with_capacity(heads.len());
    let mut weight_total = 0.0;
    for (id, name, weight) in heads {
        let items = item_stmt
            .query_map([id], |row| {
                Ok(GradeItem {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    score: row.get(2)?,
                    max_score: row.get(3)?,
                    graded_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<GradeItem>>>()?;
        let max_sum: f64 = items.iter().map(|i| i.max_score).sum();
        let percent = (max_sum > 0.0)
            .then(|| items.iter().map(|i| i.score).sum::<f64>() / max_sum * 100.0);
        weight_total += weight;
        categories.push(GradeCategory {
            id,
            name,
            weight,
            percent,
            items,
        });
    }
    Ok(GradesInfo {
        current_grade: weighted_grade(conn, class_id)?,
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
        let items: Vec<serde_json::Value> = tx
            .prepare(
                "SELECT id, name, score, max_score, graded_at FROM grade_items
                 WHERE category_id = ?1 ORDER BY id",
            )?
            .query_map([id], |r| {
                Ok(json!({
                    "id": r.get::<_, i64>(0)?,
                    "name": r.get::<_, String>(1)?,
                    "score": r.get::<_, f64>(2)?,
                    "maxScore": r.get::<_, f64>(3)?,
                    "gradedAt": r.get::<_, Option<String>>(4)?,
                }))
            })?
            .collect::<rusqlite::Result<_>>()?;
        tx.execute("DELETE FROM grade_items WHERE category_id = ?1", [id])?;
        tx.execute("DELETE FROM grade_categories WHERE id = ?1", [id])?;
        audit(
            &tx,
            "ui.delete_grade_category",
            json!({ "id": id, "classId": head.0, "name": head.1,
                    "weight": head.2, "items": items }),
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
                "SELECT category_id, name, score, max_score, graded_at
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
        if (total - 100.0).abs() < 0.01 {
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
    let mut weight_sum = 0.0;
    let mut acc = 0.0;
    for (weight, score, max) in rows {
        if max > 0.0 && weight > 0.0 {
            weight_sum += weight;
            acc += weight * (score / max);
        }
    }
    Ok((weight_sum > 0.0).then(|| acc / weight_sum * 100.0))
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
