//! SPEC §6 — the Claude Code job runner: the single gateway to the subscription.
//!
//! Queue of max 2 concurrent jobs, each spawning `~/.local/bin/claude -p` with the
//! auth env vars stripped (SPEC §1), raw stream-json persisted to a log file, and
//! condensed progress forwarded to the frontend via `job://{id}/progress` events.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{lock, now, truncate, with_conn};

/// SPEC §6: never allow these, regardless of user-level claude settings. The
/// `--allowedTools` list alone does not restrict tools the user's own config
/// permits (verified against claude 2.1.237), so deny rules are passed too.
///
/// `Task` belongs here for the same reason it belongs in the read-only list: a
/// spawned sub-agent is a path around the parent's tool scoping, so leaving it
/// available to the write-capable kinds would hand a prompt-injected synthesis
/// run the one tool this list exists to withhold.
const DISALLOWED_TOOLS: &str = "Bash,WebFetch,WebSearch,Task";
/// Kinds that only propose (sort/syllabus) must not be able to touch the tree
/// at all — the additive-allowedTools behavior above applies to the write
/// tools just the same, so read-only is only real if they are denied.
///
/// This deny list IS the security boundary for read-only kinds (the allow
/// list does not restrict, see above), and a deny list only stops names it
/// enumerates — when the CLI grows a new write-capable or delegating tool,
/// its name must be added here.
const READ_ONLY_DISALLOWED: &str =
    "Bash,WebFetch,WebSearch,Write,Edit,MultiEdit,NotebookEdit,Task";

fn disallowed_tools(kind: &str) -> &'static str {
    match kind {
        "sort_proposal" | "syllabus_scan" => READ_ONLY_DISALLOWED,
        _ => DISALLOWED_TOOLS,
    }
}

const SELF_CHECK_PROMPT: &str = "Reply with exactly: OK";

/// How long a spawned run may produce nothing before it is presumed wedged.
const STALL_LIMIT: Duration = Duration::from_secs(10 * 60);

/// Raw job logs older than this are removed at startup. They are a debugging
/// aid for a run that already finished, and `master_guide` writes a large one
/// every time; without a sweep the folder only ever grows.
const LOG_RETENTION: Duration = Duration::from_secs(14 * 24 * 60 * 60);

/// Says once, in the run's own progress stream, that the raw log is short —
/// the job itself is unaffected and keeps going.
fn log_write_failed(app: &AppHandle, job: &QueuedJob, reported: &mut bool, e: &std::io::Error) {
    if *reported {
        return;
    }
    *reported = true;
    push_event(
        app,
        job.id,
        "status",
        format!("raw log could not be written ({e}) — progress here is unaffected"),
    );
}

// ---------------------------------------------------------------------------
// Types

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub seq: u64,
    /// status | text | tool | tool_result | retry | result | error | phase
    pub kind: String,
    pub text: String,
    /// Full tool-result text (truncated); the UI shows it behind a disclosure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthCheck {
    /// pending | ok | failed
    pub status: String,
    pub detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: i64,
    pub kind: String,
    pub class_id: Option<i64>,
    pub class_name: Option<String>,
    pub class_color: Option<String>,
    pub scope: Option<String>,
    pub status: String,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    pub summary: Option<String>,
    pub log_path: Option<String>,
    pub session_id: Option<String>,
}

struct QueuedJob {
    id: i64,
    kind: String,
    class_id: Option<i64>,
    prompt: String,
    /// Kind-specific completion data (e.g. the extract batch manifest). Also
    /// persisted to jobs.payload so a failed master_guide can be resumed after
    /// an app restart (startup_recovery fails interrupted jobs but keeps rows).
    payload: Option<String>,
    /// SPEC §6 resumability: when set, spawn with `--resume <session_id>` to
    /// continue a failed run's claude session instead of starting fresh.
    resume_session: Option<String>,
}

struct RunningJob {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
    /// SPEC §8.2: a master_guide runs exclusively — while it runs, pump starts
    /// nothing else.
    exclusive: bool,
}

#[derive(Default)]
struct JobOutput {
    events: Vec<ProgressEvent>,
    next_seq: u64,
}

struct Inner {
    queue: VecDeque<QueuedJob>,
    running: HashMap<i64, RunningJob>,
    /// Condensed progress buffers for jobs run this session (snapshot for late subscribers).
    output: HashMap<i64, JobOutput>,
    /// Rolling tail (last few KB) of decoded Write/Edit input per job — the live
    /// "source feed" for master synthesis. Backfill for late subscribers.
    tails: HashMap<i64, String>,
    auth: AuthCheck,
}

pub struct JobManager {
    inner: Mutex<Inner>,
}

impl Default for JobManager {
    fn default() -> Self {
        Self {
            inner: Mutex::new(Inner {
                queue: VecDeque::new(),
                running: HashMap::new(),
                output: HashMap::new(),
                tails: HashMap::new(),
                auth: AuthCheck {
                    status: "pending".into(),
                    detail: "Startup self-check has not run yet.".into(),
                },
            }),
        }
    }
}

