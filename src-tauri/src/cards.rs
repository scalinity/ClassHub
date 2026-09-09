//! SPEC §12 — the cards.
//!
//! Every guide and digest writes a cards sidecar beside itself (§8.1, §8.4)
//! and nothing read them before this. The index walks the guide rows' files
//! under `.classhub/cards/` and the corpus notes' sidecars on read, upserting
//! on (class, scope, front) so a card keeps its box and due date across a
//! rewrite, and dropping the cards of a sidecar that is gone; a read that
//! finds every sidecar as it was last time writes nothing. `Export for Anki`
//! writes a tab-separated file Anki imports and deduplicates on the front.
//! The dashboard serves the ten due soonest across the classes, three boxes
//! deep: right moves a card up a box — back in one, three, then seven days —
//! and wrong sends it to box one for tomorrow, its topic counting among the
//! class's weak topics until it is answered right. Nothing else schedules
//! anything.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use anyhow::{bail, Context, Result};
use chrono::{Days, NaiveDate};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;
use tauri::AppHandle;

use crate::db::{emit_hub_change, lock, now, truncate, with_conn};

/// Where the Anki export lands, under `Study Guides/` like every artifact.
pub const CARDS_OUTPUT_DIR: &str = "Study Guides/Cards";
const BOXES: i64 = 3;
/// Days until a card comes back after a right answer, by the box it was in.
const INTERVALS: [u64; 3] = [1, 3, 7];
/// A side longer than this is a paragraph of the guide, which the guide holds.
const MAX_SIDE: usize = 2000;
/// How many wrong-answered topics a focus names.
const MAX_WEAK_TOPICS: usize = 12;

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CardInfo {
    pub id: i64,
    pub class_id: i64,
    pub class_name: String,
    pub class_color: String,
    pub scope: String,
    /// What the scope is called on screen — the guide's or the session's name.
    pub scope_label: String,
    pub front: String,
    pub back: String,
    pub source: Option<String>,
    pub topic: Option<String>,
    #[serde(rename = "box")]
    pub box_: i64,
    pub due_on: Option<String>,
}

/// One sidecar on disk: the scope whose cards it holds, what that scope is
/// called, the file, and the file as it was — its modification time and its
/// length — which is what a read compares to skip the write.
struct Sidecar {
    scope: String,
    label: String,
    rel_path: String,
    stamp: (i128, u64),
}

/// What the last index of each class saw: every sidecar's stamp. Kept per
/// process — the other build keeps its own and both write the same rows —
/// so a dashboard read whose files have not moved costs a `stat` a sidecar
/// and no transaction.
static INDEXED: LazyLock<Mutex<HashMap<i64, BTreeMap<String, (i128, u64)>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn stamp_of(path: &Path) -> Option<(i128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos() as i128;
    Some((modified, meta.len()))
}

