//! SPEC §7.1 — the recordings behind the Zoom tool in Canvas's course
//! navigation.
//!
//! Every course carries a `Zoom Conferences` tab, an LTI launch into Zoom's
//! own page at `applications.zoom.us/lti/rich` (SPEC §1). Launched from the
//! signed-in Canvas window through a sessionless launch, the page opens
//! hidden with no sign-in of its own, and it holds what its React app needs
//! to read the course's recordings: `appConf.page.scid` names the launch
//! and `appConf.ajaxHeaders` the headers Zoom's API wants. So the list is
//! read the way the page reads it — a same-origin fetch from inside the page
//! with those headers — never by replaying anything from Rust, and never by
//! scraping the rendered table.
//!
//! What comes back is one row per recorded meeting: Zoom's id for it, the
//! topic, a UTC start, a duration in minutes. The recording's files are one
//! more read per meeting, and the video's `playUrl` on `ufl.zoom.us/rec/play/`
//! is the player the capture already reads (`zoom.rs`), so a found recording
//! goes through the same ingestion as a pasted link, with the window hidden.
//!
//! The sync records what it finds; the capture files it. Nothing here is
//! captured twice — the meeting id is unique — and a recording that is not a
//! lecture is recorded as skipped with the reason, so the listing says why.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::canvas::Session;
use crate::db::{emit_hub_change, now, with_conn, WEEKS_DIR};
use crate::remote::{close_label, eval, is_zoom_host, js_string};
use crate::zoom::Reveal;

const WINDOW_LABEL: &str = "zoom-recordings";
/// The launch page posts to Zoom on its own; measured at two seconds (SPEC
/// §1). Bounded, so a page that never lands is not a hang.
const LAND_TIMEOUT: Duration = Duration::from_secs(45);
const LAND_POLL: Duration = Duration::from_millis(500);
const EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// One list or files read, including the poll for its in-page completion.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_POLL: Duration = Duration::from_millis(250);
/// Zoom's page size for the list; a course's term of meetings is one or two.
const MAX_PAGES: usize = 10;
/// Under this a recording is a test of the room, not a lecture: every course
/// shows a few of a minute or less beside its real sessions (SPEC §1).
pub const MIN_MINUTES: i64 = 10;

/// What a listing found for one course.
#[derive(Debug, Default, Clone, Copy)]
pub struct Listing {
    /// Recordings the list carries in all.
    pub found: usize,
    /// Recorded as `new` by this read — lectures waiting for a capture.
    pub new: usize,
    /// Recorded as `skipped` by this read, with their reasons.
    pub skipped: usize,
}

/// One recording as Zoom lists it.
#[derive(Debug, Clone, PartialEq)]
struct Found {
    meeting_id: String,
    topic: String,
    /// Local wall-clock ISO, `YYYY-MM-DDTHH:MM`.
    recorded_at: String,
    duration_minutes: i64,
}

/// Reads the course's recordings off the Zoom tool and records every one
/// the table does not hold yet (SPEC §7.1).
pub(crate) fn sync_for_course(
    app: &AppHandle,
    session: &Session,
    class_id: i64,
    course_id: i64,
    on_stage: &dyn Fn(&str),
) -> Result<Listing> {
    let _listing = match LISTING.try_lock() {
        Ok(guard) => guard,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            bail!("a read of the Zoom recordings is already in flight — try again when it ends")
        }
    };
    let Some(launch) = session.zoom_launch_url(course_id, on_stage)? else {
        bail!("the course shows no Zoom tool in its navigation");
    };
    on_stage("Reading the Zoom recordings…");
    let window = open_hidden(app, &launch)?;
    // Closed however the read ends.
    struct Closer(WebviewWindow);
    impl Drop for Closer {
        fn drop(&mut self) {
            let _ = self.0.close();
        }
    }
    let closer = Closer(window);
    let window = &closer.0;

    let opened = Instant::now();
    loop {
        let left = LAND_TIMEOUT.saturating_sub(opened.elapsed());
        if left.is_zero() {
            bail!("the Zoom tool's page did not load within {}s", LAND_TIMEOUT.as_secs());
        }
        if let Ok(v) = eval_within(window, LAND_JS, left) {
            let host = v["host"].as_str().unwrap_or_default();
            if is_zoom_host(host) && v["scid"].as_bool().unwrap_or(false) {
                eprintln!(
                    "recordings: the Zoom tool landed on {host} after {:.1}s",
                    opened.elapsed().as_secs_f64()
                );
                break;
            }
        }
        std::thread::sleep(LAND_POLL);
    }

    let today = chrono::Local::now().date_naive();
    let mut found: Vec<Found> = Vec::new();
    let mut total: Option<usize> = None;
    for page in 1..=MAX_PAGES {
        let path = format!(
            "/api/v1/lti/rich/recording/COURSE?startTime=&endTime={}&keyWord=&searchType=1\
             &status=&page={page}&total={}",
            today.format("%Y-%m-%d"),
            total.unwrap_or(0)
        );
        let started = Instant::now();
        let body = request(window, &path)?;
        let (rows, page_size, listed_total) = parse_list(&body)?;
        eprintln!(
            "recordings: list page {page} · {} of {listed_total} in {:.1}s",
            rows.len(),
            started.elapsed().as_secs_f64()
        );
        total = Some(listed_total);
        let short = rows.len() < page_size;
        found.extend(rows);
        if short || found.len() >= listed_total {
            break;
        }
    }

    // The lookups under one lock, the reads under none, the writes under
    // one: a files read needs the main thread to dispatch its eval, and a
    // command that takes the connection parks that thread, so a read issued
    // with the lock held would wait on itself.
    let (known, meetings, filed_by_hand) = with_conn(app, |conn| {
        Ok((
            known_ids(conn)?,
            meeting_weekdays(conn, class_id)?,
            dates_filed_by_hand(conn, class_id)?,
        ))
    })?;
    let found_count = found.len();
    let rows = decide(found, &known, &meetings, &filed_by_hand, today, &|meeting_id| {
        play_url(window, meeting_id)
    });
    let mut listing = with_conn(app, |conn| record_rows(conn, class_id, &rows))?;
    listing.found = found_count;
    if listing.new > 0 || listing.skipped > 0 {
        emit_hub_change(app, "recordings");
    }
    Ok(listing)
}