impl JobManager {
    fn lock_inner(&self) -> MutexGuard<'_, Inner> {
        lock(&self.inner)
    }

    pub fn events_for(&self, job_id: i64) -> Vec<ProgressEvent> {
        self.lock_inner()
            .output
            .get(&job_id)
            .map(|o| o.events.clone())
            .unwrap_or_default()
    }

    pub fn tail_for(&self, job_id: i64) -> String {
        self.lock_inner()
            .tails
            .get(&job_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn auth_check(&self) -> AuthCheck {
        self.lock_inner().auth.clone()
    }

    /// Kills every running child on the way out.
    ///
    /// `std::process::Child` has no `Drop` that kills or reaps, so quitting
    /// mid-job used to leave `claude` running, reparented to launchd: it went
    /// on burning subscription quota with nothing reading its output, and
    /// `startup_recovery` then marked the row failed while keeping its
    /// `session_id`, so RESUME would put a second writer on the same output
    /// file. Draining the queue first stops the pump handing out new work
    /// while this runs.
    pub fn shutdown(&self) {
        let children: Vec<Arc<Mutex<Option<Child>>>> = {
            let mut inner = self.lock_inner();
            inner.queue.clear();
            inner
                .running
                .values()
                .map(|r| {
                    r.cancelled.store(true, Ordering::SeqCst);
                    r.child.clone()
                })
                .collect()
        };
        for slot in children {
            if let Some(mut child) = lock(&slot).take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-kind invocation scoping (SPEC §6)

// Model and effort come from Settings at spawn time (settings.rs; defaults
// Opus at xhigh per the M3 owner decision superseding SPEC §6's per-kind
// split). One global pair covers every job kind.

fn allowed_tools(kind: &str) -> Option<&'static str> {
    match kind {
        "extract" | "module_guide" | "master_guide" | "practice" => {
            Some("Read,Glob,Grep,Write")
        }
        "sort_proposal" | "syllabus_scan" => Some("Read,Glob,Grep"),
        _ => None, // self_check needs no tools
    }
}

/// Kinds that get write tools, and so need their output path checked after.
fn writes_to_disk(kind: &str) -> bool {
    matches!(kind, "extract" | "module_guide" | "master_guide" | "practice")
}

/// Parks the full list of out-of-contract paths in the audit log. The row is
/// the record of what a run actually did; the job's error message only has
/// room for the count and the first name.
fn record_contract_breach(app: &AppHandle, job: &QueuedJob, touched: &[String]) {
    eprintln!(
        "job {} ({}) wrote outside {:?}: {touched:?}",
        job.id,
        job.kind,
        crate::db::JOB_WRITABLE
    );
    let payload = serde_json::json!({
        "jobId": job.id,
        "kind": job.kind,
        "classId": job.class_id,
        "writable": crate::db::JOB_WRITABLE,
        "touched": touched,
    });
    let recorded = with_conn(app, |conn| crate::db::audit(conn, "job.out_of_contract", payload));
    if let Err(e) = recorded {
        eprintln!("job {} contract-breach audit row failed: {e:#}", job.id);
    }
}

// ---------------------------------------------------------------------------
// Public API

/// Marks jobs left queued/running by a previous app process as failed.
pub fn startup_recovery(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE jobs SET status = 'failed', error = 'interrupted by app restart',
         finished_at = ?1 WHERE status IN ('queued', 'running')",
        [now()],
    )?;
    Ok(())
}

/// Drops raw job logs past `LOG_RETENTION`. Best effort throughout: a log that
/// cannot be read or removed is not worth failing a launch over.
pub fn prune_logs(data_dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(data_dir.join("logs")) else {
        return;
    };
    for entry in entries.flatten() {
        let aged = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m.elapsed().map(|age| age > LOG_RETENTION).unwrap_or(false))
            .unwrap_or(false);
        if aged {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// SPEC §7 step 3: one batched extract job per class per run. `payload` is the
/// JSON batch manifest that extract::finalize_job records on success.
pub fn enqueue_extract(
    app: &AppHandle,
    class_id: i64,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue(app, "extract", Some(class_id), None, prompt, Some(payload), None)
}

/// SPEC §8.1: module guide synthesis (manual trigger only). `payload` carries
/// the guides-row upsert data that guides::finalize_job records on success.
pub fn enqueue_module_guide(
    app: &AppHandle,
    class_id: i64,
    scope: &str,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue(app, "module_guide", Some(class_id), Some(scope), prompt, Some(payload), None)
}

/// SPEC §8.2: semester master synthesis (manual trigger only, runs exclusively).
/// With `resume_session` set this continues a failed run's claude session (§6).
pub fn enqueue_master_guide(
    app: &AppHandle,
    class_id: i64,
    prompt: &str,
    payload: String,
    resume_session: Option<String>,
) -> Result<i64> {
    enqueue(
        app,
        "master_guide",
        Some(class_id),
        Some("master"),
        prompt,
        Some(payload),
        resume_session,
    )
}

/// SPEC §8.3: practice exam synthesis, triggered from chat (M8). `payload`
/// carries the output path guides::finalize_practice verifies on success.
pub fn enqueue_practice(
    app: &AppHandle,
    class_id: i64,
    scope: &str,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue(app, "practice", Some(class_id), Some(scope), prompt, Some(payload), None)
}

/// SPEC §10 step 2: sort_proposal job over a class inbox (read-only tools).
/// The strict JSON it returns on stdout is recorded by sorter::finalize_job.
pub fn enqueue_sort(app: &AppHandle, class_id: i64, prompt: &str) -> Result<i64> {
    enqueue(app, "sort_proposal", Some(class_id), None, prompt, None, None)
}

/// SPEC §11: syllabus_scan over a chosen file (scope = its rel path) or the
/// whole class folder (scope None). Read-only tools; the strict JSON it
/// returns on stdout is recorded by deadlines::finalize_job.
pub fn enqueue_syllabus(
    app: &AppHandle,
    class_id: i64,
    scope: Option<&str>,
    prompt: &str,
) -> Result<i64> {
    enqueue(app, "syllabus_scan", Some(class_id), scope, prompt, None, None)
}

/// SPEC §6: startup self-check asserting the active auth is the subscription.
pub fn enqueue_self_check(app: &AppHandle) -> Result<i64> {
    set_auth(app, "pending", "Self-check running…");
    enqueue(app, "self_check", None, None, SELF_CHECK_PROMPT, None, None)
}

/// Re-runs the scheduler outside any queue transition — the concurrency
/// setter calls this so a raised limit starts already-queued jobs at once
/// instead of waiting for the next enqueue or job exit to pump.
pub fn poke(app: &AppHandle) {
    pump(app);
}

pub fn cancel_job(app: &AppHandle, job_id: i64) -> Result<()> {
    let mgr = app.state::<JobManager>();
    let mut inner = mgr.lock_inner();
    if let Some(run) = inner.running.get(&job_id) {
        run.cancelled.store(true, Ordering::SeqCst);
        if let Some(child) = lock(&run.child).as_mut() {
            let _ = child.kill();
        }
        return Ok(()); // the worker thread finalizes the row as cancelled
    }
    if let Some(pos) = inner.queue.iter().position(|q| q.id == job_id) {
        inner.queue.remove(pos);
        drop(inner);
        with_conn(app, |conn| {
            conn.execute(
                "UPDATE jobs SET status = 'cancelled', finished_at = ?1 WHERE id = ?2",
                params![now(), job_id],
            )?;
            Ok(())
        })?;
        let _ = app.emit("jobs-changed", ());
        return Ok(());
    }
    bail!("job {job_id} is not queued or running")
}

pub fn list_jobs(conn: &Connection) -> Result<Vec<JobInfo>> {
    let mut stmt = conn.prepare(
        "SELECT j.id, j.kind, j.class_id, c.display_name, c.color, j.scope, j.status,
                j.created_at, j.started_at, j.finished_at, j.error, j.summary,
                j.log_path, j.session_id
         FROM jobs j LEFT JOIN classes c ON c.id = j.class_id
         ORDER BY j.id DESC LIMIT 50",
    )?;
    let jobs = stmt
        .query_map([], |row| {
            Ok(JobInfo {
                id: row.get(0)?,
                kind: row.get(1)?,
                class_id: row.get(2)?,
                class_name: row.get(3)?,
                class_color: row.get(4)?,
                scope: row.get(5)?,
                status: row.get(6)?,
                created_at: row.get(7)?,
                started_at: row.get(8)?,
                finished_at: row.get(9)?,
                error: row.get(10)?,
                summary: row.get(11)?,
                log_path: row.get(12)?,
                session_id: row.get(13)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(jobs)
}

// ---------------------------------------------------------------------------
// Queue mechanics

fn enqueue(
    app: &AppHandle,
    kind: &str,
    class_id: Option<i64>,
    scope: Option<&str>,
    prompt: &str,
    payload: Option<String>,
    resume_session: Option<String>,
) -> Result<i64> {
    let id = with_conn(app, |conn| {
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, created_at, payload)
             VALUES (?1, ?2, ?3, 'queued', ?4, ?5)",
            params![kind, class_id, scope, now(), payload],
        )?;
        Ok(conn.last_insert_rowid())
    })?;
    {
        let mgr = app.state::<JobManager>();
        mgr.lock_inner().queue.push_back(QueuedJob {
            id,
            kind: kind.to_string(),
            class_id,
            prompt: prompt.to_string(),
            payload,
            resume_session,
        });
    }
    let _ = app.emit("jobs-changed", ());
    pump(app);
    Ok(id)
}

/// Starts queued jobs while free slots remain. SPEC §8.2 exclusivity: a
/// master_guide at the front waits for every running job to finish, and while
/// one runs (or waits, FIFO) nothing else starts.
fn pump(app: &AppHandle) {
    // Read before taking the manager lock — with_conn must never nest inside
    // it (a thread holding the Db lock may be about to take this one).
    let max_concurrent = crate::settings::job_concurrency(app);
    loop {
        let (job, child_slot, cancelled) = {
            let mgr = app.state::<JobManager>();
            let mut inner = mgr.lock_inner();
            if inner.running.values().any(|r| r.exclusive) {
                return;
            }
            let Some(front) = inner.queue.front() else {
                return;
            };
            let exclusive = front.kind == "master_guide";
            if exclusive && !inner.running.is_empty() {
                return; // queue drains first
            }
            if inner.running.len() >= max_concurrent {
                return;
            }
            let Some(job) = inner.queue.pop_front() else {
                return;
            };
            let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));
            let cancelled = Arc::new(AtomicBool::new(false));
            inner.running.insert(
                job.id,
                RunningJob {
                    child: child_slot.clone(),
                    cancelled: cancelled.clone(),
                    exclusive,
                },
            );
            (job, child_slot, cancelled)
        };
        let app = app.clone();
        std::thread::spawn(move || run_job(app, job, child_slot, cancelled));
    }
}

// ---------------------------------------------------------------------------
// Worker

enum Outcome {
    Succeeded {
        summary: String,
        /// The untruncated result text, for kinds whose final message is a
        /// machine-readable contract (sort_proposal's JSON) — the summary's
        /// display cap would corrupt it.
        result_text: Option<String>,
    },
    Failed {
        error: String,
    },
}

struct RunSlot {
    app: AppHandle,
    job_id: i64,
}

impl Drop for RunSlot {
    fn drop(&mut self) {
        let mgr = self.app.state::<JobManager>();
        let mut inner = mgr.lock_inner();
        // Normally already gone; this only bites on an unwind.
        if inner.running.remove(&self.job_id).is_some() {
            eprintln!("job {} released by unwind", self.job_id);
            drop(inner);
            let _ = self.app.emit("jobs-changed", ());
            pump(&self.app);
        }
    }
}

fn run_job(
    app: AppHandle,
    job: QueuedJob,
    child_slot: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
) {
    // Releases the concurrency slot on unwind too: a panic anywhere below
    // would otherwise leave the entry in `running` forever, consuming a slot
    // (the exclusive one, for a master_guide) with no way to reclaim it.
    let _slot = RunSlot {
        app: app.clone(),
        job_id: job.id,
    };
    // Fingerprinted before the spawn and compared after, so a run that wandered
    // outside its contracted output path is caught rather than trusted.
    let guarded_dir = writes_to_disk(&job.kind)
        .then(|| job.class_id)
        .flatten()
        .and_then(|id| with_conn(&app, |c| crate::scanner::class_dir(c, id)).ok());
    let before = guarded_dir
        .as_deref()
        .map(crate::scanner::fingerprint_sources);

    let outcome = execute_job(&app, &job, &child_slot, &cancelled);

    let (mut status, mut error, mut summary, result_text) =
        if cancelled.load(Ordering::SeqCst) {
            ("cancelled", None, None, None)
        } else {
            match outcome {
                Ok(Outcome::Succeeded {
                    summary,
                    result_text,
                }) => ("succeeded", None, Some(summary), result_text),
                Ok(Outcome::Failed { error }) => ("failed", Some(error), None, None),
                Err(e) => ("failed", Some(format!("{e:#}")), None, None),
            }
        };
    if status == "cancelled" {
        push_event(&app, job.id, "status", "cancelled — child process killed".into());
    }

    // Runs whatever the outcome: a cancelled or failed run had the same tools.
    if let (Some(dir), Some(before)) = (guarded_dir.as_deref(), before) {
        let touched = crate::scanner::diff_fingerprints(
            &before,
            &crate::scanner::fingerprint_sources(dir),
        );
        if !touched.is_empty() {
            record_contract_breach(&app, &job, &touched);
            if status == "succeeded" {
                status = "failed";
                summary = None;
            }
            error = Some(truncate(
                &format!(
                    "the run wrote outside its contracted output path — {} file(s) changed, \
                     starting with {}. Nothing was recorded; the audit log holds the full list.",
                    touched.len(),
                    touched.first().map(String::as_str).unwrap_or("?")
                ),
                2000,
            ));
        }
    }

    // Kind-specific record keeping runs BEFORE the row leaves 'running':
    // the extract pipeline treats "no active job + stale columns" as a signal
    // to enqueue, and the guides upsert must land before the UI refetches on
    // the succeeded transition.
    if status == "succeeded" {
        // A succeeded job with no artifact behind it is a lie: every kind that
        // produces one reconciles here, and demotes itself when it cannot.
        let mut demote = |what: &str, e: anyhow::Error| {
            status = "failed";
            error = Some(format!("{what}: {e:#}"));
            summary = None;
        };
        match job.kind.as_str() {
            "extract" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    if let Err(e) = crate::extract::finalize_job(&app, class_id, payload) {
                        eprintln!("extract job {} record keeping failed: {e:#}", job.id);
                    }
                }
            }
            "module_guide" | "master_guide" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    if let Err(e) = crate::guides::finalize_job(&app, class_id, payload) {
                        demote("synthesis finished but no guide was recorded", e);
                    }
                }
            }
            "practice" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    if let Err(e) = crate::guides::finalize_practice(&app, class_id, payload) {
                        demote("practice job finished but no exam was written", e);
                    }
                }
            }
            "sort_proposal" => {
                if let Some(class_id) = job.class_id {
                    match crate::sorter::finalize_job(
                        &app,
                        class_id,
                        result_text.as_deref().unwrap_or(""),
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("sort job finished but recorded no proposals", e),
                    }
                }
            }
            "syllabus_scan" => {
                if let Some(class_id) = job.class_id {
                    // An empty array is a legitimate outcome (finalize reports
                    // it honestly); only unparseable output demotes to failure.
                    match crate::deadlines::finalize_job(
                        &app,
                        class_id,
                        result_text.as_deref().unwrap_or(""),
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("syllabus scan finished but recorded no proposals", e),
                    }
                }
            }
            _ => {}
        }
    }

    let update = with_conn(&app, |conn| {
        conn.execute(
            "UPDATE jobs SET status = ?1, finished_at = ?2, error = ?3, summary = ?4
             WHERE id = ?5",
            params![status, now(), error, summary, job.id],
        )?;
        Ok(())
    });
    if let Err(e) = update {
        eprintln!("job {} finalize failed: {e:#}", job.id);
    }

    {
        let mgr = app.state::<JobManager>();
        let mut inner = mgr.lock_inner();
        inner.running.remove(&job.id);
        // The live-source tail exists to feed the streaming viewer while the
        // run composes; once the row settles nothing reads it again, and it is
        // the largest per-job buffer. The condensed event list stays — the Job
        // Center still shows it for a finished run.
        inner.tails.remove(&job.id);
    }
    let _ = app.emit("jobs-changed", ());
    // Files dropped while a sort ran were invisible to its prompt; with the
    // row settled, a follow-up run covers whatever is still fresh.
    if job.kind == "sort_proposal" && status == "succeeded" {
        if let Some(class_id) = job.class_id {
            crate::sorter::enqueue_followup(&app, class_id);
        }
    }
    pump(&app);
}

