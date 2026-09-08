//! SPEC §6 — the idle shift.
//!
//! A scheduler thread runs the night's plan once the owner has stopped: sync
//! Canvas, file what the sync placed, extract, distill what has no note or no
//! hints, rebuild the division guides whose meeting has passed — under
//! per-night caps, stopping at the first rate-limit event, holding the Mac
//! awake with `caffeinate`, catching up at launch when a window was missed,
//! and never running a master. One run a night, recorded in `shift_runs` with
//! the process that ran it, so two builds on one database never both start one.

use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{Datelike, Days, Local, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{lock, now, setting, with_conn};
use crate::settings::flag;

/// Pushed whenever a run starts, moves or ends; the Job Center refetches
/// `get_shift_status` on it.
pub const CHANGED_EVENT: &str = "shift-changed";

const ENABLED: &str = "shift_enabled";
const START: &str = "shift_start";
const END: &str = "shift_end";
const IDLE_MINUTES: &str = "shift_idle_minutes";
const GUIDES_PER_NIGHT: &str = "shift_guides_per_night";
const DIGESTS_PER_NIGHT: &str = "shift_digests_per_night";
/// The night the pause was pressed for; a new night clears it on its own.
const PAUSED_ON: &str = "shift_paused_on";
const IN_DEV_BUILD: &str = "shift_in_dev_build";

const DEFAULT_START: &str = "21:00";
const DEFAULT_END: &str = "06:00";
const DEFAULT_IDLE_MINUTES: u32 = 20;
const DEFAULT_GUIDES: u32 = 2;
const DEFAULT_DIGESTS: u32 = 4;
const MAX_CAP: u32 = 20;
const MAX_IDLE_MINUTES: u32 = 180;

/// How often the scheduler looks, and how often a run asks whether a job it
/// is waiting on has settled.
const TICK: Duration = Duration::from_secs(60);
const JOB_POLL: Duration = Duration::from_secs(5);

/// The plan's steps, in order (SPEC §6). Later milestones add to the end.
pub const STEPS: [&str; 5] = ["Sync Canvas", "File", "Extract", "Distill", "Rebuild guides"];
const SYNC: usize = 0;
const FILE: usize = 1;
const EXTRACT: usize = 2;
const DISTILL: usize = 3;
const REBUILD: usize = 4;

// ---------------------------------------------------------------------------
// Settings

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShiftSettings {
    pub enabled: bool,
    /// `HH:MM`, local.
    pub start: String,
    pub end: String,
    pub idle_minutes: u32,
    pub guides_per_night: u32,
    pub digests_per_night: u32,
    /// Whether a dev build runs shifts; the installed app always does.
    pub in_dev_build: bool,
}

fn number(conn: &Connection, key: &str, default: u32, max: u32) -> u32 {
    match setting(conn, key) {
        Ok(Some(v)) => match v.parse::<u32>().ok().filter(|n| *n <= max) {
            Some(n) => n,
            None => {
                eprintln!("settings: {key} '{v}' is not 0–{max} — using {default}");
                default
            }
        },
        Ok(None) => default,
        Err(e) => {
            eprintln!("settings: {key} unreadable ({e:#}) — using {default}");
            default
        }
    }
}

fn clock(conn: &Connection, key: &str, default: &str) -> String {
    match setting(conn, key) {
        Ok(Some(v)) if parse_clock(&v).is_some() => v,
        Ok(Some(v)) => {
            eprintln!("settings: {key} '{v}' is not HH:MM — using {default}");
            default.to_string()
        }
        Ok(None) => default.to_string(),
        Err(e) => {
            eprintln!("settings: {key} unreadable ({e:#}) — using {default}");
            default.to_string()
        }
    }
}

pub(crate) fn parse_clock(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M").ok()
}

pub fn settings(conn: &Connection) -> ShiftSettings {
    ShiftSettings {
        enabled: flag(conn, ENABLED, true),
        start: clock(conn, START, DEFAULT_START),
        end: clock(conn, END, DEFAULT_END),
        idle_minutes: number(conn, IDLE_MINUTES, DEFAULT_IDLE_MINUTES, MAX_IDLE_MINUTES),
        guides_per_night: number(conn, GUIDES_PER_NIGHT, DEFAULT_GUIDES, MAX_CAP),
        digests_per_night: number(conn, DIGESTS_PER_NIGHT, DEFAULT_DIGESTS, MAX_CAP),
        in_dev_build: flag(conn, IN_DEV_BUILD, false),
    }
}

impl ShiftSettings {
    fn window(&self) -> (NaiveTime, NaiveTime) {
        (
            parse_clock(&self.start).unwrap_or_else(|| parse_clock(DEFAULT_START).expect("default")),
            parse_clock(&self.end).unwrap_or_else(|| parse_clock(DEFAULT_END).expect("default")),
        )
    }
}

/// One of the shift's settings, validated by key and audited (SPEC §6).
pub fn set_shift_setting(app: &AppHandle, key: &str, value: &str) -> Result<()> {
    let value = value.trim();
    let stored = match key {
        ENABLED | IN_DEV_BUILD => match value {
            "1" | "true" | "on" => "1".to_string(),
            "0" | "false" | "off" => "0".to_string(),
            _ => bail!("{key} is on or off"),
        },
        START | END => parse_clock(value)
            .map(|t| t.format("%H:%M").to_string())
            .with_context(|| format!("{key} is a time like 21:00"))?,
        IDLE_MINUTES => value
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= MAX_IDLE_MINUTES)
            .map(|n| n.to_string())
            .with_context(|| format!("idle minutes is 0–{MAX_IDLE_MINUTES}"))?,
        GUIDES_PER_NIGHT | DIGESTS_PER_NIGHT => value
            .parse::<u32>()
            .ok()
            .filter(|n| *n <= MAX_CAP)
            .map(|n| n.to_string())
            .with_context(|| format!("a cap is 0–{MAX_CAP}"))?,
        _ => bail!("no shift setting called {key}"),
    };
    crate::settings::set_audited(app, key, &stored)?;
    emit_changed(app);
    Ok(())
}

// ---------------------------------------------------------------------------
// The window and the night (pure)

/// Whether `t` falls inside the window, which may cross midnight.
pub(crate) fn in_window(t: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start <= end {
        t >= start && t < end
    } else {
        t >= start || t < end
    }
}

/// The date a night is filed under: the day its window opened on. Inside a
/// window that crossed midnight, that is yesterday; outside the window it is
/// the coming night's date.
pub(crate) fn night_key(now: NaiveDateTime, start: NaiveTime, end: NaiveTime) -> NaiveDate {
    if start > end && now.time() < end {
        now.date().pred_opt().unwrap_or(now.date())
    } else {
        now.date()
    }
}

/// The night whose window closed last — what a launch checks for a run
/// (SPEC §6): the current key's predecessor, except after a same-day window
/// has closed, when it is the current key itself.
pub(crate) fn last_closed_night(now: NaiveDateTime, start: NaiveTime, end: NaiveTime) -> NaiveDate {
    let key = night_key(now, start, end);
    if start <= end && now.time() >= end {
        key
    } else {
        key.pred_opt().unwrap_or(key)
    }
}

/// Everything the scheduler's minute check weighs.
pub(crate) struct Conditions {
    pub enabled: bool,
    /// This process is the one that runs shifts (SPEC §6).
    pub runs_here: bool,
    pub paused: bool,
    pub in_window: bool,
    pub idle_secs: u64,
    pub idle_threshold_secs: u64,
    /// A run row exists for tonight, whoever started it.
    pub ran_tonight: bool,
}

pub(crate) fn should_start(c: &Conditions) -> bool {
    c.enabled
        && c.runs_here
        && !c.paused
        && c.in_window
        && c.idle_secs >= c.idle_threshold_secs
        && !c.ran_tonight
}

/// `HIDIdleTime` off `ioreg`'s listing, in seconds; the value is nanoseconds.
pub(crate) fn parse_idle(text: &str) -> Option<u64> {
    text.lines()
        .find(|line| line.contains("\"HIDIdleTime\""))
        .and_then(|line| line.rsplit('=').next())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|ns| ns / 1_000_000_000)
}