/// One listed recording with its verdict, ready to record.
#[derive(Debug, Clone, PartialEq)]
struct Decided {
    found: Found,
    status: &'static str,
    note: Option<String>,
    play_url: Option<String>,
}

/// Judges every listed recording the table does not already hold — the
/// pure half of the listing, with the one read it needs (the files of a
/// lecture, for its player link) handed in. A meeting listed twice, as a
/// page boundary that moved can list one, is judged once.
fn decide(
    found: Vec<Found>,
    known: &HashSet<String>,
    meetings: &[u32],
    filed_by_hand: &HashSet<String>,
    today: NaiveDate,
    play_url_of: &dyn Fn(&str) -> Result<Option<String>>,
) -> Vec<Decided> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut rows = Vec::new();
    for recording in found {
        if known.contains(&recording.meeting_id) || !seen.insert(recording.meeting_id.clone()) {
            continue;
        }
        let (status, note, play_url) = match verdict(&recording, meetings, filed_by_hand, today) {
            Verdict::Lecture => match play_url_of(&recording.meeting_id) {
                Ok(Some(url)) => ("new", None, Some(url)),
                Ok(None) => ("skipped", Some("no playable recording file is listed".to_string()), None),
                Err(e) => (
                    "failed",
                    Some(crate::db::truncate(&format!("its files could not be read: {e:#}"), MAX_NOTE_CHARS)),
                    None,
                ),
            },
            Verdict::Skip(why) => ("skipped", Some(why), None),
        };
        rows.push(Decided { found: recording, status, note, play_url });
    }
    rows
}

/// Records the judged rows: a new meeting inserted, and a meeting whose
/// files read failed before — a `failed` row with no player link, which
/// `known_ids` leaves out so the next listing judges it again — refreshed in
/// place. Any other row the id already names is left as it is, so a listing
/// that overlaps what another class recorded writes nothing over it.
fn record_rows(conn: &Connection, class_id: i64, rows: &[Decided]) -> Result<Listing> {
    let mut listing = Listing::default();
    for row in rows {
        let written = conn.execute(
            "INSERT INTO recordings
             (class_id, meeting_id, play_url, recorded_at, duration_minutes, title,
              status, note, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(meeting_id) DO UPDATE SET
               play_url = excluded.play_url, status = excluded.status,
               note = excluded.note, seen_at = excluded.seen_at
             WHERE recordings.status = 'failed' AND recordings.play_url IS NULL",
            params![
                class_id,
                row.found.meeting_id,
                row.play_url,
                row.found.recorded_at,
                row.found.duration_minutes,
                row.found.topic,
                row.status,
                row.note,
                now()
            ],
        )?;
        if written == 0 {
            continue;
        }
        match row.status {
            "new" => listing.new += 1,
            "skipped" => listing.skipped += 1,
            _ => {}
        }
    }
    Ok(listing)
}

/// The meetings the table already answers for, across every class since the
/// id is global: all but a `failed` row with no player link, whose files
/// read is worth another try.
fn known_ids(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT meeting_id FROM recordings WHERE NOT (status = 'failed' AND play_url IS NULL)",
    )?;
    let ids = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids.into_iter().collect())
}

enum Verdict {
    Lecture,
    Skip(String),
}

/// A recording dated this far from today is a listing error, not a lecture
/// of the term — the window the announcement scan holds a proposed date to.
const MAX_DAYS_FROM_TODAY: i64 = 200;
/// A skip or failure note as stored: a line, whatever the error carried.
const MAX_NOTE_CHARS: usize = 500;

/// Whether a listed recording is a lecture to capture, or why not: too short
/// to be one, dated outside the term, on a day the course does not meet, or
/// on a date a lecture was already filed by hand.
fn verdict(
    recording: &Found,
    meetings: &[u32],
    filed_by_hand: &HashSet<String>,
    today: NaiveDate,
) -> Verdict {
    if recording.duration_minutes < MIN_MINUTES {
        return Verdict::Skip(format!(
            "{} — a test of the room, not a lecture",
            minutes_label(recording.duration_minutes)
        ));
    }
    let date = recording.recorded_at.get(..10).unwrap_or_default();
    let Some(day) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        return Verdict::Skip("its start time could not be read".to_string());
    };
    if (day - today).num_days().abs() > MAX_DAYS_FROM_TODAY {
        return Verdict::Skip(format!("dated {}, outside the term", day.format("%b %-d, %Y")));
    }
    if !meetings.contains(&day.weekday().number_from_monday()) {
        return Verdict::Skip(format!(
            "recorded on a {}, when the course does not meet",
            day.format("%A")
        ));
    }
    if filed_by_hand.contains(date) {
        return Verdict::Skip(format!("a lecture for {} is already filed", day.format("%b %-d")));
    }
    Verdict::Lecture
}

fn minutes_label(minutes: i64) -> String {
    if minutes == 1 {
        "1 minute".to_string()
    } else {
        format!("{minutes} minutes")
    }
}