/// State accumulated while consuming the stream-json output.
#[derive(Default)]
struct StreamState {
    api_key_source: Option<String>,
    rate_limit_type: Option<String>,
    result_is_error: Option<bool>,
    result_text: Option<String>,
    /// Set when a self_check init reveals non-subscription auth: kill immediately
    /// so the child neither bills API credits nor grinds through 401 retries.
    auth_abort: Option<String>,
    /// Index of the content block currently streaming a Write/Edit tool input
    /// (master jobs run with --include-partial-messages).
    write_block: Option<u64>,
    /// Total streamed Write/Edit input bytes — the phased-progress "composing"
    /// signal. Approximate (JSON-escaped), which is fine for a progress label.
    write_bytes: usize,
    write_bytes_reported: usize,
    /// Decoded source text awaiting a tail flush, and an incomplete trailing
    /// JSON escape carried between fragments.
    tail_pending: String,
    tail_carry: String,
}

/// Best-effort unescape of a streamed JSON string fragment for the live source
/// tail. `carry` holds an incomplete trailing escape (`\`, `\u12`) between
/// fragments so sequences split across deltas decode correctly.
fn unescape_fragment(carry: &mut String, fragment: &str) -> String {
    let s = format!("{carry}{fragment}");
    carry.clear();
    let mut out = String::with_capacity(s.len());
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(&(_, next)) = chars.peek() else {
            carry.push_str(&s[i..]); // fragment ends mid-escape
            break;
        };
        match next {
            'n' => {
                out.push('\n');
                chars.next();
            }
            't' => {
                out.push('\t');
                chars.next();
            }
            'r' => {
                chars.next();
            }
            '"' | '\\' | '/' => {
                out.push(next);
                chars.next();
            }
            'u' => {
                let Some(hex) = s.get(i + 2..i + 6) else {
                    carry.push_str(&s[i..]);
                    break;
                };
                if let Some(ch) = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
                for _ in 0..5 {
                    chars.next(); // 'u' + 4 hex digits
                }
            }
            _ => {
                out.push(next);
                chars.next();
            }
        }
    }
    out
}