fn idle_seconds() -> Option<u64> {
    let child = Command::new("/usr/sbin/ioreg")
        .args(["-c", "IOHIDSystem", "-r", "-d", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = crate::jobs::wait_bounded(child, Duration::from_secs(10))?;
    parse_idle(&String::from_utf8_lossy(&output.stdout))
}

fn runs_here(conn: &Connection) -> bool {
    if cfg!(debug_assertions) {
        flag(conn, IN_DEV_BUILD, false)
    } else {
        true
    }
}

fn paused_on(conn: &Connection) -> Option<String> {
    setting(conn, PAUSED_ON).ok().flatten().filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// The run row

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub name: String,
    /// pending | running | done | skipped
    pub state: String,
    pub outcome: Option<String>,
}

fn fresh_steps() -> Vec<Step> {
    STEPS
        .iter()
        .map(|name| Step {
            name: name.to_string(),
            state: "pending".into(),
            outcome: None,
        })
        .collect()
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: i64,
    pub night: String,
    pub trigger: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub steps: Vec<Step>,
    pub jobs: Vec<i64>,
    pub stopped_by: Option<String>,
    pub summary: Option<String>,
    pub owner_pid: Option<i64>,
}

const RUN_COLUMNS: &str =
    "id, night, trigger, started_at, finished_at, steps, jobs, stopped_by, summary, owner_pid";

fn read_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunInfo> {
    let steps: String = row.get(5)?;
    let jobs: String = row.get(6)?;
    Ok(RunInfo {
        id: row.get(0)?,
        night: row.get(1)?,
        trigger: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        steps: serde_json::from_str(&steps).unwrap_or_default(),
        jobs: serde_json::from_str(&jobs).unwrap_or_default(),
        stopped_by: row.get(7)?,
        summary: row.get(8)?,
        owner_pid: row.get(9)?,
    })
}

fn latest_run(conn: &Connection) -> Result<Option<RunInfo>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM shift_runs ORDER BY id DESC LIMIT 1"),
            [],
            read_run,
        )
        .optional()?)
}

fn run_by_id(conn: &Connection, id: i64) -> Result<Option<RunInfo>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM shift_runs WHERE id = ?1"),
            [id],
            read_run,
        )
        .optional()?)
}