/// The sidecars a class holds: a guide's file under `.classhub/cards/`,
/// named for the guide and matched to its row, and a distilled session's
/// beside its corpus note. A file no row claims is not indexed: a renamed
/// guide's file left behind would read as a second scope holding the same
/// questions, and a copy is worse than a gap.
fn sidecars(conn: &Connection, class_id: i64, class_dir: &Path) -> Result<Vec<Sidecar>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(&format!(
        "SELECT g.scope, g.rel_path, COALESCE(u.name, d.title) FROM guides g {} WHERE g.class_id = ?1",
        crate::guides::label_joins("g")
    ))?;
    let guides = stmt
        .query_map([class_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<String>>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (scope, rel_path, unit_name) in guides {
        if crate::db::scope_family(&scope) != "guide" {
            continue;
        }
        let cards_rel = crate::guides::cards_rel_path(&rel_path);
        let Some(stamp) = stamp_of(&class_dir.join(&cards_rel)) else {
            continue;
        };
        out.push(Sidecar {
            label: crate::guides::scope_label(&scope, unit_name.as_deref()),
            scope,
            rel_path: cards_rel,
            stamp,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT rel_path, corpus_rel_path FROM lecture_contributions
         WHERE class_id = ?1 AND status = 'applied' ORDER BY rel_path",
    )?;
    let notes = stmt
        .query_map([class_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (transcript, corpus) in notes {
        let cards_rel = crate::lectures::note_sidecar(&corpus, "cards");
        let Some(stamp) = stamp_of(&class_dir.join(&cards_rel)) else {
            continue;
        };
        let scope = format!("{}{transcript}", crate::db::SESSION_SCOPE_PREFIX);
        out.push(Sidecar {
            label: crate::guides::scope_label(&scope, None),
            scope,
            rel_path: cards_rel,
            stamp,
        });
    }
    Ok(out)
}

/// Indexes a class's sidecars: every card upserted on (class, scope, front),
/// keeping its box and due date; the cards of a sidecar that is gone
/// dropped; a sidecar that will not parse named on stderr and its cards left
/// as they were, since it is not gone. Answers the scopes' labels.
///
/// A class whose folder is not there is refused rather than read as a class
/// whose sidecars are all gone: the delete pass would take every card's box
/// and due date with it, and nothing writes those back. A read that finds
/// every sidecar with the stamp it had last time writes nothing.
pub(crate) fn index(conn: &Connection, class_id: i64) -> Result<BTreeMap<String, String>> {
    Ok(index_with(conn, class_id)?.0)
}

/// `index`, saying whether it wrote — the read's gate, exposed for the test.
fn index_with(conn: &Connection, class_id: i64) -> Result<(BTreeMap<String, String>, bool)> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    if !class_dir.is_dir() {
        bail!("{} is not there — the cards stay as they are", class_dir.display());
    }
    // The files are read before the transaction opens, so the write lock is
    // held for the writes alone.
    let sidecars = sidecars(conn, class_id, &class_dir)?;
    let stamps: BTreeMap<String, (i128, u64)> =
        sidecars.iter().map(|s| (s.rel_path.clone(), s.stamp)).collect();
    let labels: BTreeMap<String, String> =
        sidecars.iter().map(|s| (s.scope.clone(), s.label.clone())).collect();
    if lock(&INDEXED).get(&class_id).is_some_and(|seen| *seen == stamps) {
        return Ok((labels, false));
    }
    let mut parsed = Vec::with_capacity(sidecars.len());
    let mut unreadable: BTreeSet<String> = BTreeSet::new();
    for sidecar in sidecars {
        let cards = std::fs::read_to_string(class_dir.join(&sidecar.rel_path))
            .context("reading")
            .and_then(|json| crate::guides::read_cards(&json));
        match cards {
            Ok(cards) => parsed.push((sidecar, cards)),
            Err(e) => {
                eprintln!("cards: {} not indexed — {e:#}", sidecar.rel_path);
                unreadable.insert(sidecar.scope);
            }
        }
    }

    // Immediate: the write lock is taken up front, which the busy handler
    // covers; a deferred transaction that had read first would be refused
    // outright when the other build committed between the read and the
    // write.
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    for (sidecar, cards) in parsed {
        let mut folded = 0usize;
        for card in cards {
            let front = truncate(card.front.trim(), MAX_SIDE);
            tx.execute(
                "INSERT INTO cards (class_id, scope, front, back, source, topic)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(class_id, scope, front) DO UPDATE SET
                   back = excluded.back, source = excluded.source, topic = excluded.topic",
                params![
                    class_id,
                    sidecar.scope,
                    front,
                    truncate(card.back.trim(), MAX_SIDE),
                    card.source.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                    card.topic.as_deref().map(str::trim).filter(|s| !s.is_empty()),
                ],
            )?;
            if !seen.insert((sidecar.scope.clone(), front)) {
                folded += 1;
            }
        }
        // Two cards with one front — the same question twice, or two fronts
        // alike for their first two thousand characters — are one row, the
        // later back standing; said, so a lost card is not silent.
        if folded > 0 {
            eprintln!(
                "cards: {folded} card(s) of {} share a front with an earlier card and were folded into it",
                sidecar.rel_path
            );
        }
    }
    let mut stmt = tx.prepare("SELECT id, scope, front FROM cards WHERE class_id = ?1")?;
    let held = stmt
        .query_map([class_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (id, scope, front) in held {
        if unreadable.contains(&scope) || seen.contains(&(scope, front)) {
            continue;
        }
        tx.execute("DELETE FROM cards WHERE id = ?1", [id])?;
    }
    tx.commit()?;
    lock(&INDEXED).insert(class_id, stamps);
    Ok((labels, true))
}

/// What a scope is called, for one card read outside an index: the
/// division's name for a unit scope, else what the scope itself says.
fn label_for(conn: &Connection, scope: &str) -> Result<String> {
    let unit_name: Option<String> = match crate::db::unit_scope_id(scope) {
        Some(unit_id) => conn
            .query_row("SELECT name FROM units WHERE id = ?1", [unit_id], |r| r.get(0))
            .optional()?,
        None => None,
    };
    Ok(crate::guides::scope_label(scope, unit_name.as_deref()))
}

fn read_card(row: &rusqlite::Row<'_>, labels: &BTreeMap<String, String>) -> rusqlite::Result<CardInfo> {
    let scope: String = row.get(4)?;
    Ok(CardInfo {
        id: row.get(0)?,
        class_id: row.get(1)?,
        class_name: row.get(2)?,
        class_color: row.get(3)?,
        scope_label: labels
            .get(&scope)
            .cloned()
            .unwrap_or_else(|| crate::guides::scope_label(&scope, None)),
        scope,
        front: row.get(5)?,
        back: row.get(6)?,
        source: row.get(7)?,
        topic: row.get(8)?,
        box_: row.get(9)?,
        due_on: row.get(10)?,
    })
}

const CARD_COLUMNS: &str = "c.id, c.class_id, k.display_name, k.color, c.scope, c.front, c.back,
                            c.source, c.topic, c.box, c.due_on";

/// A class's cards, indexed first, by scope then by the order written.
pub fn list_cards(conn: &Connection, class_id: i64) -> Result<Vec<CardInfo>> {
    let labels = index(conn, class_id)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {CARD_COLUMNS} FROM cards c JOIN classes k ON k.id = c.class_id
         WHERE c.class_id = ?1 ORDER BY c.scope, c.id"
    ))?;
    let cards = stmt
        .query_map([class_id], |row| read_card(row, &labels))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cards)
}

/// The `limit` cards due soonest across every class (SPEC §12): the ones
/// scheduled and due first — a card answered wrong yesterday comes back
/// today ahead of the backlog — then the ones never shown, the lower box
/// first among equals.
pub fn due_cards(conn: &Connection, today: &str, limit: usize) -> Result<Vec<CardInfo>> {
    let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
    let class_ids: Vec<i64> = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    for class_id in class_ids {
        match index(conn, class_id) {
            Ok(found) => labels.extend(found),
            // A class whose folder is missing keeps its cards and serves
            // none; the rest still do.
            Err(e) => eprintln!("cards: class {class_id} not indexed — {e:#}"),
        }
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {CARD_COLUMNS} FROM cards c JOIN classes k ON k.id = c.class_id
         WHERE c.due_on IS NULL OR c.due_on <= ?1
         ORDER BY c.due_on IS NULL, c.due_on, c.box, c.id LIMIT ?2"
    ))?;
    let cards = stmt
        .query_map(params![today, limit as i64], |row| read_card(row, &labels))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(cards)
}

/// Where a card goes after an answer: right moves it up a box, due after
/// the interval of the box it was in — one, three, then seven days — and
/// box three stays box three; wrong sends it to box one for tomorrow.
pub(crate) fn schedule(box_: i64, right: bool, today: NaiveDate) -> (i64, NaiveDate) {
    let box_ = box_.clamp(1, BOXES);
    if right {
        let interval = INTERVALS[(box_ - 1) as usize];
        let due = today.checked_add_days(Days::new(interval)).unwrap_or(today);
        ((box_ + 1).min(BOXES), due)
    } else {
        (1, today.checked_add_days(Days::new(1)).unwrap_or(today))
    }
}

/// `Right` or `Wrong` on the dashboard's card.
pub fn answer(app: &AppHandle, id: i64, right: bool, today: &str) -> Result<CardInfo> {
    let card = with_conn(app, |conn| answer_in(conn, id, right, today))?;
    emit_hub_change(app, "cards");
    Ok(card)
}

/// The answer's write and the read back, in one transaction: the box moves
/// and the card comes back as it is now, with no index in between that
/// could fail after the move landed.
pub(crate) fn answer_in(conn: &Connection, id: i64, right: bool, today: &str) -> Result<CardInfo> {
    let today = NaiveDate::parse_from_str(today, "%Y-%m-%d").context("today is not a date")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let held: Option<(String, i64)> = tx
        .query_row("SELECT scope, box FROM cards WHERE id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((scope, box_)) = held else {
        bail!("card #{id} is no longer on the list");
    };
    let (next_box, due) = schedule(box_, right, today);
    tx.execute(
        "UPDATE cards SET box = ?1, due_on = ?2, wrong_at = ?3 WHERE id = ?4",
        params![next_box, due.to_string(), (!right).then(now), id],
    )?;
    let labels = BTreeMap::from([(scope.clone(), label_for(&tx, &scope)?)]);
    let card = tx.query_row(
        &format!("SELECT {CARD_COLUMNS} FROM cards c JOIN classes k ON k.id = c.class_id WHERE c.id = ?1"),
        [id],
        |row| read_card(row, &labels),
    )?;
    tx.commit()?;
    Ok(card)
}

/// The topics of the class's cards answered wrong and not yet right again,
/// latest first — part of an exam's default focus (SPEC §8.3).
pub(crate) fn weak_topics(conn: &Connection, class_id: i64) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT topic FROM cards
         WHERE class_id = ?1 AND wrong_at IS NOT NULL AND topic IS NOT NULL AND topic != ''
         ORDER BY wrong_at DESC, id",
    )?;
    let mut topics: Vec<String> = Vec::new();
    for topic in stmt.query_map([class_id], |r| r.get::<_, String>(0))? {
        let topic = topic?;
        if !topics.contains(&topic) {
            topics.push(topic);
        }
        if topics.len() == MAX_WEAK_TOPICS {
            break;
        }
    }
    Ok(topics)
}

