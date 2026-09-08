//! SPEC §6 — one command reverses an audit row. The audit log already holds
//! the before-state of every note, deadline and grade edit and the source
//! and destination of every move, so the notice that follows a reversible
//! action can offer `Undo` in place of a confirmation: each row dispatches
//! on its `action` to an inverse in the module that wrote it, inside one
//! transaction with its own `undo.<action>` row naming the row it reversed.
//! A batch — a folder's files, a series' dates, a sync's placements — is the
//! same command over every row of the batch, newest first, and a row already
//! reversed or one no inverse exists for is refused by name while the rest
//! still run.

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use crate::db::{emit_hub_change, notify, with_conn};

/// What an undo did: the rows reversed, and one line per row it refused.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UndoOutcome {
    pub undone: Vec<i64>,
    pub refused: Vec<String>,
}

/// Reverses the rows, newest first, and pushes the hub changes each inverse
/// implies once at the end.
pub fn undo(app: &AppHandle, audit_ids: &[i64]) -> Result<UndoOutcome> {
    if audit_ids.is_empty() {
        bail!("nothing to undo");
    }
    let batch = with_conn(app, |conn| Ok(undo_in_conn(conn, audit_ids)))?;
    for area in &batch.areas {
        emit_hub_change(app, area);
    }
    if !batch.outcome.undone.is_empty() {
        let text = match batch.texts.as_slice() {
            [one] => one.clone(),
            many => format!("Undone: {} actions", many.len()),
        };
        notify(app, text, Vec::new(), batch.class_id);
    }
    Ok(batch.outcome)
}

/// What a batch did on the connection: the outcome for the caller, the hub
/// areas to push, each row's words and the class the notice belongs to.
struct Batch {
    outcome: UndoOutcome,
    areas: std::collections::BTreeSet<&'static str>,
    texts: Vec<String>,
    class_id: Option<i64>,
}

/// The batch on the connection, newest row first, each row once: the rows
/// already reversed are read once here, not once per row over the whole
/// log, and a row this batch reverses joins them so a repeated id is
/// refused the second time.
fn undo_in_conn(conn: &Connection, audit_ids: &[i64]) -> Batch {
    let mut ids = audit_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids.reverse();
    let mut batch = Batch {
        outcome: UndoOutcome::default(),
        areas: std::collections::BTreeSet::new(),
        texts: Vec::new(),
        class_id: None,
    };
    let mut undone = match already_undone(conn) {
        Ok(undone) => undone,
        Err(e) => {
            batch.outcome.refused.push(format!("the audit log could not be read: {e:#}"));
            return batch;
        }
    };
    for id in ids {
        match undo_with(conn, id, &undone) {
            Ok((reversed, touched)) => {
                undone.insert(id);
                batch.outcome.undone.push(id);
                batch.areas.extend(touched.iter().copied());
                batch.texts.push(reversed.what);
                batch.class_id = batch.class_id.or(reversed.class_id);
            }
            Err(e) => batch.outcome.refused.push(format!("#{id}: {e:#}")),
        }
    }
    batch
}

/// Every audit row an `undo.*` row names, in one read of the log.
fn already_undone(conn: &Connection) -> Result<std::collections::HashSet<i64>> {
    let mut stmt = conn.prepare(
        "SELECT json_extract(payload, '$.auditId') FROM audit_log
         WHERE action LIKE 'undo.%' AND json_valid(payload)",
    )?;
    let ids = stmt
        .query_map([], |r| r.get::<_, Option<i64>>(0))?
        .filter_map(|id| id.ok().flatten())
        .collect();
    Ok(ids)
}

/// The hub areas an action's inverse changes.
type Areas = &'static [&'static str];

/// One row, reading the reversed set itself — the tests' entry point.
#[cfg(test)]
fn undo_one(conn: &Connection, audit_id: i64) -> Result<(crate::deadlines::Undone, Areas)> {
    let undone = already_undone(conn)?;
    undo_with(conn, audit_id, &undone)
}

