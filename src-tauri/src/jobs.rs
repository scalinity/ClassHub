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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

const MAX_CONCURRENT: usize = 2;
/// SPEC §6: never allow these, regardless of user-level claude settings. The
/// `--allowedTools` list alone does not restrict tools the user's own config
/// permits (verified against claude 2.1.237), so deny rules are passed too.
const DISALLOWED_TOOLS: &str = "Bash,WebFetch,WebSearch";

const PROBE_PROMPT: &str = "List the files in this class folder using the Glob tool. \
Reply with a flat list of relative paths, one per line, then a final line \
`TOTAL: <n> files`. Do not read file contents.";
const SELF_CHECK_PROMPT: &str = "Reply with exactly: OK";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn with_conn<T>(app: &AppHandle, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let db = app.state::<crate::Db>();
    let guard = lock(&db.0);
    f(&guard)
}

// ---------------------------------------------------------------------------
// Types

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub seq: u64,
    /// status | text | tool | tool_result | retry | result | error
    pub kind: String,
    pub text: String,
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
}

struct RunningJob {
    child: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
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

    pub fn auth_check(&self) -> AuthCheck {
        self.lock_inner().auth.clone()
    }
}

// ---------------------------------------------------------------------------
// Per-kind invocation scoping (SPEC §6)

/// Owner decision (2026-08-22): every job runs Opus 5 at xhigh effort by default,
/// superseding SPEC §6's per-kind model split. Model and effort level become
/// user-configurable in Settings when that lands (M11).
const DEFAULT_MODEL: &str = "opus";
const DEFAULT_EFFORT: &str = "xhigh";

fn allowed_tools(kind: &str) -> Option<&'static str> {
    match kind {
        "extract" | "module_guide" | "master_guide" | "practice" => {
            Some("Read,Glob,Grep,Write")
        }
        "sort_proposal" | "syllabus_scan" | "probe" => Some("Read,Glob,Grep"),
        _ => None, // self_check needs no tools
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

/// M3 throwaway verification job: list files in the class folder.
pub fn enqueue_probe(app: &AppHandle, class_id: i64) -> Result<i64> {
    enqueue(app, "probe", Some(class_id), None, PROBE_PROMPT)
}

/// SPEC §6: startup self-check asserting the active auth is the subscription.
pub fn enqueue_self_check(app: &AppHandle) -> Result<i64> {
    set_auth(app, "pending", "Self-check running…");
    enqueue(app, "self_check", None, None, SELF_CHECK_PROMPT)
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
) -> Result<i64> {
    let id = with_conn(app, |conn| {
        conn.execute(
            "INSERT INTO jobs (kind, class_id, scope, status, created_at)
             VALUES (?1, ?2, ?3, 'queued', ?4)",
            params![kind, class_id, scope, now()],
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
        });
    }
    let _ = app.emit("jobs-changed", ());
    pump(app);
    Ok(id)
}

/// Starts queued jobs while free slots remain.
fn pump(app: &AppHandle) {
    loop {
        let (job, child_slot, cancelled) = {
            let mgr = app.state::<JobManager>();
            let mut inner = mgr.lock_inner();
            if inner.running.len() >= MAX_CONCURRENT {
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
    Succeeded { summary: String },
    Failed { error: String },
}

fn run_job(
    app: AppHandle,
    job: QueuedJob,
    child_slot: Arc<Mutex<Option<Child>>>,
    cancelled: Arc<AtomicBool>,
) {
    let outcome = execute_job(&app, &job, &child_slot, &cancelled);

    let (status, error, summary) = if cancelled.load(Ordering::SeqCst) {
        ("cancelled", None, None)
    } else {
        match outcome {
            Ok(Outcome::Succeeded { summary }) => ("succeeded", None, Some(summary)),
            Ok(Outcome::Failed { error }) => ("failed", Some(error), None),
            Err(e) => ("failed", Some(format!("{e:#}")), None),
        }
    };
    if status == "cancelled" {
        push_event(&app, job.id, "status", "cancelled — child process killed".into());
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
        mgr.lock_inner().running.remove(&job.id);
    }
    let _ = app.emit("jobs-changed", ());
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
    let mut cmd = Command::new(&claude);
    cmd.arg("-p")
        .arg(&job.prompt)
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--model", DEFAULT_MODEL])
        .args(["--effort", DEFAULT_EFFORT])
        .args(["--disallowedTools", DISALLOWED_TOOLS]);
    if let Some(tools) = allowed_tools(&job.kind) {
        cmd.args(["--allowedTools", tools]);
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

    let mut stream = StreamState::default();
    for line in BufReader::new(stdout).lines() {
        let line = line.context("reading child stdout")?;
        let _ = writeln!(log, "{line}");
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

    // Take the child out of the slot so a late cancel can't block on wait().
    let mut child = lock(child_slot).take().context("child handle missing")?;
    let exit = child.wait().context("waiting for child")?;
    let _ = stderr_thread.join();

    if job.kind == "self_check" {
        return Ok(resolve_self_check(app, &stream, &lock(&stderr_tail)));
    }

    let stderr_text = lock(&stderr_tail).trim().to_string();
    match (stream.result_is_error, stream.result_text) {
        (Some(false), text) => Ok(Outcome::Succeeded {
            summary: truncate(text.as_deref().unwrap_or("done"), 4000),
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
            Outcome::Succeeded { summary: detail }
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
                let condensed = if block["is_error"].as_bool().unwrap_or(false) {
                    format!("tool error · {}", truncate(&text, 300))
                } else {
                    format!("{} lines", text.lines().count())
                };
                push_event(app, job.id, "tool_result", condensed);
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
    let event = {
        let mgr = app.state::<JobManager>();
        let mut inner = mgr.lock_inner();
        let out = inner.output.entry(job_id).or_default();
        let event = ProgressEvent {
            seq: out.next_seq,
            kind: kind.to_string(),
            text,
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

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}