fn ran_on(conn: &Connection, night: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM shift_runs WHERE night = ?1",
        [night],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Inserts the night's run unless one exists, inside one IMMEDIATE
/// transaction: the write lock is what orders this process against the other
/// build on the same database, so two never both start a night's run.
pub(crate) fn insert_run(conn: &Connection, night: &str, trigger: &str) -> Result<Option<i64>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    if ran_on(&tx, night)? {
        return Ok(None);
    }
    tx.execute(
        "INSERT INTO shift_runs (night, trigger, started_at, steps, jobs, owner_pid)
         VALUES (?1, ?2, ?3, ?4, '[]', ?5)",
        params![
            night,
            trigger,
            now(),
            serde_json::to_string(&fresh_steps())?,
            i64::from(std::process::id())
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.commit()?;
    Ok(Some(id))
}

fn finish_run(conn: &Connection, id: i64, stopped_by: &str, summary: &str) -> Result<()> {
    conn.execute(
        "UPDATE shift_runs SET finished_at = ?1, stopped_by = ?2, summary = ?3
         WHERE id = ?4 AND finished_at IS NULL",
        params![now(), stopped_by, summary, id],
    )?;
    Ok(())
}

/// A run whose process is gone and that never finished is settled at launch
/// the way a job's row is (SPEC §6); it still counts as the night's run.
pub fn startup_recovery(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("SELECT id, owner_pid FROM shift_runs WHERE finished_at IS NULL")?;
    let open: Vec<(i64, Option<i64>)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, owner) in open {
        if crate::jobs::owner_gone(owner) {
            eprintln!("shift run {id}: its process is gone — settled as interrupted");
            finish_run(conn, id, "error", "the app quit before the run finished")?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// State and the scheduler

#[derive(Default)]
pub struct ShiftState {
    running: AtomicBool,
    run_id: Mutex<Option<i64>>,
    caffeinate: Mutex<Option<Child>>,
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(CHANGED_EVENT, ());
    crate::tray::refresh(app);
}

/// The minute check, and the catch-up at launch (SPEC §6).
pub fn start_scheduler(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        catch_up(&app);
        loop {
            std::thread::sleep(TICK);
            tick(&app);
        }
    });
}

/// A window that closed since the last run with no run row starts a run at
/// once. With no run on record nothing was missed: the first shift waits
/// for its window rather than starting at install.
fn catch_up(app: &AppHandle) {
    let decision = with_conn(app, |conn| {
        let s = settings(conn);
        if !s.enabled || !runs_here(conn) {
            return Ok(None);
        }
        let Some(latest) = latest_run(conn)? else {
            return Ok(None);
        };
        let (start, end) = s.window();
        let now_local = Local::now().naive_local();
        // Inside an open window the coming run covers the same backlog; a
        // catch-up beside it would spend two nights' caps in one evening.
        if in_window(now_local.time(), start, end) {
            return Ok(None);
        }
        let missed = last_closed_night(now_local, start, end).to_string();
        if missed <= latest.night || ran_on(conn, &missed)? {
            return Ok(None);
        }
        if paused_on(conn).as_deref() == Some(missed.as_str()) {
            eprintln!("shift: the {missed} window was missed and paused — not caught up");
            return Ok(None);
        }
        Ok(Some(missed))
    });
    match decision {
        Ok(Some(missed)) => match start(app, "launch", &missed) {
            Ok(id) => eprintln!("shift: catching up the {missed} window as run {id}"),
            Err(e) => eprintln!("shift: catch-up not started — {e:#}"),
        },
        Ok(None) => {}
        Err(e) => eprintln!("shift: catch-up check failed — {e:#}"),
    }
}

fn tick(app: &AppHandle) {
    let state = app.state::<ShiftState>();
    if state.running.load(Ordering::SeqCst) {
        return;
    }
    // The tray's next-meeting line follows the clock, on the hour: every
    // change of state refreshes it on its own, and a meeting passing is the
    // only thing a quiet hour changes.
    if Local::now().minute() == 0 {
        crate::tray::refresh(app);
    }
    let decision = with_conn(app, |conn| {
        let s = settings(conn);
        let (start, end) = s.window();
        let now_local = Local::now().naive_local();
        let night = night_key(now_local, start, end).to_string();
        let cheap = Conditions {
            enabled: s.enabled,
            runs_here: runs_here(conn),
            paused: paused_on(conn).as_deref() == Some(night.as_str()),
            in_window: in_window(now_local.time(), start, end),
            idle_secs: 0,
            idle_threshold_secs: u64::from(s.idle_minutes) * 60,
            ran_tonight: ran_on(conn, &night)?,
        };
        Ok((cheap, night))
    });
    let (mut conditions, night) = match decision {
        Ok(d) => d,
        Err(e) => {
            eprintln!("shift: the minute check failed — {e:#}");
            return;
        }
    };
    // `ioreg` only once everything else says go: the idle read spawns a
    // process, and most minutes fail an earlier condition.
    let Conditions {
        enabled,
        runs_here,
        paused,
        in_window,
        ran_tonight,
        ..
    } = conditions;
    if !(enabled && runs_here && !paused && in_window && !ran_tonight) {
        return;
    }
    conditions.idle_secs = match idle_seconds() {
        Some(secs) => secs,
        None => {
            eprintln!("shift: idle time unreadable — not starting");
            return;
        }
    };
    if should_start(&conditions) {
        match start(app, "idle", &night) {
            Ok(id) => eprintln!("shift: run {id} started ({night}, idle {} s)", conditions.idle_secs),
            Err(e) => eprintln!("shift: not started — {e:#}"),
        }
    }
}

/// `Run the shift now` (SPEC §6): trigger `manual`, once a night like the rest.
pub fn run_now(app: &AppHandle) -> Result<i64> {
    let night = with_conn(app, |conn| {
        let (start, end) = settings(conn).window();
        Ok(night_key(Local::now().naive_local(), start, end).to_string())
    })?;
    start(app, "manual", &night)
}

fn start(app: &AppHandle, trigger: &str, night: &str) -> Result<i64> {
    let state = app.state::<ShiftState>();
    if state.running.load(Ordering::SeqCst) {
        bail!("a shift is already running");
    }
    let id = with_conn(app, |conn| insert_run(conn, night, trigger))?;
    let Some(id) = id else {
        bail!("the shift already ran tonight ({night}); it runs once a night");
    };
    state.running.store(true, Ordering::SeqCst);
    *lock(&state.run_id) = Some(id);
    let app = app.clone();
    std::thread::spawn(move || run_shift(&app, id));
    Ok(id)
}

/// `Pause tonight`, and its undo: the night's key on the setting, which the
/// next night leaves behind on its own. A run under way stops between jobs.
pub fn pause_tonight(app: &AppHandle, paused: bool) -> Result<()> {
    // The night the pause is for: the run under way's, so a press after the
    // window closed still stops the run still working, else the coming one.
    let current = *lock(&app.state::<ShiftState>().run_id);
    let night = with_conn(app, |conn| {
        if let Some(run) = current.map(|id| run_by_id(conn, id)).transpose()?.flatten() {
            return Ok(run.night);
        }
        let (start, end) = settings(conn).window();
        Ok(night_key(Local::now().naive_local(), start, end).to_string())
    })?;
    crate::settings::set_audited(app, PAUSED_ON, if paused { &night } else { "" })?;
    emit_changed(app);
    Ok(())
}

/// The app is quitting: the run row says so, and `caffeinate` goes with it.
pub fn shutdown(app: &AppHandle) {
    let state = app.state::<ShiftState>();
    if let Some(id) = lock(&state.run_id).take() {
        let _ = with_conn(app, |conn| {
            finish_run(conn, id, "error", "the app quit before the run finished")
        });
    }
    release_awake(app);
}

// ---------------------------------------------------------------------------
// The run

fn hold_awake(app: &AppHandle) {
    let state = app.state::<ShiftState>();
    let spawned = Command::new("/usr/bin/caffeinate")
        .args(["-i", "-w", &std::process::id().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match spawned {
        Ok(child) => *lock(&state.caffeinate) = Some(child),
        Err(e) => eprintln!("shift: caffeinate not started — {e}"),
    }
}

fn release_awake(app: &AppHandle) {
    let state = app.state::<ShiftState>();
    let held = lock(&state.caffeinate).take();
    if let Some(mut child) = held {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Why a run ended, and what it says in the morning.
struct End {
    stopped_by: &'static str,
    summary: String,
}

fn run_shift(app: &AppHandle, id: i64) {
    struct Finished<'a>(&'a AppHandle);
    impl Drop for Finished<'_> {
        fn drop(&mut self) {
            let state = self.0.state::<ShiftState>();
            release_awake(self.0);
            *lock(&state.run_id) = None;
            state.running.store(false, Ordering::SeqCst);
            emit_changed(self.0);
        }
    }
    let _finished = Finished(app);
    hold_awake(app);
    emit_changed(app);

    let end = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_plan(app, id))) {
        Ok(Ok(end)) => end,
        Ok(Err(e)) => End {
            stopped_by: "error",
            summary: format!("stopped: {e:#}"),
        },
        Err(_) => End {
            stopped_by: "error",
            summary: "the run stopped unexpectedly".to_string(),
        },
    };
    if let Err(e) = with_conn(app, |conn| finish_run(conn, id, end.stopped_by, &end.summary)) {
        eprintln!("shift run {id}: could not be recorded as finished — {e:#}");
    }
    eprintln!("shift run {id}: {} ({})", end.summary, end.stopped_by);
    crate::notifications::notify(
        app,
        crate::settings::NOTIFY_SHIFT_FINISHED,
        "Shift finished",
        &end.summary,
    );
}

/// The run's record as it goes: each step's state and outcome, and the jobs
/// it ran, written to the row at every change so the panel can read it.
struct Progress<'a> {
    app: &'a AppHandle,
    id: i64,
    night: String,
    /// idle | launch | manual: an idle run ends with its window; the other
    /// two start outside it by design.
    trigger: String,
    steps: Vec<Step>,
    jobs: Vec<i64>,
    filed: usize,
    extracts: usize,
    distilled: usize,
    rebuilt: usize,
    failed: usize,
    /// Work the caps left for tomorrow.
    left: usize,
}

impl Progress<'_> {
    fn begin(&mut self, index: usize) {
        self.steps[index].state = "running".into();
        self.save();
    }

    fn done(&mut self, index: usize, outcome: impl Into<String>) {
        self.steps[index].state = "done".into();
        self.steps[index].outcome = Some(outcome.into());
        self.save();
    }

    fn skip(&mut self, index: usize, why: impl Into<String>) {
        self.steps[index].state = "skipped".into();
        self.steps[index].outcome = Some(why.into());
        self.save();
    }

    /// The run ends before its last step: every step not yet reached reads
    /// `skipped` with the reason, never `pending` on a finished run.
    fn finish_early(&mut self, end: End) -> End {
        let why = match end.stopped_by {
            "paused" => "not reached — paused",
            "rate_limit" => "not reached — the rate limit",
            "window" => "not reached — the window closed",
            _ => "not reached",
        };
        for step in &mut self.steps {
            if step.state == "pending" {
                step.state = "skipped".into();
                step.outcome = Some(why.into());
            }
        }
        self.save();
        end
    }

    fn save(&self) {
        let steps = serde_json::to_string(&self.steps).unwrap_or_else(|_| "[]".into());
        let jobs = serde_json::to_string(&self.jobs).unwrap_or_else(|_| "[]".into());
        let id = self.id;
        if let Err(e) = with_conn(self.app, |conn| {
            conn.execute(
                "UPDATE shift_runs SET steps = ?1, jobs = ?2 WHERE id = ?3",
                params![steps, jobs, id],
            )?;
            Ok(())
        }) {
            eprintln!("shift run {id}: progress not saved — {e:#}");
        }
        emit_changed(self.app);
    }

    /// Waits for a job the plan enqueued and records how it ended. A job
    /// that has not settled within `JOB_WAIT_LIMIT` is left to the runner's
    /// own watchdog and counted as failed here, so the run can end.
    fn wait(&mut self, job_id: i64) -> String {
        self.jobs.push(job_id);
        self.save();
        let status = wait_for_job(self.app, job_id, JOB_WAIT_LIMIT);
        if status == "failed" || status == "unsettled" {
            self.failed += 1;
        }
        status
    }

    /// Whether the run has to stop before the next job (SPEC §6): the pause,
    /// the rate limit, or — for a run the idle check started — the window's
    /// close, since the owner is back at the machine by then.
    fn stop_reason(&self) -> Option<End> {
        let (paused, window_closed) = with_conn(self.app, |conn| {
            let paused = paused_on(conn).is_some_and(|on| on == self.night);
            let (start, end) = settings(conn).window();
            let closed = self.trigger == "idle" && !in_window(Local::now().time(), start, end);
            Ok((paused, closed))
        })
        .unwrap_or((false, false));
        if paused {
            return Some(End {
                stopped_by: "paused",
                summary: format!("{} · paused before the rest", self.tally()),
            });
        }
        if window_closed {
            return Some(End {
                stopped_by: "window",
                summary: format!("{} · the window closed before the rest", self.tally()),
            });
        }
        crate::jobs::rate_limit_reached().map(|why| End {
            stopped_by: "rate_limit",
            summary: format!("{} · stopped at the rate limit, {why}", self.tally()),
        })
    }

    /// `2 sessions distilled, 1 guide rebuilt, 3 files filed`.
    fn tally(&self) -> String {
        let mut parts = Vec::new();
        if self.distilled > 0 {
            parts.push(plural(self.distilled, "session distilled", "sessions distilled"));
        }
        if self.rebuilt > 0 {
            parts.push(plural(self.rebuilt, "guide rebuilt", "guides rebuilt"));
        }
        if self.filed > 0 {
            parts.push(plural(self.filed, "file filed", "files filed"));
        }
        if self.extracts > 0 {
            parts.push(plural(self.extracts, "extract", "extracts"));
        }
        if self.failed > 0 {
            parts.push(plural(self.failed, "job failed", "jobs failed"));
        }
        if parts.is_empty() {
            "nothing was waiting".to_string()
        } else {
            parts.join(", ")
        }
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// How long a run waits on one job before leaving it to the runner. The
/// longest run measured is an 88-minute extract; the runner's own stall
/// watchdog covers a silent child, and this covers one that never stops
/// talking.
const JOB_WAIT_LIMIT: Duration = Duration::from_secs(3 * 60 * 60);

fn wait_for_job(app: &AppHandle, job_id: i64, limit: Duration) -> String {
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() >= limit {
            eprintln!("shift: job {job_id} has not settled in {} min — left to the runner", limit.as_secs() / 60);
            return "unsettled".into();
        }
        std::thread::sleep(JOB_POLL);
        let status = with_conn(app, |conn| {
            Ok(conn
                .query_row("SELECT status FROM jobs WHERE id = ?1", [job_id], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?)
        });
        match status {
            Ok(Some(s)) if s == "queued" || s == "running" => continue,
            Ok(Some(s)) => return s,
            Ok(None) => return "gone".into(),
            Err(e) => {
                eprintln!("shift: job {job_id} unreadable — {e:#}");
                return "unknown".into();
            }
        }
    }
}

fn run_plan(app: &AppHandle, id: i64) -> Result<End> {
    let (night, trigger, s) = with_conn(app, |conn| {
        let run = run_by_id(conn, id)?.context("the run row is gone")?;
        Ok((run.night, run.trigger, settings(conn)))
    })?;
    let mut p = Progress {
        app,
        id,
        night,
        trigger,
        steps: fresh_steps(),
        jobs: Vec::new(),
        filed: 0,
        extracts: 0,
        distilled: 0,
        rebuilt: 0,
        failed: 0,
        left: 0,
    };

    // 1. Sync Canvas, quietly; 2. what it filed.
    p.begin(SYNC);
    if !crate::canvas::remembered_session_kept() {
        p.done(SYNC, "no stored Canvas session — sign in from Settings to let the shift sync");
        p.skip(FILE, "not synced");
    } else {
        match crate::canvas_sync::sync_now(app) {
            Ok(end) => match (end.results, end.sign_in_needed, end.error) {
                (Some(results), _, _) => {
                    let deadlines: usize = results.iter().map(|r| r.deadlines_recorded).sum();
                    let announcements: usize =
                        results.iter().map(|r| r.announcements_recorded).sum();
                    let failed: Vec<&str> = results
                        .iter()
                        .filter(|r| r.error.is_some())
                        .map(|r| r.class_name.as_str())
                        .collect();
                    p.filed = results.iter().map(|r| r.files_filed).sum();
                    let mut outcome = format!(
                        "synced {} — {}, {}",
                        plural(results.len(), "class", "classes"),
                        plural(deadlines, "deadline recorded", "deadlines recorded"),
                        plural(announcements, "announcement", "announcements"),
                    );
                    if !failed.is_empty() {
                        outcome.push_str(&format!(" · failed for {}", failed.join(", ")));
                    }
                    p.begin(FILE);
                    p.done(SYNC, outcome);
                    let filed = p.filed;
                    p.done(
                        FILE,
                        if filed == 0 {
                            "nothing to file".to_string()
                        } else {
                            format!("{} where Canvas keeps them", plural(filed, "file filed", "files filed"))
                        },
                    );
                }
                (None, true, _) => {
                    p.done(SYNC, "Canvas wants a sign-in — not synced; the next press from Settings asks");
                    p.skip(FILE, "not synced");
                }
                (None, false, error) => {
                    p.done(SYNC, format!("not synced: {}", error.unwrap_or_default()));
                    p.skip(FILE, "not synced");
                }
            },
            Err(e) => {
                p.done(SYNC, format!("not started: {e:#}"));
                p.skip(FILE, "not synced");
            }
        }
    }
    if let Some(end) = p.stop_reason() {
        return Ok(p.finish_early(end));
    }

    // 3. Extract, class by class, waiting for each run.
    p.begin(EXTRACT);
    let class_ids: Vec<i64> = with_conn(app, |conn| {
        let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    })?;
    let mut extract_notes = Vec::new();
    for class_id in &class_ids {
        match crate::extract::run_pipeline(app, *class_id) {
            Ok(Some(job_id)) => {
                p.extracts += 1;
                let status = p.wait(job_id);
                if status != "succeeded" {
                    extract_notes.push(format!("job {job_id} {status}"));
                }
                if let Some(end) = p.stop_reason() {
                    p.done(EXTRACT, format!("stopped after job {job_id}"));
                    return Ok(p.finish_early(end));
                }
            }
            Ok(None) => {}
            Err(e) => extract_notes.push(format!("class {class_id}: {e:#}")),
        }
    }
    let extracts = p.extracts;
    p.done(
        EXTRACT,
        if extracts == 0 && extract_notes.is_empty() {
            "nothing stale".to_string()
        } else {
            let mut outcome = plural(extracts, "run", "runs");
            if !extract_notes.is_empty() {
                outcome.push_str(&format!(" · {}", extract_notes.join(", ")));
            }
            outcome
        },
    );

    // 4. Distill, oldest session first, under the cap.
    p.begin(DISTILL);
    let candidates = with_conn(app, |conn| digest_candidates(conn, now()))?;
    let (todo, left) = capped(candidates, s.digests_per_night);
    p.left += left;
    let mut digest_notes = Vec::new();
    for c in &todo {
        match crate::lectures::enqueue_digest(app, c.class_id, &c.rel_path, &c.date) {
            Ok(job_id) => {
                let status = p.wait(job_id);
                if status == "succeeded" {
                    p.distilled += 1;
                } else {
                    digest_notes.push(format!("{} {status}", c.date));
                }
            }
            Err(e) => digest_notes.push(format!("{}: {e:#}", c.date)),
        }
        if let Some(end) = p.stop_reason() {
            p.done(DISTILL, format!("stopped after {}", c.date));
            return Ok(p.finish_early(end));
        }
    }
    p.done(DISTILL, step_outcome(p.distilled, todo.len(), left, "session", "sessions", &digest_notes));

    // 5. Rebuild the division guides whose meeting has passed, oldest first.
    p.begin(REBUILD);
    let candidates = with_conn(app, |conn| guide_candidates(conn, Local::now().naive_local(), now()))?;
    let (todo, left) = capped(candidates, s.guides_per_night);
    p.left += left;
    let mut guide_notes = Vec::new();
    for c in &todo {
        let label = Local::now().format("%B %-d, %Y at %-I:%M %p").to_string();
        match crate::guides::synthesize_unit(app, c.class_id, c.unit_id, &label) {
            Ok(job_id) => {
                let status = p.wait(job_id);
                if status == "succeeded" {
                    p.rebuilt += 1;
                } else {
                    guide_notes.push(format!("{} {status}", c.unit_name));
                }
            }
            Err(e) => guide_notes.push(format!("{}: {e:#}", c.unit_name)),
        }
        if let Some(end) = p.stop_reason() {
            p.done(REBUILD, format!("stopped after {}", c.unit_name));
            return Ok(p.finish_early(end));
        }
    }
    p.done(REBUILD, step_outcome(p.rebuilt, todo.len(), left, "guide", "guides", &guide_notes));

    let summary = p.tally();
    Ok(if p.left > 0 {
        End {
            stopped_by: "budget",
            summary: format!("{summary} · {} more for tomorrow", p.left),
        }
    } else {
        End {
            stopped_by: "done",
            summary,
        }
    })
}

fn step_outcome(
    succeeded: usize,
    tried: usize,
    left: usize,
    one: &str,
    many: &str,
    notes: &[String],
) -> String {
    let mut outcome = match (tried, left) {
        (0, 0) => format!("no {many} waiting"),
        // A cap of zero: the work waits, and the cap is why.
        (0, left) => format!("{left} {} waiting, the cap is 0", if left == 1 { one } else { many }),
        (tried, _) => format!("{succeeded} of {tried} {}", if tried == 1 { one } else { many }),
    };
    if tried > 0 && left > 0 {
        outcome.push_str(&format!(" · {left} past the cap"));
    }
    if !notes.is_empty() {
        outcome.push_str(&format!(" · {}", notes.join(", ")));
    }
    outcome
}

/// The first `cap` of a list, and how many it left.
pub(crate) fn capped<T>(mut items: Vec<T>, cap: u32) -> (Vec<T>, usize) {
    let cap = cap as usize;
    let left = items.len().saturating_sub(cap);
    items.truncate(cap);
    (items, left)
}

// ---------------------------------------------------------------------------
// The plan's selection

/// A filed lecture the shift would distill: one without its note, or one a
/// digest has not read for what was flagged (SPEC §8.4).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DigestCandidate {
    pub class_id: i64,
    pub rel_path: String,
    /// The session date the file name opens with — the digest's `date`.
    pub date: String,
    pub reason: &'static str,
}

/// How long a candidate whose last job failed is left alone (SPEC §6), so a
/// lecture or a division that fails for a stable reason cannot hold a cap
/// night after night.
const FAILED_REST: i64 = 3 * 24 * 60 * 60;

/// Whether the newest job of this kind and scope failed within `FAILED_REST`
/// of `now`.
fn recently_failed(conn: &Connection, kind: &str, class_id: i64, scope: &str, now: i64) -> Result<bool> {
    let latest: Option<(String, Option<i64>)> = conn
        .query_row(
            "SELECT status, finished_at FROM jobs
             WHERE kind = ?1 AND class_id = ?2 AND scope = ?3
             ORDER BY id DESC LIMIT 1",
            params![kind, class_id, scope],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    Ok(latest.is_some_and(|(status, finished_at)| {
        status == "failed" && finished_at.is_some_and(|at| at > now - FAILED_REST)
    }))
}

/// Every applied contribution without a note, then every one whose ledger
/// was never read, each group oldest session first; one whose last digest
/// failed within `FAILED_REST` waits.
pub(crate) fn digest_candidates(conn: &Connection, now: i64) -> Result<Vec<DigestCandidate>> {
    let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
    let class_ids: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut no_note = Vec::new();
    let mut unread = Vec::new();
    for class_id in class_ids {
        for c in crate::lectures::list_contributions(conn, class_id)? {
            if recently_failed(conn, "lecture_digest", class_id, &c.rel_path, now)? {
                continue;
            }
            let date = crate::lectures::session_date(&c.rel_path);
            let candidate = |reason| DigestCandidate {
                class_id,
                rel_path: c.rel_path.clone(),
                date: date.clone(),
                reason,
            };
            if !c.distilled {
                no_note.push(candidate("no note"));
            } else if !c.hints_read {
                unread.push(candidate("not read for what was flagged"));
            }
        }
    }
    no_note.sort_by(|a, b| a.date.cmp(&b.date).then(a.rel_path.cmp(&b.rel_path)));
    unread.sort_by(|a, b| a.date.cmp(&b.date).then(a.rel_path.cmp(&b.rel_path)));
    no_note.extend(unread);
    Ok(no_note)
}

/// A division whose guide the shift would build: it has sources, its guide
/// is stale or absent, and its meeting has passed.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct GuideCandidate {
    pub class_id: i64,
    pub unit_id: i64,
    pub unit_name: String,
    /// When the division's meeting ended — the order the list is built in.
    pub meeting_end: NaiveDateTime,
    pub stale: bool,
}

/// A class's meetings as `(weekday 1=Mon..7=Sun, end time)`.
fn meeting_ends(conn: &Connection, class_id: i64) -> Result<Vec<(u32, NaiveTime)>> {
    let mut stmt =
        conn.prepare("SELECT weekday, end_time FROM meetings WHERE class_id = ?1 ORDER BY weekday")?;
    let rows = stmt
        .query_map([class_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(weekday, end)| {
            let weekday = u32::try_from(weekday).ok().filter(|d| (1..=7).contains(d))?;
            Some((weekday, parse_clock(&end)?))
        })
        .collect())
}

/// When a division's meeting ends (SPEC §6): for a dated division, the
/// class's first meeting on or after its start date; for an undated one — a
/// Part over a week range — this calendar week's meeting. `None` where the
/// course declares no meeting to measure by, so nothing is built on a guess.
pub(crate) fn meeting_end(
    starts_on: Option<&str>,
    meetings: &[(u32, NaiveTime)],
    now: NaiveDateTime,
) -> Option<NaiveDateTime> {
    let anchor = match starts_on.and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()) {
        Some(date) => date,
        // Monday of the current week.
        None => now
            .date()
            .checked_sub_days(Days::new(u64::from(now.weekday().number_from_monday() - 1)))?,
    };
    meetings
        .iter()
        .filter_map(|(weekday, end)| {
            let ahead = (weekday + 7 - anchor.weekday().number_from_monday()) % 7;
            Some(anchor.checked_add_days(Days::new(u64::from(ahead)))?.and_time(*end))
        })
        .min()
}

