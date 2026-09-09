//! SPEC §8.3 — a practice exam's self-score.
//!
//! The exam's self-scoring panel posts `{exam, results: [{question, topic,
//! correct}]}` to the frame's parent once every section is totalled. The
//! viewer hands it here for the exam it opened, and the rows replace the
//! exam's earlier ones. The topics it missed, with the class's cards
//! answered wrong (`cards.rs`), are the next exam's default focus.

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::db::{emit_hub_change, now, truncate, with_conn};

/// An exam has twenty questions or so; past this the message is not an
/// exam's.
const MAX_RESULTS: usize = 200;
const MAX_TEXT: usize = 120;
/// How many topics a default focus names from each source.
const MAX_FOCUS_TOPICS: usize = 12;

/// One answer as the panel posts it.
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
pub(crate) struct PostedAnswer {
    pub question: String,
    pub topic: String,
    pub correct: bool,
}

#[derive(Deserialize, Debug)]
struct Posted {
    #[serde(default)]
    exam: String,
    results: Vec<PostedAnswer>,
}

/// Reads the panel's message — `{exam, results: [{question, topic, correct}]}`,
/// every answer with a question and a topic — into the exam it names and its
/// answers. Model output, so the shape is checked and the strings capped;
/// anything else is refused by name rather than recorded as a score.
pub(crate) fn parse_posted(value: &serde_json::Value) -> Result<(String, Vec<PostedAnswer>)> {
    let posted: Posted = serde_json::from_value(value.clone())
        .context("not {exam, results: [{question, topic, correct}]}")?;
    if posted.results.is_empty() {
        bail!("the message carries no results");
    }
    if posted.results.len() > MAX_RESULTS {
        bail!(
            "the message carries {} results — past the cap of {MAX_RESULTS}",
            posted.results.len()
        );
    }
    let results: Vec<PostedAnswer> = posted
        .results
        .into_iter()
        .map(|a| PostedAnswer {
            question: truncate(a.question.trim(), MAX_TEXT),
            topic: truncate(a.topic.trim(), MAX_TEXT),
            correct: a.correct,
        })
        .collect();
    if results.iter().any(|a| a.question.is_empty() || a.topic.is_empty()) {
        bail!("an answer names no question or no topic");
    }
    Ok((posted.exam.trim().to_string(), results))
}

/// What an exam's row shows: correct of total, when, and the topics missed.
#[derive(Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ExamScore {
    pub correct: i64,
    pub total: i64,
    pub recorded_at: i64,
    pub weak_topics: Vec<String>,
}