/// The class's meeting days, 1 = Monday … 7 = Sunday.
fn meeting_weekdays(conn: &Connection, class_id: i64) -> Result<Vec<u32>> {
    let mut stmt = conn.prepare("SELECT weekday FROM meetings WHERE class_id = ?1")?;
    let days = stmt
        .query_map([class_id], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(days
        .into_iter()
        .filter_map(|d| u32::try_from(d).ok().filter(|d| (1..=7).contains(d)))
        .collect())
}

/// The dates of the lectures filed in this class that no row of this table
/// claims — the owner's own filings, and a capture whose mark was lost —
/// which a found recording of the same day must not sit beside as a second
/// copy. Read off the contributions, which is what filing a transcript
/// writes, so a note or a deck carrying a date under `Weeks/` counts for
/// nothing.
fn dates_filed_by_hand(conn: &Connection, class_id: i64) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path FROM lecture_contributions
         WHERE class_id = ?1 AND status = 'applied'
           AND rel_path NOT IN (SELECT rel_path FROM recordings WHERE rel_path IS NOT NULL)",
    )?;
    let paths = stmt
        .query_map([class_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(paths.iter().filter_map(|p| date_of_transcript(p)).collect())
}

/// The date a filed transcript's name opens with: `Weeks/Week 03 — …/2026-09-08 — Lecture.md`.
fn date_of_transcript(rel_path: &str) -> Option<String> {
    let name = rel_path.rsplit('/').next()?;
    let date = name.get(..10)?;
    NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    Some(date.to_string())
}

// ---------------------------------------------------------------------------
// Which week a found recording files into

/// The week a recording of `date` files into (SPEC §7.1): for a course whose
/// weeks carry dates, the week whose meeting is nearest, as the form's
/// default is; for one whose divisions name week ranges and no days, the
/// one-meeting rule over what is already filed. `None` when nothing settles
/// it, and the row waits for the form.
pub(crate) fn week_for(conn: &Connection, class_id: i64, date: &str) -> Result<Option<i64>> {
    let slots = crate::units::week_slots(conn, class_id)?;
    if slots.is_empty() {
        return Ok(None);
    }
    if slots.iter().any(|s| s.meets_on.is_some()) {
        return Ok(crate::units::nearest_week(&slots, date));
    }
    let Some(day) = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok() else {
        return Ok(None);
    };
    let meetings = meeting_weekdays(conn, class_id)?;
    let declared: Vec<i64> = slots.iter().map(|s| s.week).collect();
    Ok(week_by_meetings(last_filed(conn, class_id)?, day, &meetings, &declared))
}

/// The one-meeting rule: each course meets once a week and divides itself
/// no finer, so one meeting is one week. Counting the course's meeting days
/// after the latest filed lecture up to the recording's date: none passed
/// is the same meeting again — a second part of one session — and files
/// into the same week; exactly one is the next week; more is a gap nothing
/// here should bridge by arithmetic, and a recording older than the latest
/// filing is the form's to place. `None` with nothing filed yet.
pub(crate) fn week_by_meetings(
    last: Option<(NaiveDate, i64)>,
    date: NaiveDate,
    meeting_weekdays: &[u32],
    declared: &[i64],
) -> Option<i64> {
    let (last_date, last_week) = last?;
    if date < last_date {
        return None;
    }
    let passed = last_date
        .iter_days()
        .skip(1)
        .take_while(|d| *d <= date)
        .filter(|d| meeting_weekdays.contains(&d.weekday().number_from_monday()))
        .count();
    match passed {
        0 => Some(last_week),
        1 => Some(last_week + 1).filter(|w| declared.contains(w)),
        _ => None,
    }
}

/// The latest filed lecture of the class — its date and the week of the
/// folder it sits in — read off the contributions, which is what filing
/// writes.
fn last_filed(conn: &Connection, class_id: i64) -> Result<Option<(NaiveDate, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT rel_path FROM lecture_contributions WHERE class_id = ?1 AND status = 'applied'",
    )?;
    let paths = stmt
        .query_map([class_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(paths
        .iter()
        .filter_map(|p| {
            let date = NaiveDate::parse_from_str(&date_of_transcript(p)?, "%Y-%m-%d").ok()?;
            let week = crate::units::week_from_rel_path(p)?;
            Some((date, week))
        })
        .max_by_key(|(date, _)| *date))
}

// ---------------------------------------------------------------------------
// Capturing what was found

/// What a capture pass did. The counts add up to the rows it was given:
/// every row is captured, waiting for the form, skipped, failed or left.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Captured {
    pub captured: usize,
    /// Rows whose week nothing settled, left for the form.
    pub waiting: usize,
    /// Rows a lecture already filed for their day covers.
    pub skipped: usize,
    pub failed: usize,
    /// Rows past the cap, past a stop, or behind a class already ingesting.
    pub left: usize,
    pub notes: Vec<String>,
}

impl Captured {
    /// `2 captured, 1 waiting for the form, 1 failed, 3 past the cap`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.captured > 0 {
            parts.push(format!("{} captured", self.captured));
        }
        if self.waiting > 0 {
            parts.push(format!("{} waiting for the form", self.waiting));
        }
        if self.skipped > 0 {
            parts.push(format!("{} already filed", self.skipped));
        }
        if self.failed > 0 {
            parts.push(format!("{} failed", self.failed));
        }
        if self.left > 0 {
            parts.push(format!("{} past the cap", self.left));
        }
        if parts.is_empty() {
            "nothing new".to_string()
        } else {
            parts.join(", ")
        }
    }
}

struct Pending {
    id: i64,
    class_id: i64,
    class_name: String,
    play_url: String,
    recorded_at: String,
}

/// Where a capture left its row (SPEC §7.1), applied by `record_transition`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Transition {
    /// Filed and marked by the ingestion itself.
    Filed { rel_path: String },
    /// Filed, but the row's mark did not land: the row stays as it is, and
    /// the next pass finds the day filed and skips it, so nothing is
    /// captured twice while the reader hears why.
    NotMarked { rel_path: String },
    /// The ingestion refused or failed; the row says why and is tried again.
    Failed { why: String },
    /// A lecture is already filed for that day.
    Skipped { why: String },
    /// No week settles it; the form's to place.
    Waiting,
}

fn record_transition(conn: &Connection, id: i64, transition: &Transition) -> Result<()> {
    let (status, note): (&str, Option<String>) = match transition {
        Transition::Filed { .. } => return Ok(()),
        Transition::NotMarked { .. } => return Ok(()),
        Transition::Failed { why } => ("failed", Some(crate::db::truncate(why, MAX_NOTE_CHARS))),
        Transition::Skipped { why } => ("skipped", Some(crate::db::truncate(why, MAX_NOTE_CHARS))),
        Transition::Waiting => ("new", Some("pick its week in the Add lecture form".to_string())),
    };
    conn.execute(
        "UPDATE recordings SET status = ?1, note = ?2 WHERE id = ?3",
        params![status, note, id],
    )?;
    Ok(())
}