/// `Export for Anki` (SPEC §12): the class's cards as one tab-separated
/// file at `Study Guides/Cards/<class>.tsv` — front, back, tags — opening
/// with Anki's own header lines so the import needs no dialog settings.
/// Answers the file's class-relative path.
pub fn export_anki(app: &AppHandle, class_id: i64) -> Result<String> {
    with_conn(app, |conn| export_in(conn, class_id))
}

pub(crate) fn export_in(conn: &Connection, class_id: i64) -> Result<String> {
    let class_dir = crate::scanner::class_dir(conn, class_id)?;
    let display_name: String =
        conn.query_row("SELECT display_name FROM classes WHERE id = ?1", [class_id], |r| r.get(0))?;
    let cards = list_cards(conn, class_id)?;
    if cards.is_empty() {
        bail!("no cards to export — write a guide or distill a lecture first");
    }
    let class_tag = tag(&display_name);
    let rows: Vec<(String, String, String)> = cards
        .into_iter()
        .map(|c| {
            let tags = format!("ClassHub {class_tag} {}", tag(&c.scope_label));
            (c.front, c.back, tags)
        })
        .collect();
    let rel_path = format!("{CARDS_OUTPUT_DIR}/{}.tsv", crate::units::folder_segment(&display_name));
    let path = class_dir.join(&rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::db::write_atomic(&path, &tsv(&rows))?;
    Ok(rel_path)
}

/// Anki's text import: a header naming the separator, that fields are HTML
/// and which column holds the tags, then one card a line. A field holding a
/// tab, a line break or a quote is quoted, with its quotes doubled.
pub(crate) fn tsv(rows: &[(String, String, String)]) -> String {
    let mut out = String::from("#separator:tab\n#html:true\n#tags column:3\n");
    for (front, back, tags) in rows {
        out.push_str(&field(front));
        out.push('\t');
        out.push_str(&field(back));
        out.push('\t');
        out.push_str(&field(tags));
        out.push('\n');
    }
    out
}

fn field(s: &str) -> String {
    if s.contains(['\t', '\n', '\r', '"']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// An Anki tag holds no spaces: `Week 3 — Data Exploration` is
/// `Week_3_Data_Exploration`.
pub(crate) fn tag(s: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in s.chars() {
        if c.is_alphanumeric() || c == '-' {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{memory_db, set_setting};

    struct Scratch(std::path::PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("classhub-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The boxes' schedule: right climbs a box and comes back in one, three,
    /// then seven days, staying in box three; wrong is box one, tomorrow.
    #[test]
    fn the_boxes_schedule_one_three_then_seven_days() {
        let today = d("2026-09-09");
        assert_eq!(schedule(1, true, today), (2, d("2026-09-10")));
        assert_eq!(schedule(2, true, today), (3, d("2026-09-12")));
        assert_eq!(schedule(3, true, today), (3, d("2026-09-16")));
        assert_eq!(schedule(3, false, today), (1, d("2026-09-10")));
        assert_eq!(schedule(1, false, today), (1, d("2026-09-10")));
        assert_eq!(schedule(0, true, today), (2, d("2026-09-10")), "a box below one reads as one");
        assert_eq!(schedule(9, true, today), (3, d("2026-09-16")), "a box past three reads as three");
    }

    /// Anki's file: the header lines, a field holding a tab, a line break
    /// or a quote quoted with its quotes doubled, and tags with no spaces.
    #[test]
    fn the_tsv_escapes_what_anki_would_split_on() {
        let rows = vec![
            ("What is MCAR?".to_string(), "Missing completely at random".to_string(), "ClassHub Biostatistics".to_string()),
            ("A \"quoted\" front\twith a tab".to_string(), "line one\nline two".to_string(), "t".to_string()),
        ];
        let text = tsv(&rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(&lines[..3], &["#separator:tab", "#html:true", "#tags column:3"]);
        assert_eq!(lines[3], "What is MCAR?\tMissing completely at random\tClassHub Biostatistics");
        assert_eq!(lines[4], "\"A \"\"quoted\"\" front\twith a tab\"\t\"line one");
        assert_eq!(lines[5], "line two\"\tt");
        assert_eq!(tag("Week 3 — Data Exploration, Processing"), "Week_3_Data_Exploration_Processing");
        assert_eq!(tag("Biostatistics for AI"), "Biostatistics_for_AI");
        assert_eq!(tag("—"), "untitled");
    }

    /// The index (SPEC §12): a guide's sidecar under its row's scope and a
    /// session's beside its note, a file no row claims left out, upserted on
    /// the front so a rewrite keeps a card's box and due date, a card the
    /// rewrite dropped dropped, a sidecar that is gone taking its cards, a
    /// read with nothing changed writing nothing, and the due list across
    /// classes — the scheduled first — with the boxes' answers, the weak
    /// topics and the export.
    #[test]
    fn sidecars_are_indexed_on_read_and_served_by_due_date() {
        let conn = memory_db();
        let scratch = Scratch::new("cards");
        let root = scratch.0.clone();
        set_setting(&conn, "aibhs_root", &root.to_string_lossy()).unwrap();
        let bio = root.join("Biostatistics for AI");
        let fun = root.join("Fundamentals of Artificial Intelligence in Medicine I");
        std::fs::create_dir_all(bio.join(".classhub/cards")).unwrap();
        std::fs::create_dir_all(bio.join(".classhub/corpus/Week 3")).unwrap();
        std::fs::create_dir_all(fun.join(".classhub/cards")).unwrap();
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, number, source)
             VALUES (8, 3, 3, 'week', 'Week 3 — Data Exploration', 3, 'syllabus')",
            [],
        )
        .unwrap();
        crate::guides::upsert_guide(&conn, 3, "unit:8", "Study Guides/Week 3 — Data Exploration.html", "[]").unwrap();
        std::fs::write(
            bio.join(".classhub/cards/Week 3 — Data Exploration.json"),
            r#"[{"front":"What is MCAR?","back":"Missing completely at random","source":"note","topic":"missing data"},
                {"front":"Define IQR","back":"Q3 − Q1","source":"deck","topic":"quantiles"}]"#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
             VALUES (3, 8, 'Weeks/Week 03/2026-09-03 — Lecture.md', 0, 1, 1, 2,
                     '.classhub/corpus/Week 3/2026-09-03 — Lecture.md', '', 'high', 'applied', 1)",
            [],
        )
        .unwrap();
        std::fs::write(
            bio.join(".classhub/corpus/Week 3/2026-09-03 — Lecture.cards.json"),
            r#"[{"front":"Listwise deletion","back":"Dropping every incomplete row","source":"00:45","topic":"missing data"}]"#,
        )
        .unwrap();
        // A file no row claims, on another class: not the index's.
        std::fs::write(
            fun.join(".classhub/cards/Orphan.json"),
            r#"[{"front":"Four eras","back":"Rules, statistics, deep learning, generative","source":null,"topic":"history"}]"#,
        )
        .unwrap();

        let (_, wrote) = index_with(&conn, 3).unwrap();
        assert!(wrote, "the first read indexes");
        let (_, wrote) = index_with(&conn, 3).unwrap();
        assert!(!wrote, "a read with every sidecar as it was writes nothing");
        let bio_cards = list_cards(&conn, 3).unwrap();
        assert_eq!(bio_cards.len(), 3);
        assert_eq!(bio_cards[0].scope_label, "2026-09-03 — Lecture", "the session's, by scope order");
        assert_eq!(bio_cards[1].scope, "unit:8");
        assert_eq!(bio_cards[1].scope_label, "Week 3 — Data Exploration");
        assert!(list_cards(&conn, 1).unwrap().is_empty(), "an unclaimed file is left out");

        // Ten due: none shown yet, so all three, the lower ids first.
        let due = due_cards(&conn, "2026-09-09", 10).unwrap();
        assert_eq!(due.len(), 3);
        assert_eq!(due_cards(&conn, "2026-09-09", 2).unwrap().len(), 2);

        // Answered right: up a box, back in a day; wrong: box one, tomorrow,
        // and its topic weak until it is right again.
        let mcar = bio_cards.iter().find(|c| c.front == "What is MCAR?").unwrap().id;
        let iqr = bio_cards.iter().find(|c| c.front == "Define IQR").unwrap().id;
        let answered = answer_in(&conn, mcar, true, "2026-09-09").unwrap();
        assert_eq!((answered.box_, answered.due_on.as_deref()), (2, Some("2026-09-10")));
        assert_eq!(answered.scope_label, "Week 3 — Data Exploration", "labelled without an index");
        let answered = answer_in(&conn, iqr, false, "2026-09-09").unwrap();
        assert_eq!((answered.box_, answered.due_on.as_deref()), (1, Some("2026-09-10")));
        assert!(answer_in(&conn, 999, true, "2026-09-09").is_err(), "a card that is gone is refused");
        assert_eq!(weak_topics(&conn, 3).unwrap(), vec!["quantiles".to_string()]);
        let due_today = due_cards(&conn, "2026-09-09", 10).unwrap();
        assert!(due_today.iter().all(|c| c.id != mcar && c.id != iqr), "both wait for tomorrow");
        // Tomorrow the scheduled two come first — the wrong one in box one
        // ahead of the right one in box two — and the never-shown card after.
        let due_tomorrow: Vec<i64> = due_cards(&conn, "2026-09-10", 10).unwrap().iter().map(|c| c.id).collect();
        assert_eq!(due_tomorrow.len(), 3);
        assert_eq!(&due_tomorrow[..2], &[iqr, mcar]);

        // The export: the header, one line a card, at the class's own file.
        let rel = export_in(&conn, 3).unwrap();
        assert_eq!(rel, "Study Guides/Cards/Biostatistics for AI.tsv");
        let text = std::fs::read_to_string(bio.join(&rel)).unwrap();
        assert_eq!(text.lines().count(), 6);
        assert!(text.contains("\tClassHub Biostatistics_for_AI Week_3_Data_Exploration\n"), "{text}");
        assert!(export_in(&conn, 1).unwrap_err().to_string().contains("no cards"));

        // The guide is rewritten: one card kept with its box, one changed
        // back, one dropped, one added, and a front twice folded into one.
        std::fs::write(
            bio.join(".classhub/cards/Week 3 — Data Exploration.json"),
            r#"[{"front":"What is MCAR?","back":"Missingness unrelated to anything","source":"note","topic":"missing data"},
                {"front":"What is MAR?","back":"Missingness explained by observed values","source":"note","topic":"missing data"},
                {"front":"What is MAR?","back":"Said twice","source":"note","topic":"missing data"}]"#,
        )
        .unwrap();
        let again = list_cards(&conn, 3).unwrap();
        assert_eq!(again.len(), 3);
        let kept = again.iter().find(|c| c.id == mcar).expect("kept under its id");
        assert_eq!((kept.box_, kept.back.as_str()), (2, "Missingness unrelated to anything"));
        assert!(again.iter().all(|c| c.id != iqr), "dropped with the rewrite");
        assert_eq!(again.iter().find(|c| c.front == "What is MAR?").unwrap().back, "Said twice");
        assert!(weak_topics(&conn, 3).unwrap().is_empty(), "the wrong card went with the rewrite");

        // The session's sidecar is gone: its card goes; the guide's stay.
        std::fs::remove_file(bio.join(".classhub/corpus/Week 3/2026-09-03 — Lecture.cards.json")).unwrap();
        let after = list_cards(&conn, 3).unwrap();
        assert_eq!(after.len(), 2);
        assert!(after.iter().all(|c| c.scope == "unit:8"));

        // A sidecar that will not parse keeps its cards as they were.
        std::fs::write(bio.join(".classhub/cards/Week 3 — Data Exploration.json"), "not json").unwrap();
        assert_eq!(list_cards(&conn, 3).unwrap().len(), 2);

        // A class folder that is not there — a volume unmounted, a folder
        // renamed — is refused rather than read as a class whose sidecars
        // are all gone: the rows keep their boxes.
        set_setting(&conn, "aibhs_root", &root.join("elsewhere").to_string_lossy()).unwrap();
        assert!(index(&conn, 3).unwrap_err().to_string().contains("is not there"));
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM cards WHERE class_id = 3", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 2);
        assert!(due_cards(&conn, "2026-09-20", 10).unwrap().iter().any(|c| c.id == mcar), "still served");
    }
}