/// `Command::output()` with a deadline: kills and reaps on expiry rather than
/// blocking forever on a child that never exits.
pub(crate) fn wait_bounded(mut child: Child, limit: Duration) -> Option<std::process::Output> {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The long-lived `claude setup-token` credential, exported from `.zshrc`.
///
/// Launched from Finder, the app is started by launchd, which supplies none of
/// the interactive shell environment — so the variable is simply absent and
/// `claude` falls back to the Keychain OAuth the setup token exists to bypass,
/// failing the self-check with a 401. Read it back out of an interactive shell
/// so `.zshrc` stays the one place the token lives. Cached for the process:
/// sourcing the rc file is not free, and the token does not change under a
/// running app. A terminal launch already has it inherited and never forks.
fn subscription_token() -> Option<&'static str> {
    static TOKEN: OnceLock<Option<String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            if let Some(t) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN")
                .ok()
                .filter(|t| !t.is_empty())
            {
                return Some(t);
            }
            // Fenced, because an interactive rc file writes to stdout too —
            // Terminal's session restore alone prepends a "Restored session:"
            // line, which lands in the header and is rejected as a line break
            // mid-token. Only what sits between the markers is the value.
            // Bounded, because `-i` sources the whole rc file and this runs
            // inside a OnceLock every other job thread waits on: an rc file
            // that stalls on a plugin manager or a network-touching prompt
            // would otherwise take the entire runner down with it, with the
            // jobs stuck in `running` and nothing to cancel. Falling back to
            // "no subscription auth" is an outcome the self-check reports
            // honestly, so giving up is safe.
            let child = Command::new("/bin/zsh")
                .args(["-ic", r#"print -rn -- "<<CHTOK>>$CLAUDE_CODE_OAUTH_TOKEN<</CHTOK>>""#])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let out = wait_bounded(child, Duration::from_secs(5))?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let token = stdout
                .rsplit_once("<<CHTOK>>")
                .and_then(|(_, rest)| rest.split_once("<</CHTOK>>"))
                .map(|(token, _)| token.trim())?;
            // An Authorization header cannot hold whitespace: a token that has
            // any is contaminated, and passing it on would trade the honest
            // "no subscription auth" verdict for a confusing 400.
            (!token.is_empty() && !token.contains(char::is_whitespace))
                .then(|| token.to_string())
        })
        .as_deref()
}