/// The listing's guard: one read of the Zoom tool at a time, since one
/// window carries it and a second read would close the first's.
static LISTING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Files every recording waiting for a capture — `new`, and `failed` for
/// another try — oldest first, up to `cap`, through the ordinary ingestion
/// with the window hidden as `reveal` says; `stop` is asked between rows,
/// and a row it stops ahead of counts as left. The class's ingestion claim
/// is taken before a row spends the cap, so a class the form is adding a
/// lecture for leaves its rows for later rather than failing them. No
/// digest is enqueued: by hand the row's `Distill` is a click away, and the
/// shift's own Distill step lists a filed lecture without its note under
/// the digest cap (SPEC §6).
pub(crate) fn capture_waiting(
    app: &AppHandle,
    class_id: Option<i64>,
    cap: usize,
    reveal: Reveal,
    stop: &dyn Fn() -> bool,
    on_stage: &dyn Fn(&str),
) -> Captured {
    let mut out = Captured::default();
    let pending = match with_conn(app, |conn| {
        let mut stmt = conn.prepare(
            "SELECT r.id, r.class_id, c.display_name, r.play_url, r.recorded_at
             FROM recordings r JOIN classes c ON c.id = r.class_id
             WHERE r.status IN ('new', 'failed') AND r.play_url IS NOT NULL
               AND (?1 IS NULL OR r.class_id = ?1)
             ORDER BY r.recorded_at, r.id",
        )?;
        let rows = stmt
            .query_map([class_id], |row| {
                Ok(Pending {
                    id: row.get(0)?,
                    class_id: row.get(1)?,
                    class_name: row.get(2)?,
                    play_url: row.get(3)?,
                    recorded_at: row.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }) {
        Ok(rows) => rows,
        Err(e) => {
            out.notes.push(format!("the recordings could not be read: {e:#}"));
            return out;
        }
    };
    let mut done = 0usize;
    let mut changed = false;
    for p in pending {
        if done >= cap || stop() {
            out.left += 1;
            continue;
        }
        let date = p.recorded_at.get(..10).unwrap_or_default().to_string();
        let day_label = NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .map(|d| d.format("%a, %b %-d").to_string())
            .unwrap_or_else(|_| date.clone());
        // The claim first: a class the form is adding a lecture for is not a
        // failure, and its rows wait for the next pass without a slot spent.
        let _claim = match crate::lectures::claim_ingest(p.class_id) {
            Ok(claim) => claim,
            Err(e) => {
                out.notes.push(format!("{day_label}: {e:#}"));
                out.left += 1;
                continue;
            }
        };
        let plan = with_conn(app, |conn| {
            if dates_filed_by_hand(conn, p.class_id)?.contains(&date) {
                return Ok(Err(Transition::Skipped {
                    why: format!("a lecture for {day_label} is already filed"),
                }));
            }
            Ok(week_for(conn, p.class_id, &date)?.ok_or(Transition::Waiting))
        });
        let week = match plan {
            Ok(Ok(week)) => week,
            Ok(Err(transition)) => {
                match &transition {
                    Transition::Skipped { .. } => out.skipped += 1,
                    _ => out.waiting += 1,
                }
                changed |= settle(app, &p, &day_label, &transition, &mut out);
                continue;
            }
            Err(e) => {
                let transition = Transition::Failed { why: format!("{e:#}") };
                out.failed += 1;
                out.notes.push(format!("{day_label}: {e:#}"));
                changed |= settle(app, &p, &day_label, &transition, &mut out);
                continue;
            }
        };
        done += 1;
        on_stage(&format!("Capturing {}'s {day_label} recording…", p.class_name));
        let request = crate::lectures::AddRequest {
            class_id: p.class_id,
            source: p.play_url.clone(),
            week: Some(week),
            date: date.clone(),
            title: None,
            digest: false,
            recording_id: Some(p.id),
        };
        let transition = match crate::lectures::add_with(app, &request, reveal, on_stage) {
            Ok(result) if result.recording_marked == Some(true) => {
                Transition::Filed { rel_path: result.rel_path }
            }
            Ok(result) => Transition::NotMarked { rel_path: result.rel_path },
            Err(e) => Transition::Failed { why: format!("{e:#}") },
        };
        match &transition {
            Transition::Filed { rel_path } => {
                out.captured += 1;
                let folder = rel_path.rsplit_once('/').map(|(dir, _)| dir.to_string());
                crate::db::notify(
                    app,
                    format!(
                        "Captured {}'s {day_label} recording into {}/",
                        p.class_name,
                        folder.unwrap_or_else(|| WEEKS_DIR.to_string())
                    ),
                    Vec::new(),
                    Some(p.class_id),
                );
            }
            Transition::NotMarked { rel_path } => {
                out.failed += 1;
                out.notes.push(format!(
                    "{day_label}: filed as {rel_path}, but its row could not be marked — the next \
                     pass skips that day"
                ));
            }
            Transition::Failed { why } => {
                out.failed += 1;
                out.notes.push(format!("{day_label}: {why}"));
            }
            _ => {}
        }
        changed |= settle(app, &p, &day_label, &transition, &mut out);
    }
    if changed {
        emit_hub_change(app, "recordings");
    }
    out
}

/// Writes a transition onto its row; a write that fails is a note, since
/// the row is what the next pass reads. Answers whether the row changed.
fn settle(app: &AppHandle, p: &Pending, day_label: &str, transition: &Transition, out: &mut Captured) -> bool {
    match with_conn(app, |conn| record_transition(conn, p.id, transition)) {
        Ok(()) => !matches!(transition, Transition::NotMarked { .. }),
        Err(e) => {
            out.notes.push(format!("{day_label}: its row could not be updated — {e:#}"));
            false
        }
    }
}
// ---------------------------------------------------------------------------
// The listing the workspace shows, and the find run by hand

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub id: i64,
    pub class_id: i64,
    pub title: String,
    pub recorded_at: String,
    pub duration_minutes: i64,
    /// new | failed — the rows still waiting; a filed one is its transcript.
    pub status: String,
    pub note: Option<String>,
}

/// The class's recordings still waiting for a capture, newest first.
pub fn list_waiting(conn: &Connection, class_id: i64) -> Result<Vec<RecordingInfo>> {
    let mut stmt = conn.prepare(
        "SELECT id, class_id, title, recorded_at, duration_minutes, status, note
         FROM recordings WHERE class_id = ?1 AND status IN ('new', 'failed')
         ORDER BY recorded_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([class_id], |row| {
            Ok(RecordingInfo {
                id: row.get(0)?,
                class_id: row.get(1)?,
                title: row.get(2)?,
                recorded_at: row.get(3)?,
                duration_minutes: row.get(4)?,
                status: row.get(5)?,
                note: row.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The player link of a waiting recording, for the form to open pre-filled.
pub fn play_url_of(conn: &Connection, id: i64) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT play_url FROM recordings WHERE id = ?1", [id], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// Marks a waiting recording filed, from inside the ingestion that filed it
/// (`lectures::add_with`) — the capture's and the form's alike — and only
/// while the source filed is the row's own player link, so a form opened
/// from a recording and then pointed elsewhere files that instead and
/// leaves the recording waiting. Answers whether the row was marked.
pub(crate) fn mark_filed(conn: &Connection, id: i64, source: &str, rel_path: &str) -> Result<bool> {
    let marked = conn.execute(
        "UPDATE recordings SET status = 'filed', rel_path = ?1, note = NULL
         WHERE id = ?2 AND play_url = ?3 AND status IN ('new', 'failed')",
        params![rel_path, id, source.trim()],
    )?;
    Ok(marked == 1)
}

pub const PROGRESS_EVENT: &str = "recordings://progress";

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Progress<'a> {
    class_id: i64,
    stage: &'a str,
    done: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Classes with a find in flight.
static FINDING: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// `Find recordings` from the Lectures section: reads the course's list
/// through a Canvas session — asking for a sign-in if Canvas wants one —
/// then captures what is waiting, the window shown only when Zoom asks.
/// Reports over `PROGRESS_EVENT`; refused while one runs for the class.
pub fn spawn_find(app: &AppHandle, class_id: i64) -> Result<()> {
    {
        let mut busy = crate::db::lock(&FINDING);
        if busy.contains(&class_id) {
            bail!("recordings are already being looked for in this class");
        }
        busy.push(class_id);
    }
    let app = app.clone();
    std::thread::spawn(move || {
        struct Claim(i64);
        impl Drop for Claim {
            fn drop(&mut self) {
                crate::db::lock(&FINDING).retain(|id| *id != self.0);
            }
        }
        let _claim = Claim(class_id);
        let emit = |stage: &str, done: bool, summary: Option<String>, error: Option<String>| {
            let _ = app.emit(
                PROGRESS_EVENT,
                Progress { class_id, stage, done, summary, error },
            );
        };
        let on_stage = |stage: &str| emit(stage, false, None, None);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            find(&app, class_id, &on_stage)
        }));
        match outcome {
            Ok(Ok(summary)) => emit("Done", true, Some(summary), None),
            Ok(Err(e)) => emit("Failed", true, None, Some(format!("{e:#}"))),
            Err(_) => emit("Failed", true, None, Some("finding recordings crashed — see the log".into())),
        }
    });
    Ok(())
}

fn find(app: &AppHandle, class_id: i64, on_stage: &dyn Fn(&str)) -> Result<String> {
    let (class_name, course_id) = with_conn(app, |conn| {
        let (name, course): (String, Option<String>) = conn.query_row(
            "SELECT display_name, canvas_course_id FROM classes WHERE id = ?1",
            [class_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((name, course.and_then(|c| c.parse::<i64>().ok())))
    })?;
    let Some(course_id) = course_id else {
        bail!("this class has not been matched to a Canvas course yet — sync Canvas first");
    };
    let listing = {
        on_stage("Opening Canvas…");
        let session = Session::open(app, on_stage)?;
        sync_for_course(app, &session, class_id, course_id, on_stage)?
    };
    // Capped as the shift caps its own step: a find that meets a term's backlog
    // captures a night's worth and says what it left, and the next press
    // takes the next.
    let cap = with_conn(app, |conn| Ok(crate::shift::settings(conn).digests_per_night as usize))?;
    let captured = capture_waiting(app, Some(class_id), cap, Reveal::OnAsk, &|| false, on_stage);
    let mut summary = format!(
        "{} listed for {class_name} · {} new · {}",
        listing.found,
        listing.new,
        captured.summary()
    );
    if captured.left > 0 {
        summary.push_str(" — press again for the next");
    }
    if !captured.notes.is_empty() {
        summary.push_str(&format!(" · {}", captured.notes.join("; ")));
    }
    Ok(summary)
}

// ---------------------------------------------------------------------------
// The page

fn open_hidden(app: &AppHandle, url: &str) -> Result<WebviewWindow> {
    close_label(app, WINDOW_LABEL, "recordings")?;
    let parsed = tauri::Url::parse(url).context("parsing the Zoom launch")?;
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("Zoom recordings — ClassHub is reading the list")
        .inner_size(1000.0, 700.0)
        .visible(false)
        .build()
        .context("opening the recordings window")
}

/// One eval round-trip within `budget`, so a caller's own deadline is the
/// bound and not the deadline plus a whole eval.
fn eval_within(window: &WebviewWindow, js: &str, budget: Duration) -> Result<Value> {
    eval(window, js, budget.min(EVAL_TIMEOUT), "recordings")
}

/// Request slots on the page, one per read; unique for the window's life,
/// which is one listing.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// One read the way the page issues its own, polled to completion; the
/// answer's body as text. A page that has left Zoom — a lapsed launch, a
/// bounce to a sign-in — is named as such rather than as an answer that
/// would not parse.
fn request(window: &WebviewWindow, path: &str) -> Result<String> {
    let id = format!("r{}", REQUESTS.fetch_add(1, Ordering::Relaxed));
    let js = REQUEST_JS
        .replace("__ID__", &js_string(&id))
        .replace("__PATH__", &js_string(path));
    let started = Instant::now();
    loop {
        let left = REQUEST_TIMEOUT.saturating_sub(started.elapsed());
        if left.is_zero() {
            bail!("Zoom did not answer {path} within {}s", REQUEST_TIMEOUT.as_secs());
        }
        let v = eval_within(window, &js, left)?;
        match v["state"].as_str().unwrap_or_default() {
            "pending" => {}
            "offsite" => {
                let host = v["host"].as_str().unwrap_or("nowhere");
                bail!("the Zoom tool's page left Zoom for {host} before {path} could be read");
            }
            "error" => bail!(
                "the Zoom page could not request {path}: {}",
                v["message"].as_str().unwrap_or("no reason given")
            ),
            "done" => {
                let status = v["status"].as_u64().unwrap_or(0);
                if !(200..300).contains(&status) {
                    bail!(
                        "Zoom answered {status} for {path}: {}",
                        crate::db::truncate(v["body"].as_str().unwrap_or_default().trim(), 200)
                    );
                }
                return Ok(v["body"].as_str().unwrap_or_default().to_string());
            }
            other => bail!("the Zoom page reported an unknown state: {other}"),
        }
        std::thread::sleep(REQUEST_POLL);
    }
}

/// The video's player link among a recording's files: the first MP4, else
/// the audio, else none.
fn play_url(window: &WebviewWindow, meeting_id: &str) -> Result<Option<String>> {
    let encoded: String = meeting_id
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let body = request(window, &format!("/api/v1/lti/rich/recording/file?meetingId={encoded}"))?;
    let value: Value = serde_json::from_str(&body).context("the files answer is not JSON")?;
    Ok(play_url_in(&value))
}

/// Pure half of `play_url`, over the files answer.
fn play_url_in(value: &Value) -> Option<String> {
    let files = value["result"]["recordingFiles"].as_array()?;
    let of_type = |kind: &str| {
        files.iter().find_map(|f| {
            let url = f["playUrl"].as_str().filter(|u| !u.is_empty())?;
            let host = tauri::Url::parse(url).ok()?.host_str()?.to_ascii_lowercase();
            (f["fileType"].as_str() == Some(kind) && is_zoom_host(&host)).then(|| url.to_string())
        })
    };
    of_type("MP4").or_else(|| of_type("M4A"))
}

/// The list page: its rows, Zoom's page size, and the total it names.
fn parse_list(body: &str) -> Result<(Vec<Found>, usize, usize)> {
    let value: Value = serde_json::from_str(body).context("the list answer is not JSON")?;
    if value["status"].as_bool() != Some(true) {
        bail!(
            "Zoom declined the list: {}",
            crate::db::truncate(value["errorMessage"].as_str().unwrap_or("no reason given"), 200)
        );
    }
    let result = &value["result"];
    let page_size = result["pageSize"].as_u64().unwrap_or(12).max(1) as usize;
    let total = result["total"].as_u64().unwrap_or(0) as usize;
    let rows = result["list"]
        .as_array()
        .context("the list answer carries no list")?
        .iter()
        .filter_map(found_of)
        .collect();
    Ok((rows, page_size, total))
}

fn found_of(row: &Value) -> Option<Found> {
    let meeting_id = row["meetingId"].as_str().filter(|s| !s.is_empty())?;
    let recorded_at = local_from_utc(row["startTime"].as_str()?)?;
    Some(Found {
        meeting_id: meeting_id.to_string(),
        topic: crate::db::truncate(row["topic"].as_str().unwrap_or("Recording").trim(), 200),
        recorded_at,
        duration_minutes: row["duration"].as_i64().unwrap_or(0),
    })
}

/// Zoom's `YYYY-MM-DD HH:MM:SS`, a UTC instant, as local wall-clock ISO.
fn local_from_utc(text: &str) -> Option<String> {
    let naive = NaiveDateTime::parse_from_str(text.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    Some(
        naive
            .and_utc()
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%dT%H:%M")
            .to_string(),
    )
}

/// Where the launch has landed, and whether the page holds its launch id yet.
const LAND_JS: &str = r#"
(function () {
  var c = window.appConf;
  return { host: location.host, ready: document.readyState, scid: !!(c && c.page && c.page.scid) };
})()
"#;

/// The start-then-poll read `canvas.rs` uses, with the page's own headers:
/// `appConf.ajaxHeaders` is the list Zoom's app sends on every call, and the
/// launch id rides the query string as it does on the page's own requests.
/// The host check comes first, as it does there: a page that has navigated
/// away reports where it is rather than issuing the request from there.
const REQUEST_JS: &str = r#"
(function () {
  var S = (window.__classhub_recordings = window.__classhub_recordings || {});
  var id = __ID__;
  if (S[id]) return S[id];
  if (S["p:" + id]) return { state: "pending" };
  var host = location.host;
  if (!(host === "zoom.us" || host === "zoom.com" || /\.zoom\.(us|com)$/.test(host))) {
    return { state: "offsite", host: host };
  }
  S["p:" + id] = 1;
  var conf = window.appConf || {};
  var scid = conf.page && conf.page.scid;
  var headers = { Accept: "application/json, text/plain, */*" };
  (conf.ajaxHeaders || []).forEach(function (h) { headers[h.key] = h.value; });
  var url = __PATH__ + (scid ? "&lti_scid=" + encodeURIComponent(scid) : "");
  fetch(url, { credentials: "same-origin", headers: headers })
    .then(function (r) { return r.text().then(function (t) { S[id] = { state: "done", status: r.status, body: t }; }); })
    .catch(function (e) { S[id] = { state: "error", message: String((e && e.message) || e) }; });
  return { state: "pending" };
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// Applied Generative AI meets on Tuesdays (weekday 2) and names no
    /// dates: a recording the Tuesday after the latest filing is the next
    /// week, one the same day is the same week, two Tuesdays on is a gap the
    /// form settles, and nothing filed settles nothing.
    #[test]
    fn the_one_meeting_rule_files_after_one_meeting_and_waits_after_two() {
        let tue = [2];
        let declared: Vec<i64> = (1..=16).collect();
        let last = Some((d("2026-09-01"), 2));
        assert_eq!(week_by_meetings(last, d("2026-09-08"), &tue, &declared), Some(3));
        assert_eq!(week_by_meetings(last, d("2026-09-01"), &tue, &declared), Some(2));
        assert_eq!(week_by_meetings(last, d("2026-09-15"), &tue, &declared), None);
        assert_eq!(week_by_meetings(last, d("2026-08-25"), &tue, &declared), None);
        assert_eq!(week_by_meetings(None, d("2026-09-08"), &tue, &declared), None);
        // A week the course never declared is no week to file into.
        assert_eq!(week_by_meetings(Some((d("2026-12-08"), 16)), d("2026-12-15"), &tue, &declared), None);
    }

    fn found(id: &str, at: &str, minutes: i64) -> Found {
        Found {
            meeting_id: id.into(),
            topic: "CAI5720".into(),
            recorded_at: at.into(),
            duration_minutes: minutes,
        }
    }

    /// A recording on a day the course does not meet, or too short to be a
    /// lecture, or dated outside the term, or on a date already filed by
    /// hand, is skipped and says why.
    #[test]
    fn a_recording_is_a_lecture_only_on_a_meeting_day_and_long_enough() {
        let tue = [2];
        let today = d("2026-09-08");
        let filed: HashSet<String> = ["2026-09-01".to_string()].into_iter().collect();
        let skip = |at: &str, minutes: i64| match verdict(&found("m", at, minutes), &tue, &filed, today) {
            Verdict::Skip(why) => why,
            Verdict::Lecture => panic!("{at} for {minutes} minutes read as a lecture"),
        };
        assert!(matches!(
            verdict(&found("m", "2026-09-08T15:52", 209), &tue, &filed, today),
            Verdict::Lecture
        ));
        assert!(skip("2026-09-08T15:49", 1).contains("1 minute"));
        assert!(skip("2026-09-05T15:32", 45).contains("Saturday"));
        assert!(skip("2026-09-01T15:26", 197).contains("already filed"));
        assert!(skip("2027-09-07T15:52", 209).contains("outside the term"));
    }

    /// The listing's pure half: a meeting the table holds is passed over, a
    /// meeting listed twice is judged once, and the files read's three
    /// outcomes become `new`, `skipped` and `failed` with their notes.
    #[test]
    fn decide_judges_each_meeting_once_with_the_files_reads_outcome() {
        let tue = [2];
        let today = d("2026-09-08");
        let known: HashSet<String> = ["held".to_string()].into_iter().collect();
        let listed = vec![
            found("held", "2026-09-08T15:52", 209),
            found("ok", "2026-09-01T15:50", 279),
            found("ok", "2026-09-01T15:50", 279),
            found("nofile", "2026-08-25T15:54", 194),
            found("broken", "2026-08-18T15:54", 190),
            found("test", "2026-09-08T15:49", 1),
        ];
        let rows = decide(listed, &known, &tue, &HashSet::new(), today, &|id| match id {
            "ok" => Ok(Some("https://ufl.zoom.us/rec/play/x".into())),
            "nofile" => Ok(None),
            _ => Err(anyhow::anyhow!("500")),
        });
        let shape: Vec<(&str, &str, bool)> = rows
            .iter()
            .map(|r| (r.found.meeting_id.as_str(), r.status, r.play_url.is_some()))
            .collect();
        assert_eq!(
            shape,
            vec![("ok", "new", true), ("nofile", "skipped", false), ("broken", "failed", false), ("test", "skipped", false)]
        );
        assert!(rows[2].note.as_deref().unwrap().contains("could not be read"));
    }

    /// The list answer as Zoom serves it (SPEC §1), and the files answer:
    /// the rows with their UTC start converted, the page size and total,
    /// and the MP4's player link ahead of the audio's.
    #[test]
    fn the_list_and_the_files_answers_parse_as_zoom_serves_them() {
        let list = r#"{"status":true,"result":{"pageNum":1,"pageSize":12,"total":2,"list":[
            {"meetingId":"wxWxT6QaQAG1mMtyZ+3aqA==","topic":"CAI5720 - Fund AI in Medicine I","startTime":"2026-09-08 19:52:26","duration":209},
            {"meetingId":"nRH/mstESWm8PQPLLAUfsQ==","topic":"CAI5720 - Fund AI in Medicine I","startTime":"2026-09-03 22:15:42","duration":0},
            {"topic":"no id","startTime":"2026-09-03 22:15:42","duration":0}]}}"#;
        let (rows, page_size, total) = parse_list(list).unwrap();
        assert_eq!((rows.len(), page_size, total), (2, 12, 2));
        assert_eq!(rows[0].meeting_id, "wxWxT6QaQAG1mMtyZ+3aqA==");
        assert_eq!(rows[0].duration_minutes, 209);
        // 19:52 UTC is a local afternoon anywhere in the Americas; the shape
        // is what is asserted, since the machine's zone converts it.
        assert_eq!(rows[0].recorded_at.len(), 16);
        assert!(rows[0].recorded_at.starts_with("2026-09-08T") || rows[0].recorded_at.starts_with("2026-09-09T"));
        assert!(parse_list(r#"{"success":false,"errorMessage":"Sorry, your session was expired.","errorCode":403}"#).is_err());

        let files: Value = serde_json::from_str(r#"{"status":true,"result":{"recordingFiles":[
            {"fileType":"TIMELINE","playUrl":""},
            {"fileType":"M4A","playUrl":"https://ufl.zoom.us/rec/play/audio"},
            {"fileType":"CC","playUrl":""},
            {"fileType":"MP4","playUrl":"https://ufl.zoom.us/rec/play/video"}]}}"#).unwrap();
        assert_eq!(play_url_in(&files).as_deref(), Some("https://ufl.zoom.us/rec/play/video"));
        let audio_only: Value = serde_json::from_str(r#"{"status":true,"result":{"recordingFiles":[
            {"fileType":"M4A","playUrl":"https://ufl.zoom.us/rec/play/audio"}]}}"#).unwrap();
        assert_eq!(play_url_in(&audio_only).as_deref(), Some("https://ufl.zoom.us/rec/play/audio"));
        let elsewhere: Value = serde_json::from_str(r#"{"status":true,"result":{"recordingFiles":[
            {"fileType":"MP4","playUrl":"https://notzoom.us/rec/play/video"}]}}"#).unwrap();
        assert_eq!(play_url_in(&elsewhere), None);
    }

    /// The table takes a meeting once: a second listing of the same rows
    /// writes nothing and counts nothing, a `failed` row with no player link
    /// is the one exception — left out of the known ids and refreshed in
    /// place by the next listing — and a row another class holds is never
    /// written over.
    #[test]
    fn a_meeting_is_recorded_once_and_a_failed_files_read_is_tried_again() {
        let conn = crate::db::memory_db();
        let row = |id: &str, status: &'static str, url: Option<&str>| Decided {
            found: found(id, "2026-09-08T15:52", 209),
            status,
            note: None,
            play_url: url.map(str::to_string),
        };
        let first = record_rows(
            &conn,
            1,
            &[row("m1", "new", Some("https://ufl.zoom.us/rec/play/x")), row("m2", "failed", None)],
        )
        .unwrap();
        assert_eq!((first.new, first.skipped), (1, 0));
        // The same list again: nothing written, nothing counted.
        let again = record_rows(&conn, 1, &[row("m1", "new", Some("https://ufl.zoom.us/rec/play/x"))]).unwrap();
        assert_eq!((again.new, again.skipped), (0, 0));
        // m1 is known; m2's files read failed, so it is judged again.
        let known = known_ids(&conn).unwrap();
        assert!(known.contains("m1") && !known.contains("m2"));
        let retried = record_rows(&conn, 1, &[row("m2", "new", Some("https://ufl.zoom.us/rec/play/y"))]).unwrap();
        assert_eq!(retried.new, 1);
        let (status, url): (String, Option<String>) = conn
            .query_row("SELECT status, play_url FROM recordings WHERE meeting_id = 'm2'", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((status.as_str(), url.as_deref()), ("new", Some("https://ufl.zoom.us/rec/play/y")));
        // Another class listing m1 — one Canvas course matched twice — leaves it.
        let other = record_rows(&conn, 2, &[row("m1", "skipped", None)]).unwrap();
        assert_eq!((other.new, other.skipped), (0, 0));
        let class: i64 = conn
            .query_row("SELECT class_id FROM recordings WHERE meeting_id = 'm1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(class, 1);

        let waiting = list_waiting(&conn, 1).unwrap();
        assert_eq!(waiting.len(), 2);
        let m1 = waiting.iter().find(|w| w.status == "new" && w.id == 1).unwrap();
        // The mark takes only while the source filed is the row's own link.
        assert!(!mark_filed(&conn, m1.id, "https://ufl.zoom.us/rec/play/other", "Weeks/W/x.md").unwrap());
        assert_eq!(list_waiting(&conn, 1).unwrap().len(), 2);
        assert!(mark_filed(&conn, m1.id, " https://ufl.zoom.us/rec/play/x ", "Weeks/Week 03 — X/2026-09-08 — Lecture.md").unwrap());
        assert_eq!(list_waiting(&conn, 1).unwrap().len(), 1);
        assert!(!mark_filed(&conn, m1.id, "https://ufl.zoom.us/rec/play/x", "Weeks/W/again.md").unwrap());
    }

    /// A capture's outcome lands on its row: a failure or a skip with its
    /// note, a row nothing settles as waiting for the form, and a filing
    /// whose mark was lost left exactly as it was, so the next pass reads
    /// the day as filed. The summary adds up what the pass did.
    #[test]
    fn a_captures_transition_is_written_on_its_row() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO recordings (class_id, meeting_id, play_url, recorded_at, duration_minutes,
             title, status, seen_at) VALUES (1, 'm', 'https://ufl.zoom.us/rec/play/x',
             '2026-09-08T15:52', 209, 'T', 'new', 1)",
            [],
        )
        .unwrap();
        let read = |conn: &Connection| -> (String, Option<String>) {
            conn.query_row("SELECT status, note FROM recordings WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap()
        };
        record_transition(&conn, 1, &Transition::Waiting).unwrap();
        assert_eq!(read(&conn), ("new".into(), Some("pick its week in the Add lecture form".into())));
        record_transition(&conn, 1, &Transition::Failed { why: "x".repeat(900) }).unwrap();
        let (status, note) = read(&conn);
        assert_eq!(status, "failed");
        assert!(note.unwrap().chars().count() <= MAX_NOTE_CHARS + 1);
        record_transition(&conn, 1, &Transition::NotMarked { rel_path: "Weeks/W/x.md".into() }).unwrap();
        assert_eq!(read(&conn).0, "failed");
        record_transition(&conn, 1, &Transition::Skipped { why: "a lecture for Tue, Sep 8 is already filed".into() }).unwrap();
        assert_eq!(read(&conn).0, "skipped");
        let summary = Captured { captured: 2, waiting: 1, skipped: 1, failed: 1, left: 3, notes: vec![] }.summary();
        assert_eq!(summary, "2 captured, 1 waiting for the form, 1 already filed, 1 failed, 3 past the cap");
        assert_eq!(Captured::default().summary(), "nothing new");
    }

    /// A lecture is filed by hand when its contribution row names a path no
    /// recordings row claims; a date-named note under `Weeks/` is nothing.
    #[test]
    fn a_hand_filing_is_a_contribution_no_capture_claims() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, source)
             VALUES (1, 1, 'week', 'Week 1 — Intro', 1, 'syllabus')",
            [],
        )
        .unwrap();
        let contribution = |rel: &str| {
            conn.execute(
                "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                 start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
                 VALUES (1, (SELECT id FROM units WHERE class_id = 1), ?1, 0, 1, 0, 1, ?2, 's', 'high', 'applied', 1)",
                params![rel, format!(".classhub/corpus/Week 1 — Intro/{}", rel.rsplit('/').next().unwrap())],
            )
            .unwrap();
        };
        contribution("Weeks/Week 01 — Intro/2026-08-25 — Lecture.md");
        contribution("Weeks/Week 03 — X/2026-09-08 — Lecture.md");
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (1, 'Weeks/Week 02 — Y/2026-09-01 — Notes.md', 'b', 1, 1, 'md')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO recordings (class_id, meeting_id, recorded_at, duration_minutes, title,
             rel_path, status, seen_at)
             VALUES (1, 'm', '2026-09-08T15:52', 209, 'T', 'Weeks/Week 03 — X/2026-09-08 — Lecture.md', 'filed', 1)",
            [],
        )
        .unwrap();
        let by_hand = dates_filed_by_hand(&conn, 1).unwrap();
        assert_eq!(by_hand, ["2026-08-25".to_string()].into_iter().collect());
    }

    /// A dated course files by the nearest meeting date; an undated one by
    /// the one-meeting rule off what is filed.
    #[test]
    fn a_found_recordings_week_follows_the_course_s_dates_or_its_meetings() {
        let conn = crate::db::memory_db();
        for (ordinal, date) in [(1, "2026-08-25"), (2, "2026-09-01"), (3, "2026-09-08")] {
            conn.execute(
                "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
                 VALUES (1, ?1, 'week', ?2, ?1, ?3, 'syllabus')",
                params![ordinal, format!("Week {ordinal} — Topic"), date],
            )
            .unwrap();
        }
        assert_eq!(week_for(&conn, 1, "2026-09-08").unwrap(), Some(3));
        assert_eq!(week_for(&conn, 1, "2026-09-09").unwrap(), Some(3));

        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, first_week, last_week, source)
             VALUES (4, 1, 'part', 'Part I', 1, 1, 8, 'syllabus')",
            [],
        )
        .unwrap();
        assert_eq!(week_for(&conn, 4, "2026-09-08").unwrap(), None);
        conn.execute(
            "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
             start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
             VALUES (4, (SELECT id FROM units WHERE class_id = 4), 'Weeks/Week 02/2026-09-01 — Lecture.md',
                     0, 1, 0, 1, '.classhub/corpus/Part I/2026-09-01 — Lecture.md', 's', 'high', 'applied', 1)",
            [],
        )
        .unwrap();
        assert_eq!(week_for(&conn, 4, "2026-09-08").unwrap(), Some(3));
        assert_eq!(week_for(&conn, 4, "2026-09-15").unwrap(), None);
    }
}