/// Every division with sources whose guide is stale or absent and whose
/// meeting has passed, oldest meeting first. Never the master, never a folder
/// guide: neither has a meeting.
pub(crate) fn guide_candidates(
    conn: &Connection,
    now: NaiveDateTime,
    now_secs: i64,
) -> Result<Vec<GuideCandidate>> {
    let mut stmt = conn.prepare("SELECT id FROM classes ORDER BY id")?;
    let class_ids: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for class_id in class_ids {
        let meetings = meeting_ends(conn, class_id)?;
        let guides = crate::guides::unit_guides(conn, class_id)?;
        for unit in crate::units::list_units(conn, class_id)? {
            let has_sources = unit.materials.unwrap_or(0) > 0
                || !crate::lectures::corpus_notes(conn, class_id, unit.id)?.is_empty();
            if !has_sources {
                continue;
            }
            let scope = crate::db::unit_scope(unit.id);
            if recently_failed(conn, "module_guide", class_id, &scope, now_secs)? {
                continue;
            }
            let stale = match guides.iter().find(|g| g.scope == scope) {
                Some(guide) if !guide.stale => continue,
                Some(_) => true,
                None => false,
            };
            let Some(end) = meeting_end(unit.starts_on.as_deref(), &meetings, now) else {
                continue;
            };
            if end > now {
                continue;
            }
            out.push(GuideCandidate {
                class_id,
                unit_id: unit.id,
                unit_name: unit.name,
                meeting_end: end,
                stale,
            });
        }
    }
    out.sort_by(|a, b| a.meeting_end.cmp(&b.meeting_end).then(a.unit_id.cmp(&b.unit_id)));
    Ok(out)
}