fn execute_job(
    app: &AppHandle,
    job: &QueuedJob,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancelled: &Arc<AtomicBool>,
) -> Result<Outcome> {
    let class_dir: Option<PathBuf> = match job.class_id {
        Some(class_id) => Some(with_conn(app, |c| crate::scanner::class_dir(c, class_id))?),
        None => None,
    };
    if let Some(dir) = &class_dir {
        if !dir.is_dir() {
            bail!("class folder not found: {}", dir.display());
        }
    }

    let data_dir = app.path().app_data_dir().context("resolving app data dir")?;
    let log_dir = data_dir.join("logs");
    fs::create_dir_all(&log_dir)?;
    let log_path = log_dir.join(format!("job-{}.jsonl", job.id));
    let mut log = fs::File::create(&log_path)
        .with_context(|| format!("creating log {}", log_path.display()))?;
    let mut log_broken = false;

    with_conn(app, |conn| {
        conn.execute(
            "UPDATE jobs SET status = 'running', started_at = ?1, log_path = ?2 WHERE id = ?3",
            params![now(), log_path.to_string_lossy(), job.id],
        )?;
        Ok(())
    })?;
    let _ = app.emit("jobs-changed", ());

    if cancelled.load(Ordering::SeqCst) {
        bail!("cancelled before start");
    }

    let home = dirs::home_dir().context("resolving home directory")?;
    let claude = home.join(".local").join("bin").join("claude");
    // The Settings screen's job model/effort, read at spawn time so a change
    // applies to the very next job — queued jobs included.
    let (model, effort) = crate::settings::job_spawn_options(app);
    let mut cmd = Command::new(&claude);
    cmd.arg("-p")
        .arg(&job.prompt)
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--model", &model])
        .args(["--effort", &effort])
        .args(["--disallowedTools", disallowed_tools(&job.kind)])
        // The user-level claude config leaks MCP servers (e.g. web search)
        // into spawns; with no --mcp-config this loads zero MCP servers.
        .arg("--strict-mcp-config");
    if let Some(tools) = allowed_tools(&job.kind) {
        cmd.args(["--allowedTools", tools]);
    }
    // SPEC §6 resumability: continue a failed run's session where it left off.
    if let Some(session) = &job.resume_session {
        cmd.args(["--resume", session]);
    }
    // SPEC §8.2 long-job UX: partial message chunks expose the guide being
    // written as it streams, powering the phased progress display. Master only —
    // shorter kinds don't need the extra stream volume.
    if job.kind == "master_guide" {
        cmd.arg("--include-partial-messages");
        // Without this the API assembles each Write/Edit input server-side and
        // releases its partial_json deltas in one burst at block completion
        // (CLI ≥2.1.40 gates fine-grained tool streaming behind a feature flag
        // or this env var), reducing the live source feed to block-sized jumps.
        cmd.env("CLAUDE_CODE_ENABLE_FINE_GRAINED_TOOL_STREAMING", "1");
    }
    match &class_dir {
        Some(dir) => {
            cmd.current_dir(dir);
            cmd.arg("--add-dir").arg(dir);
        }
        None => {
            cmd.current_dir(&data_dir);
        }
    }
    // SPEC §1 auth precedence: without this, synthesis silently bills API credits.
    cmd.env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN");
    if let Some(token) = subscription_token() {
        cmd.env("CLAUDE_CODE_OAUTH_TOKEN", token);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", claude.display()))?;
    let stdout = child.stdout.take().context("taking child stdout")?;
    let stderr = child.stderr.take().context("taking child stderr")?;
    *lock(child_slot) = Some(child);
    // A cancel may have raced the spawn, setting the flag before the child
    // landed in the slot — kill here so it doesn't run unsupervised.
    if cancelled.load(Ordering::SeqCst) {
        if let Some(c) = lock(child_slot).as_mut() {
            let _ = c.kill();
        }
    }

    let stderr_tail: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let stderr_thread = {
        let tail = stderr_tail.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut tail = lock(&tail);
                tail.push_str(&line);
                tail.push('\n');
                if tail.len() > 4000 {
                    let cut = tail.len() - 4000;
                    let cut = tail
                        .char_indices()
                        .map(|(i, _)| i)
                        .find(|&i| i >= cut)
                        .unwrap_or(0);
                    tail.drain(..cut);
                }
            }
        })
    };

    // A child that connects and then goes quiet holds its concurrency slot
    // forever, and a master_guide holds the exclusive slot, so everything
    // queued behind it stops too. Duration is not the signal — SPEC §8.2
    // expects master synthesis to run past thirty minutes — but it streams
    // throughout, so silence is.
    let last_line = Arc::new(Mutex::new(Instant::now()));
    let stalled = Arc::new(AtomicBool::new(false));
    let watchdog = {
        let last_line = last_line.clone();
        let stalled = stalled.clone();
        let child_slot = child_slot.clone();
        let cancelled = cancelled.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(5));
            // The slot empties when the run finishes; that is the exit signal.
            if lock(&child_slot).is_none() || cancelled.load(Ordering::SeqCst) {
                return;
            }
            if lock(&last_line).elapsed() >= STALL_LIMIT {
                stalled.store(true, Ordering::SeqCst);
                if let Some(c) = lock(&child_slot).as_mut() {
                    let _ = c.kill();
                }
                return;
            }
        })
    };

    let mut stream = StreamState::default();
    // Read in a closure so an I/O error cannot return past the cleanup below:
    // the child is still running and still in the slot at that point, and
    // nothing else would kill it or join the stderr thread parked on its pipe.
    let read_result = (|| -> Result<()> {
        for line in BufReader::new(stdout).lines() {
            let line = line.context("reading child stdout")?;
            *lock(&last_line) = Instant::now();
            if let Err(e) = writeln!(log, "{line}") {
                // The Job Center points at this file; a silently truncated log
                // would make it lie about what the run did.
                log_write_failed(app, job, &mut log_broken, &e);
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            handle_event(app, job, &value, &mut stream);
            if stream.auth_abort.is_some() {
                if let Some(c) = lock(child_slot).as_mut() {
                    let _ = c.kill();
                }
                break;
            }
        }
        Ok(())
    })();

    // Take the child out of the slot so a late cancel can't block on wait().
    let mut child = lock(child_slot).take().context("child handle missing")?;
    if read_result.is_err() {
        let _ = child.kill();
    }
    let exit = child.wait().context("waiting for child")?;
    let _ = stderr_thread.join();
    let _ = watchdog.join();
    if stalled.load(Ordering::SeqCst) {
        return Ok(Outcome::Failed {
            error: format!(
                "no output for {} minutes — the run was stopped as wedged",
                STALL_LIMIT.as_secs() / 60
            ),
        });
    }
    read_result?;

    if job.kind == "self_check" {
        return Ok(resolve_self_check(app, &stream, &lock(&stderr_tail)));
    }

    let stderr_text = lock(&stderr_tail).trim().to_string();
    match (stream.result_is_error, stream.result_text) {
        (Some(false), text) => Ok(Outcome::Succeeded {
            summary: truncate(text.as_deref().unwrap_or("done"), 4000),
            result_text: text,
        }),
        (Some(true), text) => Ok(Outcome::Failed {
            error: truncate(text.as_deref().unwrap_or("claude reported an error"), 2000),
        }),
        (None, _) => Ok(Outcome::Failed {
            error: if stderr_text.is_empty() {
                format!("claude exited ({exit}) without a result event")
            } else {
                truncate(&stderr_text, 2000)
            },
        }),
    }
}