fn undo_with(
    conn: &Connection,
    audit_id: i64,
    undone: &std::collections::HashSet<i64>,
) -> Result<(crate::deadlines::Undone, Areas)> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT action, payload FROM audit_log WHERE id = ?1",
            [audit_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((action, payload)) = row else {
        bail!("no such audit row");
    };
    let payload: Value = serde_json::from_str(&payload).context("the row's payload is not JSON")?;
    if undone.contains(&audit_id) {
        bail!("{action} was already undone");
    }
    // A move writes its own transaction around the rename; every other
    // inverse is a row edit, wrapped here with its undo row.
    if matches!(action.as_str(), "sort.move" | "canvas.filed") {
        let undone = crate::sorter::undo_move(conn, &action, &payload, audit_id)?;
        return Ok((undone, &["proposals", "files"]));
    }
    let tx = conn.unchecked_transaction()?;
    let (undone, areas): (crate::deadlines::Undone, Areas) = match action.as_str() {
        "ui.upsert_deadline" | "chat.upsert_deadline" => {
            (crate::deadlines::undo_upsert(&tx, &payload)?, &["deadlines"])
        }
        "ui.delete_deadline" | "chat.delete_deadline" => {
            (crate::deadlines::undo_delete(&tx, &payload)?, &["deadlines"])
        }
        "ui.set_deadline_status" | "chat.complete_deadline" => {
            (crate::deadlines::undo_status(&tx, &payload)?, &["deadlines"])
        }
        "syllabus.insert_deadline" => (
            crate::deadlines::undo_insert(&tx, &payload)?,
            &["deadlines", "deadlineProposals"],
        ),
        "ui.write_note" | "chat.write_note" => (crate::notes::undo_write(&tx, &payload)?, &["notes"]),
        "ui.save_grade_category" | "chat.upsert_grade_category" => {
            (crate::grades::undo_save_category(&tx, &payload)?, &["grades"])
        }
        "ui.delete_grade_category" => {
            (crate::grades::undo_delete_category(&tx, &payload)?, &["grades"])
        }
        "ui.save_grade_item" | "chat.add_grade_item" => {
            (crate::grades::undo_save_item(&tx, &payload)?, &["grades"])
        }
        "ui.delete_grade_item" => (crate::grades::undo_delete_item(&tx, &payload)?, &["grades"]),
        other if other.starts_with("canvas.") => {
            bail!("a Canvas sync wrote this, and the next sync would write it again")
        }
        _ => bail!("{action} cannot be reversed"),
    };
    let mut record = json!({ "auditId": audit_id, "classId": undone.class_id });
    // A note's undo writes into the class folder, and the write guard reads
    // its path off this row as it reads the save's.
    if let (Some(into), Some(rel_path)) = (record.as_object_mut(), payload["relPath"].as_str()) {
        into.insert("relPath".to_string(), json!(rel_path));
    }
    crate::db::audit(&tx, &format!("undo.{action}"), record)?;
    tx.commit()?;
    Ok((undone, areas))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn db() -> Connection {
        crate::db::memory_db()
    }

    fn audit(conn: &Connection, action: &str, payload: Value) -> i64 {
        crate::db::audit(conn, action, payload).expect("audit")
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).expect("count")
    }

    /// The proposal row a move wrote, read off its audit payload.
    fn proposal_of(conn: &Connection, audit_id: i64) -> i64 {
        conn.query_row(
            "SELECT json_extract(payload, '$.proposalId') FROM audit_log WHERE id = ?1",
            [audit_id],
            |r| r.get(0),
        )
        .expect("proposal id")
    }

    /// A created deadline is removed, an edited one takes its earlier state
    /// back, a deleted one comes back under its id, and a status flip flips
    /// back — each writing its `undo.` row, and none twice.
    #[test]
    fn deadline_rows_reverse_and_only_once() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (7, 1, 'Homework 1', 'assignment', '2026-09-07', 'open', 'manual')",
            [],
        )
        .unwrap();
        let created = audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 7, "classId": 1, "title": "Homework 1", "created": true }),
        );
        let (undone, _) = undo_one(&conn, created).expect("undo the create");
        assert_eq!(undone.what, "Removed Homework 1");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM deadlines WHERE id = 7"), 0);
        let again = undo_one(&conn, created).expect_err("a second undo is refused");
        assert!(again.to_string().contains("already undone"), "{again:#}");

        let deleted = audit(
            &conn,
            "ui.delete_deadline",
            json!({ "id": 7, "classId": 1, "title": "Homework 1", "kind": "assignment",
                    "dueAt": "2026-09-07", "notes": null, "status": "open",
                    "source": "manual", "canvasAssignmentId": "77" }),
        );
        undo_one(&conn, deleted).expect("undo the delete");
        let (title, canvas): (String, Option<String>) = conn
            .query_row(
                "SELECT title, canvas_assignment_id FROM deadlines WHERE id = 7",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!((title.as_str(), canvas.as_deref()), ("Homework 1", Some("77")));

        conn.execute("UPDATE deadlines SET title = 'Homework 1b', due_at = '2026-09-08' WHERE id = 7", [])
            .unwrap();
        let edited = audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 7, "classId": 1,
                    "before": { "title": "Homework 1", "kind": "assignment",
                                "dueAt": "2026-09-07", "notes": null },
                    "after": { "title": "Homework 1b", "kind": "assignment",
                               "dueAt": "2026-09-08", "notes": null } }),
        );
        undo_one(&conn, edited).expect("undo the edit");
        let (title, due): (String, String) = conn
            .query_row("SELECT title, due_at FROM deadlines WHERE id = 7", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((title.as_str(), due.as_str()), ("Homework 1", "2026-09-07"));

        conn.execute("UPDATE deadlines SET status = 'done' WHERE id = 7", []).unwrap();
        let done = audit(
            &conn,
            "ui.set_deadline_status",
            json!({ "id": 7, "classId": 1, "status": "done", "before": "open" }),
        );
        undo_one(&conn, done).expect("undo the completion");
        let status: String =
            conn.query_row("SELECT status FROM deadlines WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "open");
        // A pre-M33 status row carries no `before`: the other status it is.
        conn.execute("UPDATE deadlines SET status = 'done' WHERE id = 7", []).unwrap();
        let old_shape = audit(&conn, "chat.complete_deadline", json!({ "id": 7 }));
        undo_one(&conn, old_shape).expect("undo the old-shape completion");
        let status: String =
            conn.query_row("SELECT status FROM deadlines WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(status, "open");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM audit_log WHERE action LIKE 'undo.%'"), 5);
    }

    /// An approved syllabus card's deadline goes and the card returns; a
    /// Canvas row is refused, as is one no inverse exists for.
    #[test]
    fn a_syllabus_insert_returns_its_card_and_a_canvas_row_is_refused() {
        let conn = db();
        conn.execute(
            "INSERT INTO deadline_proposals
             (id, class_id, title, kind, due_at, status, source, created_at, resolved_at)
             VALUES (3, 1, 'Live coding session 09/08', 'assignment', '2026-09-08',
                     'approved', 'syllabus', 1, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (9, 1, 'Live coding session 09/08', 'assignment', '2026-09-08', 'open',
                     'syllabus')",
            [],
        )
        .unwrap();
        let inserted = audit(
            &conn,
            "syllabus.insert_deadline",
            json!({ "proposalId": 3, "deadlineId": 9, "classId": 1,
                    "title": "Live coding session 09/08" }),
        );
        undo_one(&conn, inserted).expect("undo the insert");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM deadlines WHERE id = 9"), 0);
        let status: String = conn
            .query_row("SELECT status FROM deadline_proposals WHERE id = 3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "pending");

        let canvas = audit(&conn, "canvas.insert_deadline", json!({ "deadlineId": 9, "classId": 1 }));
        let refused = undo_one(&conn, canvas).expect_err("a Canvas row is refused");
        assert!(refused.to_string().contains("next sync"), "{refused:#}");
        let added = audit(&conn, "lecture.added", json!({ "classId": 1, "relPath": "x" }));
        let refused = undo_one(&conn, added).expect_err("no inverse");
        assert!(refused.to_string().contains("cannot be reversed"), "{refused:#}");
    }

    /// A category and its scores come back from their delete row; a created
    /// category holding scores is refused rather than emptied; a chat
    /// category row from before it carried its earlier state is refused.
    #[test]
    fn grade_rows_reverse_with_their_refusals() {
        let conn = db();
        conn.execute(
            "INSERT INTO grade_categories (id, class_id, name, weight) VALUES (4, 1, 'Quizzes', 20)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_items (id, category_id, name, score, max_score)
             VALUES (5, 4, 'Quiz 1', 9, 10)",
            [],
        )
        .unwrap();
        let created = audit(
            &conn,
            "ui.save_grade_category",
            json!({ "id": 4, "classId": 1, "name": "Quizzes", "weight": 20, "created": true }),
        );
        let refused = undo_one(&conn, created).expect_err("holds a score");
        assert!(refused.to_string().contains("holds 1 score"), "{refused:#}");

        let item = audit(
            &conn,
            "ui.save_grade_item",
            json!({ "id": 5, "categoryId": 4, "classId": 1, "category": "Quizzes",
                    "name": "Quiz 1", "score": 9, "maxScore": 10, "created": true }),
        );
        undo_one(&conn, item).expect("undo the score");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_items"), 0);
        undo_one(&conn, created).expect("empty now, so the category goes");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_categories"), 0);

        let deleted = audit(
            &conn,
            "ui.delete_grade_category",
            json!({ "id": 4, "classId": 1, "name": "Quizzes", "weight": 20,
                    "canvasGroupId": "g1",
                    "items": [{ "id": 5, "name": "Quiz 1", "score": 9, "maxScore": 10,
                                "gradedAt": null, "canvasAssignmentId": null }] }),
        );
        undo_one(&conn, deleted).expect("undo the delete");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM grade_items WHERE category_id = 4"), 1);
        let group: Option<String> = conn
            .query_row("SELECT canvas_group_id FROM grade_categories WHERE id = 4", [], |r| r.get(0))
            .unwrap();
        assert_eq!(group.as_deref(), Some("g1"));

        let old_chat = audit(
            &conn,
            "chat.upsert_grade_category",
            json!({ "id": 4, "classId": 1, "name": "Quizzes", "weight": 25, "previousWeight": 20 }),
        );
        let refused = undo_one(&conn, old_chat).expect_err("no earlier state on the row");
        assert!(refused.to_string().contains("no earlier state"), "{refused:#}");
    }

    /// A note save is undone only while the note still holds what it wrote:
    /// a later edit, or a row from before the hash, is refused.
    #[test]
    fn a_note_save_is_reversed_only_over_its_own_text() {
        let conn = db();
        let root = std::env::temp_dir().join(format!("classhub-undo-{}", std::process::id()));
        let notes = root.join("Biostatistics for AI").join("Notes");
        std::fs::create_dir_all(&notes).unwrap();
        crate::db::set_setting(&conn, "aibhs_root", root.to_str().unwrap()).unwrap();
        let written = crate::notes::write_note(&conn, 3, "Recap", "second\n", "ui.write_note")
            .expect("write");
        assert!(written.created);
        let (undone, _) = undo_one(&conn, written.audit_id).expect("undo the create");
        assert_eq!(undone.what, "Removed Recap");
        assert!(!notes.join("Recap.md").exists());

        std::fs::write(notes.join("Recap.md"), "first\n").unwrap();
        let written = crate::notes::write_note(&conn, 3, "Recap", "second\n", "chat.write_note")
            .expect("overwrite");
        std::fs::write(notes.join("Recap.md"), "third\n").unwrap();
        let refused = undo_one(&conn, written.audit_id).expect_err("edited since");
        assert!(refused.to_string().contains("edited since"), "{refused:#}");
        std::fs::write(notes.join("Recap.md"), "second\n").unwrap();
        undo_one(&conn, written.audit_id).expect("undo the overwrite");
        assert_eq!(std::fs::read_to_string(notes.join("Recap.md")).unwrap(), "first\n");

        let old = audit(
            &conn,
            "ui.write_note",
            json!({ "classId": 3, "relPath": "Notes/Recap.md", "created": false,
                    "previousContent": "zero\n" }),
        );
        let refused = undo_one(&conn, old).expect_err("no hash on the row");
        assert!(refused.to_string().contains("before undo existed"), "{refused:#}");
        std::fs::remove_dir_all(&root).ok();
    }

    /// A move goes back with its index row; a source path taken since is a
    /// refusal, never an overwrite; a by-name row is dismissed and a Canvas
    /// placement's card returns to pending.
    #[test]
    fn a_move_returns_and_a_taken_source_is_refused() {
        let conn = db();
        let root = std::env::temp_dir().join(format!("classhub-undo-move-{}", std::process::id()));
        let class_dir = root.join("Biostatistics for AI");
        std::fs::create_dir_all(class_dir.join("Slides")).unwrap();
        std::fs::create_dir_all(class_dir.join("_Inbox")).unwrap();
        crate::db::set_setting(&conn, "aibhs_root", root.to_str().unwrap()).unwrap();
        std::fs::write(class_dir.join("Slides/Week3 deck.csv"), "a,b\n").unwrap();
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (3, 'Slides/Week3 deck.csv', 'h', 4, 1, 'csv')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, source, starts_on)
             VALUES (3, 3, 'week', 'Week 3 — Data', 3, 'syllabus', '2026-09-03')",
            [],
        )
        .unwrap();
        let audit_id = crate::sorter::move_file(
            &conn,
            3,
            &class_dir,
            "Slides/Week3 deck.csv",
            "Weeks/Week 03 — Data/Week3 deck.csv",
            crate::sorter::Recorded {
                proposal: crate::sorter::ProposalRow::New { source: "by_name", reasoning: "test" },
                proposed_by: "by_name",
                confidence: None,
                action: "sort.move",
                extra: json!({}),
            },
        )
        .expect("move");
        let proposal_id = proposal_of(&conn, audit_id);
        assert!(class_dir.join("Weeks/Week 03 — Data/Week3 deck.csv").is_file());

        // The source taken since: refused by name, the file stays.
        std::fs::write(class_dir.join("Slides/Week3 deck.csv"), "other\n").unwrap();
        let refused = undo_one(&conn, audit_id).expect_err("taken");
        assert!(refused.to_string().contains("is taken"), "{refused:#}");
        assert!(class_dir.join("Weeks/Week 03 — Data/Week3 deck.csv").is_file());
        std::fs::remove_file(class_dir.join("Slides/Week3 deck.csv")).unwrap();

        let (undone, _) = undo_one(&conn, audit_id).expect("undo the move");
        assert_eq!(undone.what, "Returned Week3 deck.csv to Slides/");
        assert!(class_dir.join("Slides/Week3 deck.csv").is_file());
        let rel: String = conn
            .query_row("SELECT rel_path FROM files WHERE class_id = 3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rel, "Slides/Week3 deck.csv");
        let status: String = conn
            .query_row("SELECT status FROM move_proposals WHERE id = ?1", [proposal_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "dismissed", "a by-name row is dismissed so the row offers again");

        // A Canvas placement returns to the inbox, unindexed, with its card pending.
        std::fs::write(class_dir.join("_Inbox/Quiz.csv"), "q\n").unwrap();
        let audit_id = crate::sorter::move_file(
            &conn,
            3,
            &class_dir,
            "_Inbox/Quiz.csv",
            "Quizzes/Quiz.csv",
            crate::sorter::Recorded {
                proposal: crate::sorter::ProposalRow::New { source: "canvas", reasoning: "test" },
                proposed_by: "canvas",
                confidence: None,
                action: "canvas.filed",
                extra: json!({ "batch": "b" }),
            },
        )
        .expect("file");
        let proposal_id = proposal_of(&conn, audit_id);
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE rel_path = 'Quizzes/Quiz.csv'"), 1);
        undo_one(&conn, audit_id).expect("undo the placement");
        assert!(class_dir.join("_Inbox/Quiz.csv").is_file());
        assert!(!class_dir.join("Quizzes").exists(), "the folder the move created goes once empty");
        assert!(class_dir.join("Slides").is_dir(), "a folder that was already there stays");
        assert_eq!(count(&conn, "SELECT COUNT(*) FROM files WHERE rel_path LIKE '%Quiz.csv'"), 0);
        let status: String = conn
            .query_row("SELECT status FROM move_proposals WHERE id = ?1", [proposal_id], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "pending");
        let undo_rows: Vec<String> = conn
            .prepare("SELECT action FROM audit_log WHERE action LIKE 'undo.%' ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(undo_rows, ["undo.sort.move", "undo.canvas.filed"]);
        let _ = conn.execute("DELETE FROM files", params![]);
        std::fs::remove_dir_all(&root).ok();
    }
}

#[cfg(test)]
mod reissued_id_tests {
    use super::*;

    /// A rowid the deleted row freed and a later row took: the undo names a
    /// row that is not the one it wrote, and refuses rather than deleting it.
    #[test]
    fn an_undo_refuses_a_row_the_id_now_names_something_else() {
        let conn = db_for(|| ());
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (7, 1, 'Live coding session 09/08', 'assignment', '2026-09-08', 'open', 'syllabus')",
            [],
        )
        .unwrap();
        let inserted = crate::db::audit(
            &conn,
            "syllabus.insert_deadline",
            json!({ "proposalId": 3, "deadlineId": 7, "classId": 1,
                    "title": "Live coding session 09/08" }),
        )
        .unwrap();
        let created = crate::db::audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 7, "classId": 1, "title": "Live coding session 09/08", "created": true }),
        )
        .unwrap();
        conn.execute("DELETE FROM deadlines WHERE id = 7", []).unwrap();
        // The rowid a delete freed, reissued: modelled with the id stated.
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (7, 1, 'Final project', 'project', '2026-12-02', 'open', 'manual')",
            [],
        )
        .unwrap();
        for row in [inserted, created] {
            let refused = undo_one(&conn, row).expect_err("a different row now");
            assert!(refused.to_string().contains("different row now"), "{refused:#}");
        }
        let title: String =
            conn.query_row("SELECT title FROM deadlines WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(title, "Final project");

        conn.execute(
            "INSERT INTO grade_categories (id, class_id, name, weight) VALUES (4, 1, 'Quizzes', 20)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_items (id, category_id, name, score, max_score) VALUES (5, 4, 'Quiz 1', 9, 10)",
            [],
        )
        .unwrap();
        let category = crate::db::audit(
            &conn,
            "ui.save_grade_category",
            json!({ "id": 4, "classId": 1, "name": "Quizzes", "weight": 20, "created": true }),
        )
        .unwrap();
        let item = crate::db::audit(
            &conn,
            "ui.save_grade_item",
            json!({ "id": 5, "categoryId": 4, "classId": 1, "category": "Quizzes",
                    "name": "Quiz 1", "score": 9, "maxScore": 10, "created": true }),
        )
        .unwrap();
        conn.execute("DELETE FROM grade_items WHERE id = 5", []).unwrap();
        conn.execute("DELETE FROM grade_categories WHERE id = 4", []).unwrap();
        conn.execute(
            "INSERT INTO grade_categories (id, class_id, name, weight) VALUES (4, 1, 'Exams', 40)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO grade_items (id, category_id, name, score, max_score) VALUES (5, 4, 'Midterm', 80, 100)",
            [],
        )
        .unwrap();
        for row in [category, item] {
            let refused = undo_one(&conn, row).expect_err("a different row now");
            assert!(refused.to_string().contains("different row now"), "{refused:#}");
        }
        let kept: i64 = conn
            .query_row("SELECT COUNT(*) FROM grade_items WHERE name = 'Midterm'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1);
    }

    fn db_for(_: impl Fn()) -> Connection {
        crate::db::memory_db()
    }
}