// ---------------------------------------------------------------------------
// What the panel, the tray and the notifications read

/// This week's synthesis in counts and minutes (SPEC §12). Never dollars: the
/// CLI's cost figure is notional under subscription auth.
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Meter {
    pub digests: i64,
    pub guides: i64,
    pub exams: i64,
    pub extracts: i64,
    pub minutes: i64,
}

pub fn meter(conn: &Connection, since: i64) -> Result<Meter> {
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*), COALESCE(SUM(finished_at - started_at), 0) FROM jobs
         WHERE started_at >= ?1 AND finished_at IS NOT NULL AND status = 'succeeded'
           AND kind IN ('lecture_digest', 'module_guide', 'master_guide', 'practice', 'extract')
         GROUP BY kind",
    )?;
    let mut meter = Meter::default();
    for row in stmt.query_map([since], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
    })? {
        let (kind, count, seconds) = row?;
        match kind.as_str() {
            "lecture_digest" => meter.digests += count,
            "module_guide" | "master_guide" => meter.guides += count,
            "practice" => meter.exams += count,
            "extract" => meter.extracts += count,
            _ => {}
        }
        meter.minutes += seconds / 60;
    }
    Ok(meter)
}

/// Monday 00:00 of the current local week, unix seconds.
fn week_start() -> i64 {
    let now = Local::now();
    let monday = now
        .date_naive()
        .checked_sub_days(Days::new(u64::from(now.weekday().number_from_monday() - 1)))
        .unwrap_or_else(|| now.date_naive());
    monday
        .and_hms_opt(0, 0, 0)
        .and_then(|dt| dt.and_local_timezone(Local).single())
        .map_or(0, |dt| dt.timestamp())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShiftStatus {
    pub running: bool,
    /// The run under way, else the last one.
    pub run: Option<RunInfo>,
    pub meter: Meter,
    pub settings: ShiftSettings,
    /// Tonight's key, whether it is paused, whether it already ran, and
    /// whether this process runs shifts at all.
    pub night: String,
    pub paused: bool,
    pub ran_tonight: bool,
    pub runs_here: bool,
    pub dev_build: bool,
}

pub fn status(app: &AppHandle) -> Result<ShiftStatus> {
    let state = app.state::<ShiftState>();
    let running = state.running.load(Ordering::SeqCst);
    let current = *lock(&state.run_id);
    with_conn(app, |conn| {
        let s = settings(conn);
        let (start, end) = s.window();
        let night = night_key(Local::now().naive_local(), start, end).to_string();
        let run = match current {
            Some(id) => run_by_id(conn, id)?,
            None => latest_run(conn)?,
        };
        Ok(ShiftStatus {
            running,
            run,
            meter: meter(conn, week_start())?,
            paused: paused_on(conn).as_deref() == Some(night.as_str()),
            ran_tonight: ran_on(conn, &night)?,
            runs_here: runs_here(conn),
            dev_build: cfg!(debug_assertions),
            settings: s,
            night,
        })
    })
}

/// What the tray shows (SPEC §12).
#[derive(Clone, Debug)]
pub struct TraySummary {
    pub next_meeting: String,
    pub shift: String,
    pub paused: bool,
    /// Whether `Run the shift now` can start one: not while one runs, and
    /// not once the night has had its run.
    pub can_run: bool,
    /// Beside the icon while a run is under way: `2/5`.
    pub title: Option<String>,
}

pub fn tray_summary(app: &AppHandle) -> TraySummary {
    let state = app.state::<ShiftState>();
    let running = state.running.load(Ordering::SeqCst);
    let current = *lock(&state.run_id);
    let now = Local::now();
    with_conn(app, |conn| {
        let s = settings(conn);
        let (start, end) = s.window();
        let night = night_key(now.naive_local(), start, end).to_string();
        let paused = paused_on(conn).as_deref() == Some(night.as_str());
        let run = match current {
            Some(id) => run_by_id(conn, id)?,
            None => None,
        };
        let step = run.as_ref().and_then(|r| {
            r.steps
                .iter()
                .position(|s| s.state == "running")
                .map(|i| (i + 1, r.steps.len()))
        });
        let shift = if let Some((i, of)) = step {
            format!("Shift running · step {i} of {of}")
        } else if running {
            "Shift running".to_string()
        } else if !runs_here(conn) {
            "Shift runs in the installed app".to_string()
        } else if !s.enabled {
            "Shift off".to_string()
        } else if paused {
            "Shift paused tonight".to_string()
        } else if ran_on(conn, &night)? {
            match latest_run(conn)?.and_then(|r| r.summary) {
                Some(summary) => format!("Shift ran tonight · {summary}"),
                None => "Shift ran tonight".to_string(),
            }
        } else {
            format!("Shift tonight at {}", clock_label(start))
        };
        Ok(TraySummary {
            next_meeting: next_meeting(conn, now.naive_local())?,
            shift,
            paused,
            can_run: !running && !ran_on(conn, &night)?,
            title: step.map(|(i, of)| format!("{i}/{of}")),
        })
    })
    .unwrap_or_else(|e| TraySummary {
        next_meeting: format!("ClassHub — {e:#}"),
        shift: "Shift".to_string(),
        paused: false,
        can_run: false,
        title: None,
    })
}

/// `11:45 am`, the app's own time register.
fn clock_label(t: NaiveTime) -> String {
    t.format("%-I:%M %p").to_string().to_lowercase()
}

/// `Next class · Thu 11:45 am · Biostatistics for AI`, the soonest meeting
/// across the classes from now.
fn next_meeting(conn: &Connection, now: NaiveDateTime) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT c.display_name, m.weekday, m.start_time FROM meetings m
         JOIN classes c ON c.id = m.class_id",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, String>(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let soonest = rows
        .into_iter()
        .filter_map(|(name, weekday, start)| {
            let weekday = u32::try_from(weekday).ok().filter(|d| (1..=7).contains(d))?;
            let start = parse_clock(&start)?;
            let mut ahead = (weekday + 7 - now.weekday().number_from_monday()) % 7;
            if ahead == 0 && start <= now.time() {
                ahead = 7;
            }
            let at = now.date().checked_add_days(Days::new(u64::from(ahead)))?.and_time(start);
            Some((at, name))
        })
        .min();
    Ok(match soonest {
        Some((at, name)) => format!(
            "Next class · {} {} · {name}",
            at.format("%a"),
            clock_label(at.time())
        ),
        None => "No classes scheduled".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        capped, digest_candidates, finish_run, guide_candidates, in_window, insert_run,
        last_closed_night, meeting_end, meter, next_meeting, night_key, parse_idle,
        should_start, startup_recovery, step_outcome, Conditions,
    };
    use crate::db::{memory_db, set_setting};
    use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
    use rusqlite::{params, Connection};

    /// A scratch tree that removes itself, the sync tests' shape.
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

    /// The week's meter counts succeeded synthesis by kind, in minutes, from
    /// `since` on; a failed run and an older one do not count.
    #[test]
    fn the_meter_counts_the_weeks_synthesis_in_minutes() {
        let conn = memory_db();
        let since = 1_000_000;
        for (kind, status, started, finished) in [
            ("lecture_digest", "succeeded", since + 10, since + 10 + 13 * 60),
            ("module_guide", "succeeded", since + 20, since + 20 + 23 * 60),
            ("master_guide", "succeeded", since + 30, since + 30 + 32 * 60),
            ("practice", "succeeded", since + 40, since + 40 + 10 * 60),
            ("extract", "succeeded", since + 50, since + 50 + 2 * 60),
            ("module_guide", "failed", since + 60, since + 60 + 5 * 60),
            ("lecture_digest", "succeeded", since - 100, since - 100 + 12 * 60),
        ] {
            conn.execute(
                "INSERT INTO jobs (kind, class_id, status, created_at, started_at, finished_at)
                 VALUES (?1, 1, ?2, ?3, ?3, ?4)",
                params![kind, status, started, finished],
            )
            .unwrap();
        }
        let m = meter(&conn, since).unwrap();
        assert_eq!((m.digests, m.guides, m.exams, m.extracts, m.minutes), (1, 2, 1, 1, 80));
    }

    /// A step's line, cap and failures included.
    #[test]
    fn a_steps_outcome_names_the_count_the_cap_and_the_failures() {
        assert_eq!(step_outcome(0, 0, 0, "session", "sessions", &[]), "no sessions waiting");
        assert_eq!(step_outcome(0, 0, 2, "session", "sessions", &[]), "2 sessions waiting, the cap is 0");
        assert_eq!(step_outcome(0, 0, 1, "guide", "guides", &[]), "1 guide waiting, the cap is 0");
        assert_eq!(step_outcome(1, 1, 0, "session", "sessions", &[]), "1 of 1 session");
        assert_eq!(
            step_outcome(1, 2, 3, "guide", "guides", &["Week 3 failed".to_string()]),
            "1 of 2 guides · 3 past the cap · Week 3 failed"
        );
    }

    /// The tray's next meeting across the seeded classes: from a Tuesday at
    /// noon it is Applied's 11:45 next Tuesday no longer — Fundamentals meets
    /// at 4:05 pm the same day — and from a Friday it is Tuesday's Applied.
    #[test]
    fn the_next_meeting_is_the_soonest_across_the_classes() {
        let conn = memory_db();
        assert_eq!(
            next_meeting(&conn, at("2026-09-08 12:00")).unwrap(),
            "Next class · Tue 4:05 pm · Fundamentals of AI in Medicine I"
        );
        assert_eq!(
            next_meeting(&conn, at("2026-09-11 09:00")).unwrap(),
            "Next class · Tue 11:45 am · Applied Generative AI in Medicine"
        );
        conn.execute("DELETE FROM meetings", []).unwrap();
        assert_eq!(next_meeting(&conn, at("2026-09-11 09:00")).unwrap(), "No classes scheduled");
    }

    /// A run finishes once; a run whose process is gone is settled at launch
    /// as interrupted, and one this process owns is left alone.
    #[test]
    fn a_run_finishes_once_and_an_orphan_is_settled_at_launch() {
        let conn = memory_db();
        let mine = insert_run(&conn, "2026-09-08", "idle").unwrap().unwrap();
        finish_run(&conn, mine, "done", "1 session distilled").unwrap();
        finish_run(&conn, mine, "error", "again").unwrap();
        let (stopped, summary): (String, String) = conn
            .query_row("SELECT stopped_by, summary FROM shift_runs WHERE id = ?1", [mine], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((stopped.as_str(), summary.as_str()), ("done", "1 session distilled"));
        let orphan = insert_run(&conn, "2026-09-07", "idle").unwrap().unwrap();
        conn.execute("UPDATE shift_runs SET owner_pid = 999999999 WHERE id = ?1", [orphan]).unwrap();
        let live = insert_run(&conn, "2026-09-06", "manual").unwrap().unwrap();
        // launchd: alive, and not this process, whose own rows a launch
        // cannot have.
        conn.execute("UPDATE shift_runs SET owner_pid = 1 WHERE id = ?1", [live]).unwrap();
        startup_recovery(&conn).unwrap();
        let settled: Option<String> = conn
            .query_row("SELECT stopped_by FROM shift_runs WHERE id = ?1", [orphan], |r| r.get(0))
            .unwrap();
        assert_eq!(settled.as_deref(), Some("error"));
        let untouched: Option<String> = conn
            .query_row("SELECT stopped_by FROM shift_runs WHERE id = ?1", [live], |r| r.get(0))
            .unwrap();
        assert_eq!(untouched, None, "a live process's run is left alone");
    }

    /// A lecture whose last digest failed within three days waits; one whose
    /// failure is older, or whose last run succeeded, is listed.
    #[test]
    fn a_candidate_that_failed_lately_waits() {
        let conn = memory_db();
        set_setting(&conn, "aibhs_root", "/nonexistent/classhub-shift-rest").unwrap();
        let unit = week_unit(&conn, 1, 2, "Week 2 — Ethics", Some("2026-09-01"));
        for (rel, failed_at) in [
            ("Weeks/Week 02 — Ethics/2026-09-01 — Lecture.md", Some(1_000_000 - 3_600)),
            ("Weeks/Week 02 — Ethics/2026-09-02 — Lecture.md", Some(1_000_000 - 4 * 24 * 3_600)),
            ("Weeks/Week 02 — Ethics/2026-09-03 — Lecture.md", None),
        ] {
            conn.execute(
                "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                    start_line, end_line, corpus_rel_path, summary, confidence, status, created_at)
                 VALUES (1, ?1, ?2, 0, 1, 1, 2, ?3, '', 'high', 'applied', 1)",
                params![unit, rel, format!(".classhub/corpus/Week 2 — Ethics/{}", rel.rsplit('/').next().unwrap())],
            )
            .unwrap();
            if let Some(at) = failed_at {
                conn.execute(
                    "INSERT INTO jobs (kind, class_id, scope, status, created_at, started_at, finished_at)
                     VALUES ('lecture_digest', 1, ?1, 'failed', ?2, ?2, ?2)",
                    params![rel, at],
                )
                .unwrap();
            }
        }
        let listed: Vec<String> = digest_candidates(&conn, 1_000_000)
            .unwrap()
            .into_iter()
            .map(|c| c.date)
            .collect();
        assert_eq!(listed, vec!["2026-09-02".to_string(), "2026-09-03".to_string()]);
    }

    fn t(s: &str) -> NaiveTime {
        NaiveTime::parse_from_str(s, "%H:%M").unwrap()
    }

    fn at(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The window crosses midnight, and the night is filed under the day it
    /// opened on (SPEC §6) — a run at 02:00 is last night's, not a second one.
    #[test]
    fn the_window_and_the_night_cross_midnight() {
        let (start, end) = (t("21:00"), t("06:00"));
        assert!(in_window(t("21:00"), start, end));
        assert!(in_window(t("23:59"), start, end));
        assert!(in_window(t("02:00"), start, end));
        assert!(!in_window(t("06:00"), start, end));
        assert!(!in_window(t("13:00"), start, end));
        assert_eq!(night_key(at("2026-09-08 23:00"), start, end), d("2026-09-08"));
        assert_eq!(night_key(at("2026-09-09 02:00"), start, end), d("2026-09-08"));
        assert_eq!(night_key(at("2026-09-09 07:00"), start, end), d("2026-09-09"), "the coming night");
        assert_eq!(night_key(at("2026-09-09 15:00"), start, end), d("2026-09-09"));
        // The window a launch checks for a run.
        assert_eq!(last_closed_night(at("2026-09-09 07:00"), start, end), d("2026-09-08"));
        assert_eq!(last_closed_night(at("2026-09-09 23:00"), start, end), d("2026-09-08"));
        assert_eq!(last_closed_night(at("2026-09-10 02:00"), start, end), d("2026-09-08"));
        // A window inside one day.
        let (start, end) = (t("13:00"), t("15:00"));
        assert!(in_window(t("14:00"), start, end));
        assert!(!in_window(t("15:00"), start, end));
        assert_eq!(night_key(at("2026-09-08 14:00"), start, end), d("2026-09-08"));
        assert_eq!(last_closed_night(at("2026-09-08 16:00"), start, end), d("2026-09-08"));
        assert_eq!(last_closed_night(at("2026-09-08 12:00"), start, end), d("2026-09-07"));
    }

    /// Every condition of the minute check, each one alone enough to hold
    /// the shift back (SPEC §6).
    #[test]
    fn the_decision_needs_every_condition() {
        let go = Conditions {
            enabled: true,
            runs_here: true,
            paused: false,
            in_window: true,
            idle_secs: 1_200,
            idle_threshold_secs: 1_200,
            ran_tonight: false,
        };
        assert!(should_start(&go));
        assert!(!should_start(&Conditions { enabled: false, ..go }));
        assert!(!should_start(&Conditions { runs_here: false, ..go }), "the other build's");
        assert!(!should_start(&Conditions { paused: true, ..go }));
        assert!(!should_start(&Conditions { in_window: false, ..go }));
        assert!(!should_start(&Conditions { idle_secs: 1_199, ..go }));
        assert!(!should_start(&Conditions { ran_tonight: true, ..go }), "once a night");
        assert!(should_start(&Conditions { idle_secs: 0, idle_threshold_secs: 0, ..go }));
    }

    /// One run a night across two processes: the second insert for the same
    /// night answers with nothing.
    #[test]
    fn a_night_takes_one_run() {
        let conn = memory_db();
        assert!(insert_run(&conn, "2026-09-08", "idle").unwrap().is_some());
        assert!(insert_run(&conn, "2026-09-08", "manual").unwrap().is_none());
        assert!(insert_run(&conn, "2026-09-09", "idle").unwrap().is_some());
    }

    #[test]
    fn idle_time_is_read_off_ioreg_in_nanoseconds() {
        let text = "  | {\n  |   \"HIDIdleTime\" = 240022790625\n  |   \"HIDParameters\" = {}\n";
        assert_eq!(parse_idle(text), Some(240));
        assert_eq!(parse_idle("nothing here"), None);
    }

    #[test]
    fn the_caps_hold() {
        let (taken, left) = capped(vec![1, 2, 3], 2);
        assert_eq!((taken, left), (vec![1, 2], 1));
        let (taken, left) = capped(vec![1], 4);
        assert_eq!((taken, left), (vec![1], 0));
        let (taken, left) = capped(vec![1, 2], 0);
        assert_eq!((taken, left), (Vec::<i32>::new(), 2));
    }

    fn week_unit(conn: &Connection, class_id: i64, number: i64, name: &str, starts_on: Option<&str>) -> i64 {
        conn.execute(
            "INSERT INTO units (class_id, ordinal, kind, name, number, starts_on, source)
             VALUES (?1, ?2, 'week', ?3, ?2, ?4, 'syllabus')",
            params![class_id, number, name, starts_on],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn file(conn: &Connection, class_id: i64, rel_path: &str) {
        conn.execute(
            "INSERT INTO files (class_id, rel_path, sha256, size, mtime, kind)
             VALUES (?1, ?2, ?2, 1, 1, 'pdf')",
            params![class_id, rel_path],
        )
        .unwrap();
    }

    /// The distill list (SPEC §6): a contribution without its note first, then
    /// one whose ledger was never read, oldest first; a read one is not listed.
    #[test]
    fn the_distill_list_is_the_notes_missing_then_the_ledgers_unread() {
        let conn = memory_db();
        let scratch = Scratch::new("shift");
        let root = scratch.0.clone();
        set_setting(&conn, "aibhs_root", &root.to_string_lossy()).unwrap();
        let unit = week_unit(&conn, 1, 2, "Week 2 — Ethics", Some("2026-09-01"));
        let folder = root.join("Fundamentals of Artificial Intelligence in Medicine I");
        let note_dir = folder.join(".classhub/corpus/Week 2 — Ethics");
        std::fs::create_dir_all(&note_dir).unwrap();
        for (rel, note, read) in [
            ("Weeks/Week 02 — Ethics/2026-09-01 — Lecture.md", "2026-09-01 — Lecture.md", None::<i64>),
            ("Weeks/Week 02 — Ethics/2026-08-25 — Lecture.md", "2026-08-25 — Lecture.md", None),
            ("Weeks/Week 02 — Ethics/2026-09-02 — Lecture.md", "2026-09-02 — Lecture.md", Some(7)),
            ("Weeks/Week 02 — Ethics/2026-08-30 — Lecture.md", "2026-08-30 — Lecture.md", None),
        ] {
            conn.execute(
                "INSERT INTO lecture_contributions (class_id, unit_id, rel_path, start_ms, end_ms,
                    start_line, end_line, corpus_rel_path, summary, confidence, status, created_at,
                    hints_read_at)
                 VALUES (1, ?1, ?2, 0, 1, 1, 2, ?3, '', 'high', 'applied', 1, ?4)",
                params![unit, rel, format!(".classhub/corpus/Week 2 — Ethics/{note}"), read],
            )
            .unwrap();
        }
        // Two notes on disk, one never read; the Aug 30 lecture has no note.
        std::fs::write(note_dir.join("2026-09-01 — Lecture.md"), "note").unwrap();
        std::fs::write(note_dir.join("2026-08-25 — Lecture.md"), "note").unwrap();
        std::fs::write(note_dir.join("2026-09-02 — Lecture.md"), "note").unwrap();
        let listed: Vec<(String, &str)> = digest_candidates(&conn, 1_000_000)
            .unwrap()
            .into_iter()
            .map(|c| (c.date, c.reason))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("2026-08-30".to_string(), "no note"),
                ("2026-08-25".to_string(), "not read for what was flagged"),
                ("2026-09-01".to_string(), "not read for what was flagged"),
            ]
        );
    }

    /// A division's meeting (SPEC §6): the class's first meeting on or after
    /// a dated division's start, this week's for an undated Part.
    #[test]
    fn a_divisions_meeting_is_the_first_on_or_after_its_start() {
        // Tuesday 16:05–19:05, as Fundamentals meets.
        let meetings = vec![(2, t("19:05"))];
        assert_eq!(
            meeting_end(Some("2026-09-08"), &meetings, at("2026-09-08 21:00")),
            Some(at("2026-09-08 19:05")),
            "starts on the meeting day"
        );
        assert_eq!(
            meeting_end(Some("2026-09-06"), &meetings, at("2026-09-08 21:00")),
            Some(at("2026-09-08 19:05")),
            "starts on a Sunday, meets the Tuesday after"
        );
        // Undated: this calendar week's Tuesday, whichever day it is.
        assert_eq!(
            meeting_end(None, &meetings, at("2026-09-10 21:00")),
            Some(at("2026-09-08 19:05"))
        );
        assert_eq!(
            meeting_end(None, &meetings, at("2026-09-07 21:00")),
            Some(at("2026-09-08 19:05")),
            "on Monday the week's meeting is still ahead"
        );
        assert_eq!(meeting_end(Some("2026-09-08"), &[], at("2026-09-08 21:00")), None);
    }

    /// The rebuild list (SPEC §6): a division with sources and no guide whose
    /// meeting has passed is listed; one whose meeting is tomorrow is not; a
    /// division with a fresh guide is not; the master never; and the caps hold.
    #[test]
    fn the_rebuild_list_is_the_divisions_with_sources_whose_meeting_has_passed() {
        let conn = memory_db();
        set_setting(&conn, "aibhs_root", "/nonexistent/classhub-shift-test").unwrap();
        // Fundamentals (class 1) meets Tuesday. Week 1 met Sept 1, Week 2
        // meets Sept 15; both hold a deck under their week folder.
        let week1 = week_unit(&conn, 1, 1, "Week 1 — Intro", Some("2026-09-01"));
        let week2 = week_unit(&conn, 1, 2, "Week 2 — Data", Some("2026-09-15"));
        let week3 = week_unit(&conn, 1, 3, "Week 3 — Models", Some("2026-08-25"));
        let _empty = week_unit(&conn, 1, 4, "Week 4 — Nothing filed", Some("2026-08-18"));
        file(&conn, 1, "Weeks/Week 01 — Intro/deck.pdf");
        file(&conn, 1, "Weeks/Week 02 — Data/deck.pdf");
        file(&conn, 1, "Weeks/Week 03 — Models/deck.pdf");
        // Week 3's guide is fresh over its one deck; the master is stale.
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (1, ?1, 'Study Guides/Week 3.html', 1, ?2)",
            params![
                crate::db::unit_scope(week3),
                r#"[{"relPath":"Weeks/Week 03 — Models/deck.pdf","sha256":"Weeks/Week 03 — Models/deck.pdf"}]"#
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO guides (class_id, scope, rel_path, generated_at, source_manifest)
             VALUES (1, 'master', 'Study Guides/Semester Master.html', 1, '[]')",
            [],
        )
        .unwrap();
        let listed: Vec<(i64, bool)> = guide_candidates(&conn, at("2026-09-08 21:00"), 1_000_000)
            .unwrap()
            .into_iter()
            .map(|c| (c.unit_id, c.stale))
            .collect();
        assert_eq!(listed, vec![(week1, false)], "week 2 is ahead, week 3 fresh, week 4 empty, the master never");
        // Week 3's deck changes: its guide is stale and it is listed first, being older.
        conn.execute(
            "UPDATE files SET sha256 = 'changed' WHERE rel_path = 'Weeks/Week 03 — Models/deck.pdf'",
            [],
        )
        .unwrap();
        let listed: Vec<(i64, bool)> = guide_candidates(&conn, at("2026-09-08 21:00"), 1_000_000)
            .unwrap()
            .into_iter()
            .map(|c| (c.unit_id, c.stale))
            .collect();
        assert_eq!(listed, vec![(week3, true), (week1, false)]);
        let (taken, left) = capped(listed, 1);
        assert_eq!((taken.len(), left), (1, 1));
        let _ = week2;
    }
}