/// SPEC §6: verdict for the startup subscription-auth self-check.
fn resolve_self_check(app: &AppHandle, stream: &StreamState, stderr_tail: &str) -> Outcome {
    if let Some(detail) = &stream.auth_abort {
        set_auth(app, "failed", detail);
        return Outcome::Failed {
            error: detail.clone(),
        };
    }
    match (stream.api_key_source.as_deref(), stream.result_is_error) {
        (Some("none"), Some(false)) => {
            let window = stream.rate_limit_type.as_deref().unwrap_or("unknown");
            let detail = format!(
                "Subscription auth verified — apiKeySource none, rate window {window}."
            );
            set_auth(app, "ok", &detail);
            Outcome::Succeeded {
                summary: detail,
                result_text: None,
            }
        }
        (source, _) => {
            let reason = stream
                .result_text
                .clone()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| {
                    if stderr_tail.is_empty() {
                        format!(
                            "self-check did not complete (auth source: {})",
                            source.unwrap_or("unknown")
                        )
                    } else {
                        truncate(stderr_tail.trim(), 500)
                    }
                });
            let detail = format!("Self-check failed: {reason}");
            set_auth(app, "failed", &detail);
            Outcome::Failed { error: detail }
        }
    }
}

// ---------------------------------------------------------------------------
// Stream-json → condensed progress events