/// Records a self-score for the exam at `scope` (`practice:<rel path>`),
/// replacing the exam's earlier rows. The scope the exam was written for —
/// a division, a folder, the semester — rides each row, read off the job
/// that wrote the file, so the next exam of that scope finds this score.
pub fn record_results(
    app: &AppHandle,
    class_id: i64,
    scope: &str,
    posted: serde_json::Value,
) -> Result<ExamScore> {
    if !crate::db::is_practice_scope(scope) {
        bail!("{scope} is not a practice exam");
    }
    let (exam, results) = parse_posted(&posted)?;
    let score = with_conn(app, |conn| {
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT id, rel_path FROM guides WHERE class_id = ?1 AND scope = ?2",
                params![class_id, scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (guide_id, rel_path) =
            row.context("that exam has no row — one written before rows existed keeps no score")?;
        // The message names the file it came from; a name that is not this
        // exam's is another frame's message.
        let file_name = rel_path.rsplit('/').next().unwrap_or(&rel_path);
        if !exam.is_empty() && exam != file_name {
            bail!("the message is for {exam}, not {file_name}");
        }
        let origin = origin_scope(conn, class_id, &rel_path)?;
        let at = now();
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM practice_results WHERE guide_id = ?1", [guide_id])?;
        for a in &results {
            tx.execute(
                "INSERT INTO practice_results (guide_id, scope, question, topic, correct, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![guide_id, origin, a.question, a.topic, i64::from(a.correct), at],
            )?;
        }
        tx.commit()?;
        Ok(score_of(&results, at))
    })?;
    emit_hub_change(app, "practice");
    Ok(score)
}

fn score_of(results: &[PostedAnswer], at: i64) -> ExamScore {
    let mut weak: Vec<String> = Vec::new();
    for a in results {
        if !a.correct && !weak.contains(&a.topic) {
            weak.push(a.topic.clone());
        }
    }
    ExamScore {
        correct: results.iter().filter(|a| a.correct).count() as i64,
        total: results.len() as i64,
        recorded_at: at,
        weak_topics: weak,
    }
}

/// The scope an exam was written for, off the practice job whose payload
/// names its file; none for a file no job wrote.
fn origin_scope(conn: &Connection, class_id: i64, rel_path: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT scope FROM jobs
             WHERE kind = 'practice' AND class_id = ?1
               AND json_valid(payload) AND json_extract(payload, '$.relPath') = ?2
             ORDER BY id DESC LIMIT 1",
            params![class_id, rel_path],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// An exam's last self-score, from its rows; none until it has been scored.
pub fn last_score(conn: &Connection, class_id: i64, scope: &str) -> Result<Option<ExamScore>> {
    let mut stmt = conn.prepare(
        "SELECT r.question, r.topic, r.correct, r.recorded_at
         FROM practice_results r JOIN guides g ON g.id = r.guide_id
         WHERE g.class_id = ?1 AND g.scope = ?2 ORDER BY r.id",
    )?;
    let rows = stmt
        .query_map(params![class_id, scope], |r| {
            Ok((
                PostedAnswer {
                    question: r.get(0)?,
                    topic: r.get(1)?,
                    correct: r.get::<_, i64>(2)? != 0,
                },
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let Some(at) = rows.iter().map(|(_, at)| *at).max() else {
        return Ok(None);
    };
    let answers: Vec<PostedAnswer> = rows.into_iter().map(|(a, _)| a).collect();
    Ok(Some(score_of(&answers, at)))
}

/// The topics missed in the last self-scored exam written for `origin` in
/// the class — the latest score, whichever exam file it was recorded on.
pub(crate) fn weak_topics_of_scope(conn: &Connection, class_id: i64, origin: &str) -> Result<Vec<String>> {
    let latest: Option<i64> = conn
        .query_row(
            "SELECT r.guide_id FROM practice_results r JOIN guides g ON g.id = r.guide_id
             WHERE g.class_id = ?1 AND r.scope = ?2
             ORDER BY r.recorded_at DESC, r.id DESC LIMIT 1",
            params![class_id, origin],
            |r| r.get(0),
        )
        .optional()?;
    let Some(guide_id) = latest else {
        return Ok(Vec::new());
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT topic FROM practice_results
         WHERE guide_id = ?1 AND correct = 0 ORDER BY id",
    )?;
    let topics = stmt
        .query_map([guide_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(topics)
}

/// The focus an exam of `scope` takes when none was typed (SPEC §8.3): the
/// topics its last self-score missed, then the topics of the class's cards
/// answered wrong; none when neither has anything, which leaves the prompt's
/// own "cover the whole scope evenly".
pub(crate) fn default_focus(conn: &Connection, class_id: i64, scope: &str) -> Result<Option<String>> {
    let missed = weak_topics_of_scope(conn, class_id, scope)?;
    let wrong = crate::cards::weak_topics(conn, class_id)?;
    Ok(focus_line(&missed, &wrong))
}

pub(crate) fn focus_line(missed: &[String], wrong: &[String]) -> Option<String> {
    let list = |topics: &[String]| {
        topics
            .iter()
            .take(MAX_FOCUS_TOPICS)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !missed.is_empty() {
        parts.push(format!(
            "missed in the last self-scored exam of this scope: {}",
            list(missed)
        ));
    }
    let wrong: Vec<String> = wrong.iter().filter(|t| !missed.contains(t)).cloned().collect();
    if !wrong.is_empty() {
        parts.push(format!("answered wrong on the cards: {}", list(&wrong)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::memory_db;
    use serde_json::json;

    /// The settled shape reads; a wrong shape — no results, an answer
    /// without a topic, a correct that is not a bool — is dropped by name.
    #[test]
    fn the_panels_message_is_read_only_in_its_settled_shape() {
        let (exam, answers) = parse_posted(&json!({
            "exam": "Week 3 — 2026-09-08.html",
            "results": [
                {"question": "Q01", "topic": "missing data mechanisms", "correct": true},
                {"question": "Q02", "topic": " MCAR recognition ", "correct": false}
            ]
        }))
        .expect("the settled shape");
        assert_eq!(exam, "Week 3 — 2026-09-08.html");
        assert_eq!(answers[1].topic, "MCAR recognition");
        assert!(!answers[1].correct);
        for wrong in [
            json!({"results": []}),
            json!({"exam": "x.html"}),
            json!({"results": [{"question": "Q01", "correct": true}]}),
            json!({"results": [{"question": "Q01", "topic": "", "correct": true}]}),
            json!({"results": [{"question": "Q01", "topic": "t", "correct": "yes"}]}),
            json!("done"),
            json!({"results": (0..201).map(|i| json!({"question": format!("Q{i}"), "topic": "t", "correct": true})).collect::<Vec<_>>()}),
        ] {
            assert!(parse_posted(&wrong).is_err(), "{wrong}");
        }
    }

    /// A score replaces the exam's earlier rows, carries the scope its job
    /// named, and the scope's weak topics are the latest score's alone.
    #[test]
    fn a_score_replaces_the_exams_rows_and_names_the_scope_it_was_written_for() {
        let conn = memory_db();
        for (rel, job_scope) in [
            ("Study Guides/Practice/Week 3 — 2026-09-08.html", "unit:8"),
            ("Study Guides/Practice/Week 3 — 2026-09-15.html", "unit:8"),
        ] {
            crate::guides::upsert_guide(&conn, 3, &format!("practice:{rel}"), rel, "[]").unwrap();
            conn.execute(
                "INSERT INTO jobs (kind, class_id, scope, status, created_at, owner_pid, payload)
                 VALUES ('practice', 3, ?1, 'succeeded', 1, 1, ?2)",
                params![job_scope, json!({ "relPath": rel }).to_string()],
            )
            .unwrap();
        }
        let first = "practice:Study Guides/Practice/Week 3 — 2026-09-08.html";
        let second = "practice:Study Guides/Practice/Week 3 — 2026-09-15.html";
        let guide_id = |scope: &str| -> i64 {
            conn.query_row("SELECT id FROM guides WHERE scope = ?1", [scope], |r| r.get(0)).unwrap()
        };
        let record = |scope: &str, answers: &[(&str, &str, bool)], at: i64| {
            let id = guide_id(scope);
            let rel: String = conn.query_row("SELECT rel_path FROM guides WHERE id = ?1", [id], |r| r.get(0)).unwrap();
            let origin = origin_scope(&conn, 3, &rel).unwrap();
            conn.execute("DELETE FROM practice_results WHERE guide_id = ?1", [id]).unwrap();
            for (q, t, c) in answers {
                conn.execute(
                    "INSERT INTO practice_results (guide_id, scope, question, topic, correct, recorded_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![id, origin, q, t, i64::from(*c), at],
                )
                .unwrap();
            }
        };
        record(first, &[("Q01", "missing data", false), ("Q02", "outliers", true), ("Q03", "missing data", false)], 100);
        let score = last_score(&conn, 3, first).unwrap().expect("scored");
        assert_eq!((score.correct, score.total, score.recorded_at), (1, 3, 100));
        assert_eq!(score.weak_topics, vec!["missing data".to_string()], "a topic once");
        assert_eq!(weak_topics_of_scope(&conn, 3, "unit:8").unwrap(), vec!["missing data".to_string()]);

        // Scored again: the earlier rows are gone.
        record(first, &[("Q01", "missing data", true), ("Q02", "outliers", false)], 200);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM practice_results", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        assert_eq!(weak_topics_of_scope(&conn, 3, "unit:8").unwrap(), vec!["outliers".to_string()]);

        // A later exam of the same scope: its score is the scope's now.
        record(second, &[("Q01", "EDA in R", false)], 300);
        assert_eq!(weak_topics_of_scope(&conn, 3, "unit:8").unwrap(), vec!["EDA in R".to_string()]);
        assert!(last_score(&conn, 3, "practice:nothing").unwrap().is_none());
        assert!(weak_topics_of_scope(&conn, 3, "unit:9").unwrap().is_empty());
        // A row's origin is what its job said, not the file's name.
        assert_eq!(origin_scope(&conn, 3, "Study Guides/Practice/Week 3 — 2026-09-15.html").unwrap().as_deref(), Some("unit:8"));
        assert_eq!(origin_scope(&conn, 3, "Study Guides/Practice/Module 1 — 2026-08-22.html").unwrap(), None);
    }

    /// The default focus names the missed topics, then the cards answered
    /// wrong, once each; nothing from either leaves the prompt its own line.
    #[test]
    fn the_default_focus_names_what_was_missed_then_the_cards_answered_wrong() {
        assert_eq!(focus_line(&[], &[]), None);
        assert_eq!(
            focus_line(&["missing data".into()], &[]).as_deref(),
            Some("missed in the last self-scored exam of this scope: missing data")
        );
        assert_eq!(
            focus_line(&["missing data".into()], &["missing data".into(), "quantiles".into()]).as_deref(),
            Some("missed in the last self-scored exam of this scope: missing data; answered wrong on the cards: quantiles")
        );
        assert_eq!(
            focus_line(&[], &["quantiles".into()]).as_deref(),
            Some("answered wrong on the cards: quantiles")
        );
    }
}