#[cfg(test)]
mod edited_since_tests {
    use super::*;

    /// An edit's undo restores `before` only while the row still holds its
    /// `after`: a later edit, or a status changed since, is refused by name.
    #[test]
    fn an_edit_undo_refuses_a_row_edited_since() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
             VALUES (7, 1, 'Homework 1', 'assignment', '2026-09-08', 'open', 'manual')",
            [],
        )
        .unwrap();
        let first = crate::db::audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 7, "classId": 1,
                    "before": { "title": "Homework 1", "kind": "assignment", "dueAt": "2026-09-07", "notes": null },
                    "after": { "title": "Homework 1", "kind": "assignment", "dueAt": "2026-09-08", "notes": null } }),
        )
        .unwrap();
        conn.execute("UPDATE deadlines SET due_at = '2026-09-09' WHERE id = 7", []).unwrap();
        let refused = undo_one(&conn, first).expect_err("edited since");
        assert!(refused.to_string().contains("edited since"), "{refused:#}");
        conn.execute("UPDATE deadlines SET due_at = '2026-09-08' WHERE id = 7", []).unwrap();
        undo_one(&conn, first).expect("holds the change's state again");
        let due: String = conn.query_row("SELECT due_at FROM deadlines WHERE id = 7", [], |r| r.get(0)).unwrap();
        assert_eq!(due, "2026-09-07");

        conn.execute("UPDATE deadlines SET status = 'done' WHERE id = 7", []).unwrap();
        let done = crate::db::audit(
            &conn,
            "ui.set_deadline_status",
            json!({ "id": 7, "classId": 1, "status": "done", "before": "open" }),
        )
        .unwrap();
        conn.execute("UPDATE deadlines SET status = 'open' WHERE id = 7", []).unwrap();
        let refused = undo_one(&conn, done).expect_err("reopened since");
        assert!(refused.to_string().contains("changed since"), "{refused:#}");

        conn.execute(
            "INSERT INTO grade_categories (id, class_id, name, weight) VALUES (4, 1, 'Quizzes', 25)",
            [],
        )
        .unwrap();
        let reweighted = crate::db::audit(
            &conn,
            "ui.save_grade_category",
            json!({ "id": 4, "classId": 1,
                    "before": { "name": "Quizzes", "weight": 20 },
                    "after": { "name": "Quizzes", "weight": 25 } }),
        )
        .unwrap();
        conn.execute("UPDATE grade_categories SET weight = 30 WHERE id = 4", []).unwrap();
        let refused = undo_one(&conn, reweighted).expect_err("edited since");
        assert!(refused.to_string().contains("edited since"), "{refused:#}");
        conn.execute(
            "INSERT INTO grade_items (id, category_id, name, score, max_score) VALUES (5, 4, 'Quiz 1', 8, 10)",
            [],
        )
        .unwrap();
        let rescored = crate::db::audit(
            &conn,
            "ui.save_grade_item",
            json!({ "id": 5, "categoryId": 4, "classId": 1, "category": "Quizzes",
                    "before": { "name": "Quiz 1", "score": 9, "maxScore": 10 },
                    "after": { "name": "Quiz 1", "score": 8, "maxScore": 10 } }),
        )
        .unwrap();
        conn.execute("UPDATE grade_items SET score = 7 WHERE id = 5", []).unwrap();
        let refused = undo_one(&conn, rescored).expect_err("edited since");
        assert!(refused.to_string().contains("edited since"), "{refused:#}");
        conn.execute("UPDATE grade_items SET score = 8 WHERE id = 5", []).unwrap();
        undo_one(&conn, rescored).expect("holds the change's state again");
        let score: f64 = conn.query_row("SELECT score FROM grade_items WHERE id = 5", [], |r| r.get(0)).unwrap();
        assert_eq!(score, 9.0);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    /// A batch reverses newest first, refuses a row without an inverse
    /// while the rest go, names each reversal, and pushes the areas the
    /// inverses touched.
    #[test]
    fn a_batch_reverses_what_it_can_newest_first() {
        let conn = crate::db::memory_db();
        for (id, title) in [(7, "Homework 1"), (8, "Homework 2")] {
            conn.execute(
                "INSERT INTO deadlines (id, class_id, title, kind, due_at, status, source)
                 VALUES (?1, 1, ?2, 'assignment', '2026-09-08', 'open', 'manual')",
                rusqlite::params![id, title],
            )
            .unwrap();
        }
        let first = crate::db::audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 7, "classId": 1, "title": "Homework 1", "created": true }),
        )
        .unwrap();
        let no_inverse = crate::db::audit(&conn, "lecture.added", json!({ "classId": 1, "relPath": "x" })).unwrap();
        let second = crate::db::audit(
            &conn,
            "ui.upsert_deadline",
            json!({ "id": 8, "classId": 1, "title": "Homework 2", "created": true }),
        )
        .unwrap();
        let batch = undo_in_conn(&conn, &[first, no_inverse, second, second]);
        assert_eq!(batch.outcome.undone, [second, first], "newest first, each once");
        assert_eq!(batch.outcome.refused.len(), 1, "{:?}", batch.outcome.refused);
        assert!(batch.outcome.refused[0].contains("cannot be reversed"));
        assert_eq!(batch.texts, ["Removed Homework 2", "Removed Homework 1"]);
        assert_eq!(batch.class_id, Some(1));
        assert!(batch.areas.contains("deadlines"));
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM deadlines", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        // The same batch again: every row already reversed.
        let again = undo_in_conn(&conn, &[first, second]);
        assert!(again.outcome.undone.is_empty());
        assert!(again.outcome.refused.iter().all(|r| r.contains("already undone")), "{:?}", again.outcome.refused);
    }
}