fn handle_event(app: &AppHandle, job: &QueuedJob, value: &Value, stream: &mut StreamState) {
    match value["type"].as_str().unwrap_or("") {
        "system" => match value["subtype"].as_str().unwrap_or("") {
            "init" => {
                if let Some(session_id) = value["session_id"].as_str() {
                    let session_id = session_id.to_string();
                    let job_id = job.id;
                    let _ = with_conn(app, move |conn| {
                        conn.execute(
                            "UPDATE jobs SET session_id = ?1 WHERE id = ?2",
                            params![session_id, job_id],
                        )?;
                        Ok(())
                    });
                }
                let model = value["model"].as_str().unwrap_or("?");
                let source = value["apiKeySource"].as_str().unwrap_or("unknown");
                push_event(
                    app,
                    job.id,
                    "status",
                    format!("session start · model {model} · auth source {source}"),
                );
                if job.kind == "self_check" && source != "none" {
                    let detail = format!(
                        "Spawned claude would authenticate via {source}, not the \
                         subscription. Aborted before any API call."
                    );
                    push_event(app, job.id, "error", detail.clone());
                    stream.auth_abort = Some(detail);
                }
                stream.api_key_source = Some(source.to_string());
            }
            "api_retry" => {
                let attempt = value["attempt"].as_i64().unwrap_or(0);
                let max = value["max_retries"].as_i64().unwrap_or(0);
                let status = value["error_status"].as_i64().unwrap_or(0);
                let error = value["error"].as_str().unwrap_or("unknown");
                push_event(
                    app,
                    job.id,
                    "retry",
                    format!("api retry {attempt}/{max} · {status} {error}"),
                );
            }
            _ => {}
        },
        "rate_limit_event" => {
            stream.rate_limit_type = value["rate_limit_info"]["rateLimitType"]
                .as_str()
                .map(str::to_string);
        }
        // Partial message chunks (master_guide only): the long silent stretch of
        // a 30-min job is the model streaming one huge Write input. Tracking its
        // bytes turns that silence into live "composing" progress (SPEC §8.2).
        "stream_event" => {
            let event = &value["event"];
            match event["type"].as_str().unwrap_or("") {
                "content_block_start" => {
                    let block = &event["content_block"];
                    if block["type"].as_str() == Some("tool_use")
                        && matches!(block["name"].as_str(), Some("Write") | Some("Edit"))
                    {
                        stream.write_block = event["index"].as_u64();
                        if stream.write_bytes == 0 {
                            push_event(app, job.id, "phase", "composing — writing the guide".into());
                        }
                    }
                }
                "content_block_delta" => {
                    if stream.write_block.is_some()
                        && stream.write_block == event["index"].as_u64()
                    {
                        if let Some(chunk) = event["delta"]["partial_json"].as_str() {
                            stream.write_bytes += chunk.len();
                            // Sparse updates: one event per ~16 KB, not per delta.
                            if stream.write_bytes - stream.write_bytes_reported >= 16 * 1024 {
                                stream.write_bytes_reported = stream.write_bytes;
                                push_event(
                                    app,
                                    job.id,
                                    "phase",
                                    format!("composing · ~{} KB written", stream.write_bytes / 1024),
                                );
                            }
                            // Live source feed: decode and flush in ~1 KB chunks.
                            let text = unescape_fragment(&mut stream.tail_carry, chunk);
                            stream.tail_pending.push_str(&text);
                            if stream.tail_pending.len() >= 1024 {
                                push_tail(app, job.id, std::mem::take(&mut stream.tail_pending));
                            }
                        }
                    }
                }
                "content_block_stop" => {
                    // is_some, as in the delta arm above: with no open write
                    // block and a stop event carrying no index, None == None
                    // would flush a stray newline into the live source tail.
                    if stream.write_block.is_some()
                        && stream.write_block == event["index"].as_u64()
                    {
                        stream.write_block = None;
                        stream.tail_carry.clear();
                        stream.tail_pending.push('\n');
                        push_tail(app, job.id, std::mem::take(&mut stream.tail_pending));
                    }
                }
                _ => {}
            }
        }
        "assistant" => {
            for block in value["message"]["content"].as_array().unwrap_or(&Vec::new()) {
                match block["type"].as_str().unwrap_or("") {
                    "text" => {
                        if let Some(text) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            push_event(app, job.id, "text", truncate(text, 1500));
                        }
                    }
                    "tool_use" => {
                        let name = block["name"].as_str().unwrap_or("tool");
                        let input = compact_tool_input(&block["input"]);
                        push_event(app, job.id, "tool", format!("{name} · {input}"));
                    }
                    _ => {}
                }
            }
        }
        "user" => {
            for block in value["message"]["content"].as_array().unwrap_or(&Vec::new()) {
                if block["type"].as_str() != Some("tool_result") {
                    continue;
                }
                let text = tool_result_text(block);
                // PDF reads return rendered page images with little or no text;
                // count both so the label reflects what the model received.
                let images = block["content"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter(|p| p["type"].as_str() == Some("image"))
                            .count()
                    })
                    .unwrap_or(0);
                let condensed = if block["is_error"].as_bool().unwrap_or(false) {
                    format!("tool error · {}", truncate(&text, 300))
                } else {
                    let lines = if text.trim().is_empty() {
                        0
                    } else {
                        text.lines().count()
                    };
                    let mut parts = Vec::new();
                    if lines > 0 {
                        parts.push(count_label(lines, "line"));
                    }
                    if images > 0 {
                        parts.push(count_label(images, "image"));
                    }
                    if parts.is_empty() {
                        "empty result".to_string()
                    } else {
                        parts.join(" · ")
                    }
                };
                let detail = {
                    let trimmed = text.trim();
                    (!trimmed.is_empty()).then(|| truncate(trimmed, 4000))
                };
                push_event_detail(app, job.id, "tool_result", condensed, detail);
            }
        }
        "result" => {
            let is_error = value["is_error"].as_bool().unwrap_or(true);
            stream.result_is_error = Some(is_error);
            stream.result_text = value["result"].as_str().map(str::to_string);
            if is_error {
                let text = stream
                    .result_text
                    .clone()
                    .unwrap_or_else(|| value["subtype"].as_str().unwrap_or("error").to_string());
                push_event(app, job.id, "error", truncate(&text, 1000));
            } else {
                let turns = value["num_turns"].as_i64().unwrap_or(0);
                let secs = value["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
                push_event(
                    app,
                    job.id,
                    "result",
                    format!("done · {turns} turns · {secs:.1}s"),
                );
            }
        }
        _ => {}
    }
}

fn push_event(app: &AppHandle, job_id: i64, kind: &str, text: String) {
    push_event_detail(app, job_id, kind, text, None);
}

/// Appends to the rolling per-job tail buffer (capped) and emits the chunk.
fn push_tail(app: &AppHandle, job_id: i64, chunk: String) {
    const TAIL_CAP: usize = 8 * 1024;
    {
        let mgr = app.state::<JobManager>();
        let mut inner = mgr.lock_inner();
        let tail = inner.tails.entry(job_id).or_default();
        tail.push_str(&chunk);
        if tail.len() > TAIL_CAP {
            let mut cut = tail.len() - TAIL_CAP;
            while cut < tail.len() && !tail.is_char_boundary(cut) {
                cut += 1;
            }
            tail.drain(..cut);
        }
    }
    let _ = app.emit(&format!("job://{job_id}/tail"), chunk);
}

fn push_event_detail(
    app: &AppHandle,
    job_id: i64,
    kind: &str,
    text: String,
    detail: Option<String>,
) {
    let event = {
        let mgr = app.state::<JobManager>();
        let mut inner = mgr.lock_inner();
        let out = inner.output.entry(job_id).or_default();
        let event = ProgressEvent {
            seq: out.next_seq,
            kind: kind.to_string(),
            text,
            detail,
        };
        out.next_seq += 1;
        out.events.push(event.clone());
        if out.events.len() > 2000 {
            out.events.drain(..500);
        }
        event
    };
    let _ = app.emit(&format!("job://{job_id}/progress"), event);
}

fn set_auth(app: &AppHandle, status: &str, detail: &str) {
    let check = AuthCheck {
        status: status.to_string(),
        detail: detail.to_string(),
    };
    {
        let mgr = app.state::<JobManager>();
        mgr.lock_inner().auth = check.clone();
    }
    let _ = app.emit("auth-check", check);
}

fn compact_tool_input(input: &Value) -> String {
    for key in ["pattern", "file_path", "path", "query", "command", "description"] {
        if let Some(s) = input.get(key).and_then(Value::as_str) {
            return truncate(s, 120);
        }
    }
    truncate(&input.to_string(), 120)
}

fn tool_result_text(block: &Value) -> String {
    match &block["content"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn count_label(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// Parses the proposal-shaped jobs' output contract (sort_proposal,
/// syllabus_scan): a bare JSON array as the final message, tolerating a
/// fenced block or stray prose around it. A candidate `[` counts only when
/// the next non-whitespace character is `{` (or `]`), so a bracket inside
/// prose — e.g. a filename like `[draft] notes.pdf` — never wins the slice;
/// the stream deserializer then stops at the array's end, so trailing prose
/// is harmless too.
pub(crate) fn parse_entries(text: &str) -> Result<Vec<Value>> {
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices('[') {
        let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
        if !matches!(next, Some(b'{') | Some(b']')) {
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(Value::Array(entries))) = stream.next() {
            return Ok(entries);
        }
    }
    bail!("no JSON array of proposals in the job output")
}


#[cfg(test)]
mod tests {
    use super::{parse_entries, unescape_fragment};

    /// The decoder carries state across fragments, because the API splits a
    /// stream wherever it likes — including mid-escape.
    #[test]
    fn decodes_escapes_within_one_fragment() {
        let mut carry = String::new();
        assert_eq!(unescape_fragment(&mut carry, r"a\nb\tc"), "a\nb\tc");
        assert!(carry.is_empty());
    }

    #[test]
    fn carries_an_escape_split_across_fragments() {
        let mut carry = String::new();
        assert_eq!(unescape_fragment(&mut carry, r"line\"), "line");
        assert_eq!(carry, r"\");
        assert_eq!(unescape_fragment(&mut carry, "n next"), "\n next");
        assert!(carry.is_empty());
    }

    #[test]
    fn carries_a_unicode_escape_split_mid_sequence() {
        let mut carry = String::new();
        assert_eq!(unescape_fragment(&mut carry, r"x\u00"), "x");
        assert_eq!(carry, r"\u00");
        assert_eq!(unescape_fragment(&mut carry, "e9 y"), "é y");
    }

    #[test]
    fn drops_carriage_returns_and_keeps_literal_quotes() {
        let mut carry = String::new();
        assert_eq!(unescape_fragment(&mut carry, r#"a\r\nb\"c\\d"#), "a\nb\"c\\d");
    }

    /// The sort and syllabus jobs return their contract as a JSON array
    /// embedded in prose, so the opening bracket has to be found without a
    /// prose bracket winning the slice.
    #[test]
    fn finds_the_array_after_prose() {
        let out = parse_entries("Here is what I found:\n[{\"a\": 1}]\nDone.").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["a"], 1);
    }

    #[test]
    fn skips_a_bracket_that_opens_prose() {
        let out = parse_entries("[note] the answer is [{\"a\": 2}]").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["a"], 2);
    }

    #[test]
    fn accepts_an_empty_array() {
        assert!(parse_entries("nothing found: []").unwrap().is_empty());
    }

    #[test]
    fn handles_a_trailing_bracket_without_panicking() {
        assert!(parse_entries("no array here [").is_err());
    }

    #[test]
    fn errors_when_there_is_no_array() {
        assert!(parse_entries("I could not find anything.").is_err());
    }
}
