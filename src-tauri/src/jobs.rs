//! SPEC §6 — the Claude Code job runner: the single gateway to the subscription.
//!
//! Queue of max 2 concurrent jobs, each spawning `~/.local/bin/claude -p` with the
//! auth env vars stripped (SPEC §1), raw stream-json persisted to a log file, and
//! condensed progress forwarded to the frontend via `job://{id}/progress` events.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io::{BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
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
        "sort_proposal" | "syllabus_scan" | "announcement_scan" | "notes_review"
        | "card_options" => {
            READ_ONLY_DISALLOWED
        }
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

/// How long a successful self-check stands for. What it verifies (SPEC §1: the
/// subscription, never the API key) is a property of the environment, which
/// does not change between launches on the same day — and two builds on one
/// database, each launched several times a day, had run it over two hundred
/// times. Seconds rather than a `Duration`, because it is compared against
/// `finished_at` in the table.
const SELF_CHECK_INTERVAL: i64 = 24 * 60 * 60;

/// The latest `rate_limit_event` any run streamed, per window (SPEC §6):
/// every one logged so far says `allowed`, and the shift reads these between
/// jobs for the day one does not. Keyed by `rateLimitType`, so a five-hour
/// allowance cannot overwrite a seven-day refusal.
static RATE_LIMIT: Mutex<Option<HashMap<String, RateLimit>>> = Mutex::new(None);

/// How long a refusal that names no reset stands, so one the CLI reports
/// without a time cannot latch for the life of the process.
const UNSAID_RESET: i64 = 5 * 60 * 60;

#[derive(Clone, Debug)]
pub struct RateLimit {
    /// `allowed`, or whatever the CLI says when it is not.
    pub status: String,
    /// `five_hour` or `seven_day`.
    pub window: Option<String>,
    /// When the window resets, unix seconds, where the event said.
    pub resets_at: Option<i64>,
    /// When the event was streamed, unix seconds.
    pub seen_at: i64,
}

impl RateLimit {
    fn describe(&self) -> String {
        let window = match self.window.as_deref() {
            Some("five_hour") => "the five-hour window",
            Some("seven_day") => "the seven-day window",
            _ => "the subscription window",
        };
        match self.resets_at {
            Some(at) if at > now() => {
                let minutes = (at - now()) / 60;
                if minutes >= 60 {
                    format!("{window} resets in {} h {} min", minutes / 60, minutes % 60)
                } else {
                    format!("{window} resets in {minutes} min")
                }
            }
            _ => format!("{window} is at its limit"),
        }
    }
}

/// Whether an event says the limit is reached: a status other than `allowed`
/// whose reset, where it named one, is still ahead — and where it named none,
/// seen within `UNSAID_RESET`. Pure, since the wrong answer is silent either
/// way — a shift that never stops, or one that never starts on an event that
/// lifted hours ago.
pub(crate) fn limit_reached(limit: &RateLimit, now: i64) -> bool {
    limit.status != "allowed"
        && match limit.resets_at {
            Some(at) => at > now,
            None => now < limit.seen_at + UNSAID_RESET,
        }
}

/// What the shift asks between jobs: a reason to stop, when any window's
/// latest event says its limit is reached.
pub fn rate_limit_reached() -> Option<String> {
    let windows = lock(&RATE_LIMIT).clone()?;
    let now = now();
    windows
        .values()
        .find(|limit| limit_reached(limit, now))
        .map(RateLimit::describe)
}

/// Records a window's latest event.
fn record_rate_limit(latest: RateLimit) {
    let key = latest.window.clone().unwrap_or_else(|| "unknown".to_string());
    lock(&RATE_LIMIT).get_or_insert_with(HashMap::new).insert(key, latest);
}

/// Whether the process that owns a row is gone, for a recovery that runs
/// outside this module (the shift's runs).
pub(crate) fn owner_gone(owner: Option<i64>) -> bool {
    is_orphan(owner, std::process::id(), &process_alive)
}

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
    /// The scope as it is shown: a division's name for a `unit:<id>` scope,
    /// the transcript's stem for a session, the folder for a folder.
    pub scope_label: Option<String>,
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
    /// What the row's `scope` column says: a unit or transcript for a guide or
    /// digest, and for a sort job the one inbox file an explicit SORT BY
    /// CONTENT was pressed for (SPEC §7.2).
    scope: Option<String>,
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
        reap_foreign_children();
    }
}

// ---------------------------------------------------------------------------
// Per-kind invocation scoping (SPEC §6)

// Model and effort come from Settings at spawn time (settings.rs; defaults
// Opus at xhigh per the M3 owner decision superseding SPEC §6's per-kind
// split). One global pair covers every job kind.

fn allowed_tools(kind: &str) -> Option<&'static str> {
    match kind {
        // A digest reads one transcript and writes into two folders — the
        // session documents and the corpus note (SPEC §8.5) — and its input is
        // untrusted: whatever was said in the room, or whatever a downloaded
        // caption file contains. Scoping the write means a transcript carrying
        // something shaped like an instruction is refused by the CLI, rather
        // than caught afterwards by a fingerprint diff that walks neither
        // `Study Guides/` nor `.classhub/`.
        //
        // `Edit(<path>)` and not `Write(<path>)`: a Write rule is inert as a
        // path check — the CLI says so outright — while an Edit rule covers
        // every file-editing tool, Write included. Verified both ways against
        // the CLI: a write into the folder is allowed, one outside it refused.
        //
        // The patterns carry no space on purpose, since `--allowedTools`
        // splits on spaces as well as commas and `Study Guides` would arrive as
        // two broken specifiers. Matching the leaf folder avoids that, and the
        // working directory is already the class.
        //
        // The corpus folder is stated twice because it is hidden, and whether a
        // leading `**` crosses a dot-directory is glob-implementation trivia
        // this cannot afford to be wrong about: a pattern that fails to match
        // does not fail loudly, it turns the write into a permission prompt no
        // one is there to answer. Both spellings name the same folder, so the
        // pair costs reach nothing.
        "lecture_digest" => Some(
            "Read,Glob,Grep,Edit(**/Sessions/**),Edit(**/corpus/**),Edit(.classhub/corpus/**)",
        ),
        // The small documents (SPEC §8.6) read untrusted material too — a
        // Canvas description, the owner's drafts under `Project/`, a posted
        // deck — and each writes into one leaf folder under `Study Guides/`,
        // so each gets the digest's shape: an `Edit` rule on its own folder,
        // which the CLI refuses a write outside of before the guard has to
        // read the diff. The pre-read shares the digest's `Sessions/`.
        "assignment_brief" => Some("Read,Glob,Grep,Edit(**/Briefs/**)"),
        "project_workbook" => Some("Read,Glob,Grep,Edit(**/Workbook/**)"),
        "presentation_kit" => Some("Read,Glob,Grep,Edit(**/Presentations/**)"),
        "pre_read" => Some("Read,Glob,Grep,Edit(**/Sessions/**)"),
        "extract" | "module_guide" | "master_guide" | "practice" => {
            Some("Read,Glob,Grep,Write")
        }
        // The notes review never writes: its section is appended by the app
        // through the note write path, with the audit row a job cannot write.
        "sort_proposal" | "syllabus_scan" | "announcement_scan" | "notes_review"
        | "card_options" => {
            Some("Read,Glob,Grep")
        }
        _ => None, // self_check needs no tools
    }
}

/// Kinds that get write tools, and so need their output path checked after.
fn writes_to_disk(kind: &str) -> bool {
    matches!(
        kind,
        "extract"
            | "module_guide"
            | "master_guide"
            | "practice"
            | "lecture_digest"
            | "assignment_brief"
            | "project_workbook"
            | "presentation_kit"
            | "pre_read"
    )
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

/// The audit actions for an app move, whose `from` and `to` the write guard
/// reads as a pair: the file left `from`, and what sits at `to` is the app's
/// only while it still carries the signature it had at `from`. An approval or
/// a by-name click, a file the sync placed, and the undo of either (SPEC §6).
const APP_MOVES: &[&str] = &["sort.move", "canvas.filed", "undo.sort.move", "undo.canvas.filed"];

/// The other audit actions that record the app itself writing into a class
/// folder, with the payload keys naming the class-relative paths it wrote. The
/// guard reads these to tell the app's own writes apart from a job's; an app
/// write into source material that is on neither list is one the guard will
/// pin on whichever job was running.
const APP_WRITES: &[(&str, &[&str])] = &[
    ("sort.staged", &["staged"]),
    ("canvas.staged_file", &["source"]),
    ("lecture.added", &["relPath"]),
    ("chat.write_note", &["relPath"]),
    ("ui.write_note", &["relPath"]),
    ("review.write_note", &["relPath"]),
    ("undo.chat.write_note", &["relPath"]),
    ("undo.ui.write_note", &["relPath"]),
    ("undo.review.write_note", &["relPath"]),
];

/// What the app recorded doing to a path inside the guard's window.
#[derive(Debug, PartialEq)]
enum AppWrite {
    /// The app created, rewrote or removed the file there: the path is the
    /// app's whatever it looks like after the run.
    Wrote,
    /// The app moved a file here from the named path. Exclusion holds only
    /// while the file still has the signature it had there — a job that
    /// rewrote it afterwards is a job that wrote a source, and a rename does
    /// not change size or mtime.
    MovedFrom(String),
}

/// Every path the app recorded writing in `class_id`'s folder since `since`,
/// read out of the audit log — what the write-scope guard subtracts from a
/// job's touched list (SPEC §6).
fn app_written_paths(
    conn: &Connection,
    class_id: i64,
    since: i64,
) -> Result<HashMap<String, AppWrite>> {
    use rusqlite::types::Value;
    let actions = APP_MOVES
        .iter()
        .copied()
        .chain(APP_WRITES.iter().map(|(action, _)| *action));
    // `since` is whole seconds, and the comparison is inclusive on purpose: a
    // row stamped in the same second as the fingerprint, but before it, can
    // only exclude a path the app also wrote — it widens, never narrows.
    let mut params = vec![Value::Integer(since), Value::Integer(class_id)];
    params.extend(actions.map(|action| Value::Text(action.to_string())));
    let placeholders = vec!["?"; params.len() - 2].join(", ");
    // The class is filtered in SQL so a note row's replaced content — up to a
    // megabyte — is not parsed for a class the job is not in. json_extract
    // raises on a payload that is not JSON, and CASE is the one construct
    // guaranteed not to reach it for such a row.
    let mut stmt = conn.prepare(&format!(
        "SELECT action, payload FROM audit_log
         WHERE created_at >= ?
           AND CASE WHEN json_valid(payload) THEN json_extract(payload, '$.classId') END = ?
           AND action IN ({placeholders})"
    ))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut writes: HashMap<String, AppWrite> = HashMap::new();
    for row in rows {
        let (action, payload) = row?;
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload) else {
            continue;
        };
        if APP_MOVES.contains(&action.as_str()) {
            if let (Some(from), Some(to)) = (payload["from"].as_str(), payload["to"].as_str()) {
                // A path a file left is the app's outright, and stays so if a
                // later move brought another file through it; a path a file
                // arrived at is the app's only under the signature rule.
                writes.insert(from.to_string(), AppWrite::Wrote);
                writes
                    .entry(to.to_string())
                    .or_insert_with(|| AppWrite::MovedFrom(from.to_string()));
            }
            continue;
        }
        let Some((_, keys)) = APP_WRITES.iter().find(|(a, _)| *a == action) else {
            continue;
        };
        for key in *keys {
            match &payload[*key] {
                serde_json::Value::String(path) => {
                    writes.insert(path.clone(), AppWrite::Wrote);
                }
                serde_json::Value::Array(list) => {
                    for path in list.iter().filter_map(|v| v.as_str()) {
                        writes.insert(path.to_string(), AppWrite::Wrote);
                    }
                }
                _ => {}
            }
        }
    }
    Ok(writes)
}

/// The touched list with the app's own recorded writes taken out. What is left
/// is the job's. A path the app moved a file to is excluded only while the
/// file still carries the signature it had where it came from; the two
/// fingerprints are the run's before and after pictures.
fn excluding_app_writes(
    mut touched: Vec<String>,
    app_writes: &HashMap<String, AppWrite>,
    before: &HashMap<String, (u64, i64)>,
    after: &HashMap<String, (u64, i64)>,
) -> Vec<String> {
    touched.retain(|path| match app_writes.get(path) {
        None => true,
        Some(AppWrite::Wrote) => false,
        Some(AppWrite::MovedFrom(from)) => match before.get(from) {
            // The move alone leaves that signature intact at the new path;
            // anything else there is the job's.
            Some(signature) => after.get(path) != Some(signature),
            // The file arrived during the window — a drop, a download —
            // under a row of its own, so there is nothing to compare.
            None => false,
        },
    });
    touched
}

// ---------------------------------------------------------------------------
// The job's own log (SPEC §6, §7 step 5)

/// What a finished job's stream log says it saw and touched, class-relative:
/// the paths of every `Read` and the files a `Grep` result names, and the
/// paths of every `Write` and `Edit`. A `Glob` is a listing, not a read, and
/// a path outside the class folder is dropped — a log is the model's own
/// account, and only what sits inside the folder is a source.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct LogPaths {
    pub read: BTreeSet<String>,
    pub written: BTreeSet<String>,
}

/// Where a job's stream log lives: `logs/job-<id>.jsonl` beside the database.
fn log_path_for(app: &AppHandle, job_id: i64) -> Result<PathBuf> {
    Ok(crate::data_dir(app)?.join("logs").join(format!("job-{job_id}.jsonl")))
}

/// Reads a job's log back off disk. `None` when the file could not be read,
/// which the guard treats as strict and the finalizers as an empty read set.
fn job_log_paths(app: &AppHandle, job_id: i64, class_dir: &Path) -> Option<LogPaths> {
    let path = match log_path_for(app, job_id) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("job {job_id}: no log path: {e:#}");
            return None;
        }
    };
    match fs::File::open(&path) {
        Ok(file) => Some(log_paths(
            BufReader::new(file).lines().map_while(Result::ok),
            class_dir,
        )),
        Err(e) => {
            eprintln!("job {job_id}: could not read its log {}: {e}", path.display());
            None
        }
    }
}

/// Every `Read`, `Grep` hit, `Write` and `Edit` in a stream log's lines.
pub(crate) fn log_paths(lines: impl Iterator<Item = String>, class_dir: &Path) -> LogPaths {
    let mut out = LogPaths::default();
    // Greps awaiting their result, by tool_use id: the path each was given.
    let mut greps: HashMap<String, Option<String>> = HashMap::new();
    for line in lines {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let blocks = value["message"]["content"].as_array();
        match value["type"].as_str() {
            Some("assistant") => {
                for block in blocks.into_iter().flatten() {
                    if block["type"].as_str() != Some("tool_use") {
                        continue;
                    }
                    let input = &block["input"];
                    match block["name"].as_str().unwrap_or("") {
                        "Read" => {
                            if let Some(rel) = relativize(input["file_path"].as_str(), class_dir) {
                                out.read.insert(rel);
                            }
                        }
                        "Write" | "Edit" | "MultiEdit" => {
                            if let Some(rel) = relativize(input["file_path"].as_str(), class_dir) {
                                out.written.insert(rel);
                            }
                        }
                        "NotebookEdit" => {
                            if let Some(rel) =
                                relativize(input["notebook_path"].as_str(), class_dir)
                            {
                                out.written.insert(rel);
                            }
                        }
                        "Grep" => {
                            if let Some(id) = block["id"].as_str() {
                                greps.insert(
                                    id.to_string(),
                                    input["path"].as_str().map(str::to_string),
                                );
                            }
                        }
                        // Glob is a listing. Any other tool naming a file is
                        // taken for a write — the CLI's write tools are the
                        // four above today, and a new one must not clear a
                        // change by being unnamed here.
                        "Glob" => {}
                        _ => {
                            let named = input["file_path"]
                                .as_str()
                                .or_else(|| input["notebook_path"].as_str());
                            if let Some(rel) = relativize(named, class_dir) {
                                out.written.insert(rel);
                            }
                        }
                    }
                }
            }
            Some("user") => {
                for block in blocks.into_iter().flatten() {
                    if block["type"].as_str() != Some("tool_result") {
                        continue;
                    }
                    let Some(given) = block["tool_use_id"].as_str().and_then(|id| greps.remove(id))
                    else {
                        continue;
                    };
                    if block["is_error"].as_bool().unwrap_or(false) {
                        continue;
                    }
                    grep_hits(&tool_result_text(block), given.as_deref(), class_dir, &mut out.read);
                }
            }
            _ => {}
        }
    }
    out
}

/// A tool's path, class-relative, or `None` when it falls outside the class
/// folder — absolute and elsewhere, or reaching out through `..`. A leading
/// `./` is the same path spelled longer, and is read as such: the fingerprint
/// names the path bare, and a write the log spells `./Notes/x.md` must meet
/// it.
fn relativize(path: Option<&str>, class_dir: &Path) -> Option<String> {
    let path = path?.trim();
    if path.is_empty() {
        return None;
    }
    let given = Path::new(path);
    let rel = if given.is_absolute() {
        given.strip_prefix(class_dir).ok()?
    } else {
        given
    };
    let mut plain = PathBuf::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => plain.push(part),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    let rel = plain.to_string_lossy().into_owned();
    (!rel.is_empty()).then_some(rel)
}

/// The files a `Grep` result names. Over one file the result names that file
/// — the CLI prints its lines bare — and a result with no hits names none.
/// Over a folder each line opens with a path: bare in a files listing, before
/// the `:` of a content or count line, before the `-` of a context line.
fn grep_hits(text: &str, given: Option<&str>, class_dir: &Path, read: &mut BTreeSet<String>) {
    let text = text.trim();
    if text.is_empty() || text.starts_with("No matches found") || text.starts_with("No files found")
    {
        return;
    }
    if let Some(rel) = relativize(given, class_dir) {
        if class_dir.join(&rel).is_file() {
            read.insert(rel);
            return;
        }
    }
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with("Found ") {
            continue;
        }
        if let Some(rel) = path_prefix(line, class_dir, read) {
            read.insert(rel);
        }
    }
}

/// The longest prefix of a result line that is a file under the class folder:
/// the whole line, else the text before the rightmost `:` or `-` that leaves
/// one. A path already found is matched by its text alone, so a long content
/// result costs one comparison per line rather than a stat per separator.
fn path_prefix(line: &str, class_dir: &Path, known: &BTreeSet<String>) -> Option<String> {
    let is_file = |candidate: &str| {
        relativize(Some(candidate), class_dir).filter(|rel| class_dir.join(rel).is_file())
    };
    if let Some(rel) = known.iter().find(|rel| {
        line.strip_prefix(rel.as_str())
            .or_else(|| {
                class_dir
                    .join(rel.as_str())
                    .to_str()
                    .and_then(|abs| line.strip_prefix(abs))
            })
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(':') || rest.starts_with('-'))
    }) {
        return Some(rel.clone());
    }
    if let Some(rel) = is_file(line) {
        return Some(rel);
    }
    let boundaries: Vec<usize> = line
        .char_indices()
        .filter(|(_, c)| *c == ':' || *c == '-')
        .map(|(i, _)| i)
        .collect();
    for at in boundaries.into_iter().rev() {
        if let Some(rel) = is_file(&line[..at]) {
            return Some(rel);
        }
    }
    None
}

/// The touched list with every path the run's log shows no `Write` or `Edit`
/// to taken out: a change nothing in the log made was not the run's — the
/// owner's own edit beside a running extract — while a logged write outside
/// the contract still fails the job (SPEC §6).
fn excluding_unlogged(mut touched: Vec<String>, written: &BTreeSet<String>) -> Vec<String> {
    touched.retain(|path| written.contains(path));
    touched
}

// ---------------------------------------------------------------------------
// Public API

/// Fails the queued/running rows whose owner process is gone.
///
/// Two processes share this table (SPEC §13), so an active row is not evidence
/// of a crash: the installed app may be running that job right now, and failing
/// it here would leave the row lying while the run continues — and let the
/// extract guard enqueue the same batch a second time. Only a row nobody can
/// vouch for is failed; `is_orphan` says which.
pub fn startup_recovery(conn: &Connection) -> Result<()> {
    let failed = recover_orphans(conn, std::process::id(), process_alive)?;
    if !failed.is_empty() {
        eprintln!("startup recovery failed orphaned job(s) {failed:?}");
    }
    Ok(())
}

/// The ids failed, for the log. `alive` is a parameter so the predicate can be
/// tested without arranging real processes.
fn recover_orphans(
    conn: &Connection,
    self_pid: u32,
    alive: impl Fn(u32) -> bool,
) -> Result<Vec<i64>> {
    let mut stmt =
        conn.prepare("SELECT id, owner_pid FROM jobs WHERE status IN ('queued', 'running')")?;
    let active = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<i64>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let orphaned: Vec<i64> = active
        .into_iter()
        .filter(|(_, owner)| is_orphan(*owner, self_pid, &alive))
        .map(|(id, _)| id)
        .collect();
    for id in &orphaned {
        conn.execute(
            "UPDATE jobs SET status = 'failed',
                    error = 'interrupted — the process running it exited',
                    finished_at = ?1
             WHERE id = ?2",
            params![now(), id],
        )?;
    }
    Ok(orphaned)
}

/// Whether an active row belongs to no one. Startup recovery and the cancel
/// fall-through both ask this, so a row is never judged by its status alone.
///
/// `None` is a row from a build that predates `owner_pid`, and a negative or
/// out-of-range owner is a value no process ever had; nothing can vouch for
/// either, so both are orphaned — the old behaviour, confined to rows the old
/// build made. An owner equal to this process's own id is orphaned too: at
/// startup this process has enqueued nothing, so the pid was reused from one
/// that died, and from the cancel path it is a row this process has no handle
/// on.
fn is_orphan(owner: Option<i64>, self_pid: u32, alive: &impl Fn(u32) -> bool) -> bool {
    match owner.and_then(|pid| u32::try_from(pid).ok()) {
        None => true,
        Some(pid) if pid == self_pid => true,
        Some(pid) => !alive(pid),
    }
}

/// Whether `pid` names a live process. Signal 0 delivers nothing and only
/// reports whether it could have; EPERM means the process exists under another
/// user, which still counts as alive. Pid 0 addresses the caller's own process
/// group and would always answer yes, and no job row can legitimately carry it.
fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs the existence and permission checks
    // without delivering a signal; it has no other effect on any process.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
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
/// JSON batch manifest that extract::finalize_job records on success. `None`
/// when an extract for this class is already active — decided in the same
/// transaction as the insert, because the other process on this database may
/// be enqueuing the same batch at the same moment.
pub fn enqueue_extract(
    app: &AppHandle,
    class_id: i64,
    prompt: &str,
    payload: String,
) -> Result<Option<i64>> {
    enqueue_unique(app, "extract", Some(class_id), None, prompt, Some(payload))
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
    enqueue_unique_scoped(app, "module_guide", class_id, scope, prompt, Some(payload), None)
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
    enqueue_unique_scoped(
        app,
        "master_guide",
        class_id,
        "master",
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
    enqueue_unique_scoped(app, "practice", class_id, scope, prompt, Some(payload), None)
}

/// SPEC §8.4: distills one filed lecture transcript into a session document.
/// `scope` is the transcript's rel path, so the Job Center says which lecture
/// is being worked on. `payload` carries what lectures::finalize_digest needs
/// to record it; the chosen output paths come back on stdout, since the job
/// names its own file.
pub fn enqueue_lecture_digest(
    app: &AppHandle,
    class_id: i64,
    transcript_rel_path: &str,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue_unique_scoped(
        app,
        "lecture_digest",
        class_id,
        transcript_rel_path,
        prompt,
        Some(payload),
        None,
    )
}

/// SPEC §10 step 2: sort_proposal job over a class inbox (read-only tools).
/// The strict JSON it returns on stdout is recorded by sorter::finalize_job.
/// One active sort per class, decided by the insert itself rather than by a
/// check the caller ran a moment earlier: two clicks, or two processes, can
/// both pass such a check, and only the write lock orders them. None means
/// one is already queued or running.
pub fn enqueue_sort(
    app: &AppHandle,
    class_id: i64,
    scope: Option<&str>,
    prompt: &str,
) -> Result<Option<i64>> {
    enqueue_unique(app, "sort_proposal", Some(class_id), scope, prompt, None)
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

/// SPEC §8.6: one of the small documents — a brief, the workbook, a kit, a
/// pre-read — one active per class and scope, its `DocumentPayload` recorded
/// by guides::finalize_document on success.
pub fn enqueue_document(
    app: &AppHandle,
    kind: &str,
    class_id: i64,
    scope: &str,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue_unique_scoped(app, kind, class_id, scope, prompt, Some(payload), None)
}

/// SPEC §8.6: the notes review over one note (read-only tools), the section
/// it answers with appended by notes_review::finalize_job.
pub fn enqueue_notes_review(
    app: &AppHandle,
    class_id: i64,
    note_rel_path: &str,
    prompt: &str,
    payload: String,
) -> Result<i64> {
    enqueue_unique_scoped(
        app,
        crate::notes_review::KIND,
        class_id,
        note_rel_path,
        prompt,
        Some(payload),
        None,
    )
}

/// SPEC §7.2: the announcement scan over a class's unread notices, one per
/// class at a time (read-only tools).
pub fn enqueue_announcement_scan(
    app: &AppHandle,
    class_id: i64,
    prompt: &str,
    payload: String,
) -> Result<Option<i64>> {
    enqueue_unique(app, "announcement_scan", Some(class_id), None, prompt, Some(payload))
}

/// SPEC §12 — multiple-choice options for the cards that have none. Unique per
/// class, so pressing twice or a shift step landing on a class a run is
/// already working through does not ask for the same batch twice.
pub fn enqueue_card_options(
    app: &AppHandle,
    class_id: i64,
    prompt: &str,
    payload: String,
) -> Result<Option<i64>> {
    enqueue_unique(app, "card_options", Some(class_id), None, prompt, Some(payload))
}

/// SPEC §6: self-check asserting the active auth is the subscription. This is
/// the manual re-run behind the auth warning, and it always runs.
pub fn enqueue_self_check(app: &AppHandle) -> Result<i64> {
    set_auth(app, "pending", "Self-check running…");
    enqueue(app, "self_check", None, None, SELF_CHECK_PROMPT, None, None)
}

/// The launch-time self-check. Skipped when the standing verdict is a success
/// within `SELF_CHECK_INTERVAL`, in which case it is restored so the UI reports
/// it rather than "pending"; otherwise enqueued.
pub fn startup_self_check(app: &AppHandle) -> Result<()> {
    let now = now();
    let standing = with_conn(app, |conn| standing_self_check(conn, now))?;
    let Some((finished_at, summary)) = standing else {
        enqueue_self_check(app)?;
        return Ok(());
    };
    let ago = match (now - finished_at).max(0) / 60 {
        m if m < 60 => format!("{m} min ago"),
        m => format!("{} h ago", m / 60),
    };
    let summary = summary
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Subscription auth verified.".to_string());
    set_auth(
        app,
        "ok",
        &format!("{summary} Checked {ago}; the check runs once a day."),
    );
    Ok(())
}

/// The latest self-check verdict, when it is a success within the interval.
/// The *latest* verdict and not the latest success: a failure after a success
/// is what stands, and restoring an older "ok" over it would report auth as
/// working when the last word was that it is not. A row still in flight has
/// no `finished_at` and is not a verdict yet.
fn standing_self_check(conn: &Connection, now: i64) -> Result<Option<(i64, Option<String>)>> {
    let latest: Option<(String, i64, Option<String>)> = conn
        .query_row(
            "SELECT status, finished_at, summary FROM jobs
             WHERE kind = 'self_check' AND finished_at IS NOT NULL
             ORDER BY finished_at DESC, id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    Ok(latest.and_then(|(status, finished_at, summary)| {
        (status == "succeeded" && finished_at >= now - SELF_CHECK_INTERVAL)
            .then_some((finished_at, summary))
    }))
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
    drop(inner);
    // Not this process's job. A child handle lives only in the process that
    // spawned it, so a row another build owns can be cancelled only from
    // there — and the table is what says whether that is the case here.
    let row: Option<(String, Option<i64>)> = with_conn(app, |conn| {
        Ok(conn
            .query_row(
                "SELECT status, owner_pid FROM jobs WHERE id = ?1",
                [job_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    })?;
    let Some((status, owner)) =
        row.filter(|(status, _)| matches!(status.as_str(), "queued" | "running"))
    else {
        bail!("job {job_id} is not queued or running");
    };
    // A live owner elsewhere holds the child handle, so only it can stop the
    // run. Any other active row is a ghost nobody will settle: its owner quit
    // before the worker wrote a final status, it came from a build before
    // `owner_pid`, or it is this process's own row whose worker unwound.
    // Recovery runs only at launch, and the duplicate-active guards read this
    // row until then — so cancelling it here is the way out without a relaunch.
    let self_pid = std::process::id();
    if let Some(pid) = owner.filter(|_| !is_orphan(owner, self_pid, &process_alive)) {
        bail!(
            "job {job_id} is {status} in another ClassHub process (pid {pid}) — \
             cancel it from there"
        );
    }
    with_conn(app, |conn| {
        conn.execute(
            "UPDATE jobs SET status = 'cancelled', finished_at = ?1
             WHERE id = ?2 AND status IN ('queued', 'running')",
            params![now(), job_id],
        )?;
        Ok(())
    })?;
    let _ = app.emit("jobs-changed", ());
    Ok(())
}

pub fn list_jobs(conn: &Connection) -> Result<Vec<JobInfo>> {
    // A unit, pre-read or brief scope is a row id; the division's name or
    // the deadline's title rides along for the label.
    let mut stmt = conn.prepare(&format!(
        "SELECT j.id, j.kind, j.class_id, c.display_name, c.color, j.scope, j.status,
                j.created_at, j.started_at, j.finished_at, j.error, j.summary,
                j.log_path, j.session_id, COALESCE(u.name, d.title)
         FROM jobs j
         LEFT JOIN classes c ON c.id = j.class_id
         {}
         ORDER BY j.id DESC LIMIT 50",
        crate::guides::label_joins("j")
    ))?;
    let jobs = stmt
        .query_map([], |row| {
            let scope: Option<String> = row.get(5)?;
            let unit_name: Option<String> = row.get(14)?;
            Ok(JobInfo {
                id: row.get(0)?,
                kind: row.get(1)?,
                class_id: row.get(2)?,
                class_name: row.get(3)?,
                class_color: row.get(4)?,
                scope_label: scope
                    .as_deref()
                    .map(|s| crate::guides::scope_label(s, unit_name.as_deref())),
                scope,
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
    let id = with_conn(app, |conn| insert_job(conn, kind, class_id, scope, payload.as_deref()))?;
    queue_job(
        app,
        QueuedJob {
            id,
            kind: kind.to_string(),
            class_id,
            scope: scope.map(str::to_string),
            prompt: prompt.to_string(),
            payload,
            resume_session,
        },
    );
    Ok(id)
}

/// `enqueue` for a kind that may have one active row per class. The check and
/// the insert share one IMMEDIATE transaction, so SQLite's write lock is the
/// mutex between this process and the other one on the same database (SPEC
/// §13). A guard the caller ran before its conversions proves nothing by the
/// time they finish: two builds launching in the same minute both pass it, and
/// without this both enqueue the same batch.
fn enqueue_unique(
    app: &AppHandle,
    kind: &str,
    class_id: Option<i64>,
    scope: Option<&str>,
    prompt: &str,
    payload: Option<String>,
) -> Result<Option<i64>> {
    let id = with_conn(app, |conn| {
        insert_unique_job(conn, kind, class_id, scope, payload.as_deref())
    })?;
    let Some(id) = id else {
        return Ok(None);
    };
    queue_job(
        app,
        QueuedJob {
            id,
            kind: kind.to_string(),
            class_id,
            scope: scope.map(str::to_string),
            prompt: prompt.to_string(),
            payload,
            resume_session: None,
        },
    );
    Ok(Some(id))
}

fn insert_job(
    conn: &Connection,
    kind: &str,
    class_id: Option<i64>,
    scope: Option<&str>,
    payload: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO jobs (kind, class_id, scope, status, created_at, payload, owner_pid)
         VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6)",
        params![
            kind,
            class_id,
            scope,
            now(),
            payload,
            i64::from(std::process::id())
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// `enqueue` for a kind that may have one active row per class and scope — a
/// guide per scope, an exam per scope, a digest per transcript. The check and
/// the insert share one IMMEDIATE transaction, as `enqueue_unique` does per
/// class: the `has_active_job` a caller ran before building its prompt is the
/// courtesy message, and this is the guard, since two clicks or two
/// processes can both pass that check and only the write lock orders them.
fn enqueue_unique_scoped(
    app: &AppHandle,
    kind: &str,
    class_id: i64,
    scope: &str,
    prompt: &str,
    payload: Option<String>,
    resume_session: Option<String>,
) -> Result<i64> {
    let id = with_conn(app, |conn| {
        insert_unique_scoped_job(conn, kind, class_id, scope, payload.as_deref())
    })?;
    let Some(id) = id else {
        bail!("a job of this kind is already queued or running for this scope");
    };
    queue_job(
        app,
        QueuedJob {
            id,
            kind: kind.to_string(),
            class_id: Some(class_id),
            scope: Some(scope.to_string()),
            prompt: prompt.to_string(),
            payload,
            resume_session,
        },
    );
    Ok(id)
}

/// `insert_unique_job` keyed on the scope as well as the class.
fn insert_unique_scoped_job(
    conn: &Connection,
    kind: &str,
    class_id: i64,
    scope: &str,
    payload: Option<&str>,
) -> Result<Option<i64>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = ?1 AND class_id = ?2 AND scope = ?3
           AND status IN ('queued', 'running')",
        params![kind, class_id, scope],
        |row| row.get(0),
    )?;
    if active > 0 {
        return Ok(None); // the transaction rolls back on drop
    }
    let id = insert_job(&tx, kind, Some(class_id), Some(scope), payload)?;
    tx.commit()?;
    Ok(Some(id))
}

/// Inserts unless a row of this kind is already queued or running for the
/// class. IMMEDIATE takes the write lock before the read, so another process's
/// identical transaction waits behind this one and then sees its row.
fn insert_unique_job(
    conn: &Connection,
    kind: &str,
    class_id: Option<i64>,
    scope: Option<&str>,
    payload: Option<&str>,
) -> Result<Option<i64>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let active: i64 = tx.query_row(
        "SELECT COUNT(*) FROM jobs
         WHERE kind = ?1 AND class_id IS ?2 AND status IN ('queued', 'running')",
        params![kind, class_id],
        |row| row.get(0),
    )?;
    if active > 0 {
        return Ok(None); // the transaction rolls back on drop
    }
    let id = insert_job(&tx, kind, class_id, scope, payload)?;
    tx.commit()?;
    Ok(Some(id))
}

/// The in-memory half of an enqueue, once the row exists.
fn queue_job(app: &AppHandle, job: QueuedJob) {
    {
        let mgr = app.state::<JobManager>();
        mgr.lock_inner().queue.push_back(job);
    }
    let _ = app.emit("jobs-changed", ());
    pump(app);
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
    // The window the guard answers for opens at the first fingerprint, not at
    // `started_at`: an app move between the two would otherwise be in the
    // "before" picture and outside the audit window at once.
    let window_start = now();
    let before = guarded_dir
        .as_deref()
        .map(crate::scanner::fingerprint_sources);

    let (outcome, log_complete) = match execute_job(&app, &job, &child_slot, &cancelled) {
        Ok((outcome, log_complete)) => (Ok(outcome), log_complete),
        Err(e) => (Err(e), false),
    };
    // The run's own account of what it saw and touched (SPEC §6, §7 step 5),
    // read back off the log it streamed: every Read and Grep hit widens the
    // manifest, every Write and Edit is what the guard may pin on it. A log
    // a write failed into is no account at all — the guard stays strict and
    // the manifest stays what was listed — since the permissive direction
    // needs the stronger evidence.
    let log = if log_complete {
        guarded_dir
            .as_deref()
            .and_then(|dir| job_log_paths(&app, job.id, dir))
    } else {
        eprintln!("job {}: its log is incomplete, so it clears nothing and widens nothing", job.id);
        None
    };
    let empty_reads = BTreeSet::new();
    let read_paths = log.as_ref().map_or(&empty_reads, |l| &l.read);

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
        let after = crate::scanner::fingerprint_sources(dir);
        let mut touched = crate::scanner::diff_fingerprints(&before, &after);
        // The app itself moves files while a job runs — an approved sort, a
        // drop, a lecture filed into Weeks/ — and none of that is the job's
        // doing. Each of those writes an audit row, and the row is what
        // clears a path here: a rename with no row behind it is still the
        // job's, however much it looks like the app's.
        if !touched.is_empty() {
            let app_writes = job
                .class_id
                .map(|class_id| {
                    with_conn(&app, |conn| app_written_paths(conn, class_id, window_start))
                })
                .transpose();
            match app_writes {
                Ok(Some(app_writes)) => {
                    let seen = touched.len();
                    touched = excluding_app_writes(touched, &app_writes, &before, &after);
                    if touched.len() < seen {
                        // The only trace the exclusion leaves: a demotion that
                        // did not happen is invisible otherwise.
                        eprintln!(
                            "job {}: {} change(s) in the class folder were the app's own \
                             audited moves, not the run's",
                            job.id,
                            seen - touched.len()
                        );
                    }
                }
                Ok(None) => {}
                // Unreadable audit log: the guard stays strict rather than
                // assuming every change was the app's.
                Err(e) => eprintln!("job {} could not read the audit log: {e:#}", job.id),
            }
        }
        // A change the log shows no write to was not the run's. An unreadable
        // log leaves the guard strict rather than clearing everything.
        if let (false, Some(log)) = (touched.is_empty(), &log) {
            let seen = touched.clone();
            touched = excluding_unlogged(touched, &log.written);
            let cleared: Vec<&String> = seen.iter().filter(|p| !touched.contains(p)).collect();
            if !cleared.is_empty() {
                // Named, so a clear that should not have happened is at least
                // visible in the process's stderr.
                eprintln!(
                    "job {}: {} change(s) in the class folder have no Write or Edit in the \
                     run's log and were not the run's: {cleared:?}",
                    job.id,
                    cleared.len()
                );
            }
        }
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

    // An extract the model refused is not a stall: left stale, the pipeline
    // would enqueue the same refusal on every scan. What the run did write is
    // recorded, the rest is marked attempted at its hash (SPEC §7 step 3).
    if job.kind == "extract" && status == "failed" {
        if let (Some(class_id), Some(payload), Some(reason)) =
            (job.class_id, job.payload.as_deref(), error.as_deref())
        {
            if crate::extract::refused_by_filter(reason) {
                match crate::extract::record_refusal(&app, class_id, payload, read_paths) {
                    Ok(refused) if !refused.is_empty() => {
                        error = Some(truncate(
                            &format!(
                                "{reason} — left without an extract until the file changes: {}",
                                refused.join(", ")
                            ),
                            2000,
                        ));
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("extract job {} refusal record keeping failed: {e:#}", job.id),
                }
            }
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
                    if let Err(e) = crate::guides::finalize_job(&app, class_id, payload, read_paths) {
                        demote("synthesis finished but no guide was recorded", e);
                    }
                }
            }
            "practice" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    if let Err(e) = crate::guides::finalize_practice(&app, class_id, payload, read_paths) {
                        demote("practice job finished but no exam was written", e);
                    }
                }
            }
            "lecture_digest" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    match crate::lectures::finalize_digest(
                        &app,
                        class_id,
                        payload,
                        result_text.as_deref().unwrap_or(""),
                        read_paths,
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("digest finished but wrote no session document", e),
                    }
                }
            }
            "sort_proposal" => {
                if let Some(class_id) = job.class_id {
                    match crate::sorter::finalize_job(
                        &app,
                        class_id,
                        job.scope.as_deref(),
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
            "announcement_scan" => {
                if let Some(class_id) = job.class_id {
                    match crate::announcements::finalize_job(
                        &app,
                        class_id,
                        job.payload.as_deref(),
                        result_text.as_deref().unwrap_or(""),
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("announcement scan finished but read no notice", e),
                    }
                }
            }
            "card_options" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    match crate::cards::finalize_job(
                        &app,
                        class_id,
                        payload,
                        result_text.as_deref().unwrap_or(""),
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("the card options run wrote no options", e),
                    }
                }
            }
            "assignment_brief" | "project_workbook" | "presentation_kit" | "pre_read" => {
                if let (Some(class_id), Some(payload)) = (job.class_id, job.payload.as_deref()) {
                    if let Err(e) = crate::guides::finalize_document(&app, class_id, payload, read_paths) {
                        demote(
                            &format!("{} finished but no document was recorded", kind_label(&job.kind)),
                            e,
                        );
                    }
                }
            }
            "notes_review" => {
                if let Some(class_id) = job.class_id {
                    match crate::notes_review::finalize_job(
                        &app,
                        class_id,
                        job.payload.as_deref(),
                        result_text.as_deref().unwrap_or(""),
                    ) {
                        Ok(recorded) => summary = Some(recorded),
                        Err(e) => demote("notes review finished but appended nothing", e),
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
    // SPEC §12: one of the two notifications. A cancel is the owner's own
    // doing and says nothing; the self-check's failure has its own dialog.
    if status == "failed" && job.kind != "self_check" {
        let scope = with_conn(&app, |conn| {
            let unit_name: Option<String> = match job.scope.as_deref().and_then(crate::db::unit_scope_id) {
                Some(unit_id) => conn
                    .query_row("SELECT name FROM units WHERE id = ?1", [unit_id], |row| row.get(0))
                    .optional()?,
                None => None,
            };
            Ok(job
                .scope
                .as_deref()
                .map(|s| crate::guides::scope_label(s, unit_name.as_deref())))
        })
        .ok()
        .flatten();
        let what = [Some(kind_label(&job.kind).to_string()), scope]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ");
        crate::notifications::notify(
            &app,
            crate::settings::NOTIFY_JOB_FAILED,
            "A job failed",
            &format!("{what} — {}", error.as_deref().unwrap_or("no reason recorded")),
        );
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

/// Children spawned outside the queue — LibreOffice and Parakeet, which run on
/// the thread that needed them rather than as queued jobs. `shutdown` reaps
/// these too: a transcription run holds a whole model resident, and quitting
/// the app would otherwise reparent it to launchd for the rest of its bound
/// with nobody left to read its output.
static FOREIGN_CHILDREN: Mutex<Vec<Arc<Mutex<Option<Child>>>>> = Mutex::new(Vec::new());

fn register_foreign_child(slot: &Arc<Mutex<Option<Child>>>) {
    let mut all = lock(&FOREIGN_CHILDREN);
    all.retain(|s| lock(s).is_some());
    all.push(slot.clone());
}

fn reap_foreign_children() {
    for slot in std::mem::take(&mut *lock(&FOREIGN_CHILDREN)) {
        if let Some(mut child) = lock(&slot).take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `Command::output()` with a deadline: kills and reaps on expiry rather than
/// blocking forever on a child that never exits.
///
/// Both pipes are drained on their own threads for the duration, not read after
/// the child exits. A child whose output fills the ~64KB pipe buffer blocks in
/// `write` and never exits, so reading afterwards turns a chatty run into a
/// hang that lasts the entire bound — and both callers are quiet only in the
/// ordinary case: the first Parakeet run downloads model weights, and soffice
/// is loud whenever a profile is rebuilt.
pub(crate) fn wait_bounded(mut child: Child, limit: Duration) -> Option<std::process::Output> {
    fn drain<R: std::io::Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    let slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
    register_foreign_child(&slot);

    let deadline = Instant::now() + limit;
    let started = Instant::now();
    let status = loop {
        match lock(&slot).as_mut().map(Child::try_wait) {
            Some(Ok(Some(status))) => break status,
            Some(Ok(None)) => {}
            // Either the wait failed or shutdown took the child out from under
            // us; neither leaves an exit status to report.
            _ => return None,
        }
        if Instant::now() >= deadline {
            if let Some(mut child) = lock(&slot).take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            return None;
        }
        // Tight at first so a quick conversion is not held up by the poll, then
        // slack: the transcription bound is hours, and 25ms for that long is a
        // quarter of a million needless wakeups on battery.
        std::thread::sleep(if started.elapsed() < Duration::from_secs(5) {
            Duration::from_millis(25)
        } else {
            Duration::from_millis(250)
        });
    };
    let _ = lock(&slot).take();

    Some(std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
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

/// Runs the spawn and returns its outcome with whether every stream line
/// reached the log — the guard and the finalizers read the log only when it
/// is the whole account.
fn execute_job(
    app: &AppHandle,
    job: &QueuedJob,
    child_slot: &Arc<Mutex<Option<Child>>>,
    cancelled: &Arc<AtomicBool>,
) -> Result<(Outcome, bool)> {
    let class_dir: Option<PathBuf> = match job.class_id {
        Some(class_id) => Some(with_conn(app, |c| crate::scanner::class_dir(c, class_id))?),
        None => None,
    };
    if let Some(dir) = &class_dir {
        if !dir.is_dir() {
            bail!("class folder not found: {}", dir.display());
        }
    }

    let data_dir = crate::data_dir(app)?;
    let log_path = log_path_for(app, job.id)?;
    if let Some(log_dir) = log_path.parent() {
        fs::create_dir_all(log_dir)?;
    }
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
    // The Settings screen's job model/effort — the kind's own pair, else the
    // global one (SPEC §6) — read at spawn time so a change applies to the
    // very next job, queued jobs included.
    let (model, effort) = crate::settings::job_spawn_options(app, &job.kind);
    let mut cmd = Command::new(&claude);
    cmd.arg("-p")
        .arg(&job.prompt)
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--model", &model])
        .args(["--effort", &effort])
        .args(["--disallowedTools", disallowed_tools(&job.kind)])
        // The user-level claude config leaks MCP servers (e.g. web search)
        // into spawns; with no --mcp-config this loads zero MCP servers.
        .arg("--strict-mcp-config")
        // The user-level config also sets the permission mode, and under its
        // `auto` mode an allow rule restricts nothing: measured 2026-09-09,
        // `Edit(**/Briefs/**)` let a write into `Project/` through with no
        // denial recorded, and under `default` the same rule refused it and
        // allowed the write into `Briefs/`. The mode is stated here so the
        // folder scopes above are the boundary they claim to be, whatever
        // the interactive config says; a run has no one to prompt, so a
        // tool outside its rules is denied, never asked about.
        .args(["--permission-mode", "default"]);
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
    let log_complete = !log_broken;
    if stalled.load(Ordering::SeqCst) {
        return Ok((
            Outcome::Failed {
                error: format!(
                    "no output for {} minutes — the run was stopped as wedged",
                    STALL_LIMIT.as_secs() / 60
                ),
            },
            log_complete,
        ));
    }
    read_result?;

    if job.kind == "self_check" {
        return Ok((resolve_self_check(app, &stream, &lock(&stderr_tail)), log_complete));
    }

    let stderr_text = lock(&stderr_tail).trim().to_string();
    let outcome = match (stream.result_is_error, stream.result_text) {
        (Some(false), text) => Outcome::Succeeded {
            summary: truncate(text.as_deref().unwrap_or("done"), 4000),
            result_text: text,
        },
        (Some(true), text) => Outcome::Failed {
            error: truncate(text.as_deref().unwrap_or("claude reported an error"), 2000),
        },
        (None, _) => Outcome::Failed {
            error: if stderr_text.is_empty() {
                format!("claude exited ({exit}) without a result event")
            } else {
                truncate(&stderr_text, 2000)
            },
        },
    };
    Ok((outcome, log_complete))
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
            let info = &value["rate_limit_info"];
            stream.rate_limit_type = info["rateLimitType"].as_str().map(str::to_string);
            let latest = RateLimit {
                status: info["status"].as_str().unwrap_or("unknown").to_string(),
                window: stream.rate_limit_type.clone(),
                resets_at: info["resetsAt"].as_i64(),
                seen_at: now(),
            };
            if limit_reached(&latest, now()) {
                push_event(
                    app,
                    job.id,
                    "status",
                    format!("rate limit reached — {}", latest.describe()),
                );
            }
            record_rate_limit(latest);
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

/// What a job kind is called on screen — the Settings list's word for it
/// (SPEC §6), the kind itself where the list has none.
pub(crate) fn kind_label(kind: &str) -> &str {
    crate::settings::JOB_KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or(kind, |(_, label)| label)
}

fn count_label(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The first JSON value of the wanted shape in a job's output, tolerating a
/// fenced block or stray prose around it.
///
/// `opens` guards which candidate delimiters count: only when the next
/// non-whitespace byte is one of them is the slice tried at all, so a bracket
/// inside prose — a filename like `[draft] notes.pdf`, or an empty `{}` in a
/// sentence — never wins. The stream deserializer then stops at the value's
/// end, so trailing prose is harmless too.
///
/// Scanned forward, so an outer value always beats the ones nested inside it;
/// searching backwards would return an inner `{...}` and drop the record that
/// contained it.
fn first_json(text: &str, open: char, opens: &[u8], want: fn(&Value) -> bool) -> Option<Value> {
    let bytes = text.as_bytes();
    for (i, _) in text.match_indices(open) {
        let next = bytes[i + 1..].iter().find(|b| !b.is_ascii_whitespace());
        if !next.is_some_and(|b| opens.contains(b)) {
            continue;
        }
        let mut stream = serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>();
        if let Some(Ok(value)) = stream.next() {
            if want(&value) {
                return Some(value);
            }
        }
    }
    None
}

/// Parses the bare-array output contract: `sort_proposal`, and the older shape
/// `syllabus_scan` still answers with often enough to keep accepting.
pub(crate) fn parse_entries(text: &str) -> Result<Vec<Value>> {
    match first_json(text, '[', b"{]", Value::is_array) {
        Some(Value::Array(entries)) => Ok(entries),
        _ => bail!("no JSON array of proposals in the job output"),
    }
}

/// The single-object sibling of `parse_entries`, for jobs whose contract is one
/// record rather than a list (`lecture_digest`, and `syllabus_scan`'s two-part
/// answer). A candidate `{` counts only when
/// a quoted key follows — an empty pair parses perfectly well, and admitting it
/// let one anywhere in the prose shadow the real record.
pub(crate) fn parse_object(text: &str) -> Result<Value> {
    first_json(text, '{', b"\"", Value::is_object)
        .context("no JSON object in the job output")
}

#[cfg(test)]
mod tests {
    use super::{
        app_written_paths, excluding_app_writes, insert_unique_job, is_orphan, limit_reached,
        parse_entries, parse_object, process_alive, recover_orphans, standing_self_check,
        unescape_fragment, wait_bounded, AppWrite, RateLimit,
    };
    use crate::db::now;
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    /// The failure this function's drain threads exist for. A child that writes
    /// past the ~64KB pipe buffer blocks in `write` and never exits, so waiting
    /// first and reading afterwards hangs for the whole bound. Before the drain
    /// this returned `None` after ten seconds; the assertion is that it returns
    /// the output, promptly.
    #[test]
    fn drains_a_child_that_outfills_the_pipe_buffer() {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("yes ....... | head -c 400000; yes ....... | head -c 400000 >&2")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");

        let started = Instant::now();
        let output = wait_bounded(child, Duration::from_secs(10)).expect("child was wedged");
        assert!(output.status.success());
        assert_eq!(output.stdout.len(), 400_000, "stdout was truncated");
        assert_eq!(output.stderr.len(), 400_000, "stderr was truncated");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?} — it blocked on a full pipe",
            started.elapsed()
        );
    }

    #[test]
    fn kills_a_child_that_outlives_its_deadline() {
        let child = Command::new("/bin/sh")
            .arg("-c")
            .arg("sleep 30")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");

        let started = Instant::now();
        assert!(wait_bounded(child, Duration::from_millis(200)).is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "did not stop the child: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn reads_a_lone_object_through_fences_and_prose() {
        let out = "Here is the result:\n```json\n\
                   {\"title\": \"Transformers\", \"relPathHtml\": \"a.html\"}\n```\nDone.";
        let value = parse_object(out).expect("object");
        assert_eq!(value["title"], "Transformers");
        assert_eq!(value["relPathHtml"], "a.html");
    }

    /// The outer record is the contract; an inner one must not be mistaken for
    /// it, which is why the scan runs forward rather than backward.
    #[test]
    fn prefers_the_outer_object_over_a_nested_one() {
        let value = parse_object(r#"{"title": "T", "meta": {"cues": 12}}"#).expect("object");
        assert_eq!(value["title"], "T");
        assert_eq!(value["meta"]["cues"], 12);
    }

    /// A brace inside prose is not a record — the same guard `parse_entries`
    /// applies to stray brackets.
    #[test]
    fn ignores_braces_in_prose_and_reports_an_honest_miss() {
        assert!(parse_object("I wrote it to {the sessions folder}.").is_err());
        assert!(parse_object("no json at all").is_err());
        let value = parse_object("Note {not this}, but {\"title\": \"T\"}").expect("object");
        assert_eq!(value["title"], "T");
    }

    /// An empty pair parses as a perfectly good object, so admitting it let one
    /// anywhere in the prose win the scan and shadow the real record.
    #[test]
    fn an_empty_pair_does_not_win_the_scan() {
        let value = parse_object("Wrote the files {}. Result: {\"title\": \"T\"}")
            .expect("object");
        assert_eq!(value["title"], "T");
        assert!(parse_object("Nothing here: {}").is_err());
    }

    /// Truncated output is a miss, not a partial record.
    #[test]
    fn reports_a_miss_on_truncated_json() {
        assert!(parse_object("{\"title\": \"T\"").is_err());
        assert!(parse_object("{\"title\": ").is_err());
    }


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

    /// The recovery predicate (SPEC §13). Two processes share the table, so an
    /// active row is failed only when nobody can vouch for it: an owner that
    /// has exited, no owner at all (a build before the column), or this
    /// process's own pid, which at startup can only be a reused one.
    #[test]
    fn recovery_fails_only_the_rows_nobody_owns() {
        let conn = crate::db::memory_db();
        let self_pid = 4242u32;
        let other_live = 5000u32;
        let dead = 6000u32;
        let rows: [(i64, Option<i64>, &str); 5] = [
            (1, Some(other_live.into()), "running"), // another process, alive: kept
            (2, Some(dead.into()), "running"),       // its process exited: failed
            (3, None, "queued"),                     // pre-column build: failed
            (4, Some(self_pid.into()), "running"),   // this pid, reused: failed
            (5, Some(dead.into()), "succeeded"),     // already settled: untouched
        ];
        for (id, owner, status) in rows {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, created_at, owner_pid)
                 VALUES (?1, 'extract', ?2, 0, ?3)",
                rusqlite::params![id, status, owner],
            )
            .unwrap();
        }

        let failed = recover_orphans(&conn, self_pid, |pid| {
            pid == other_live || pid == self_pid
        })
        .unwrap();
        assert_eq!(failed, vec![2, 3, 4]);

        let status = |id: i64| -> String {
            conn.query_row("SELECT status FROM jobs WHERE id = ?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(status(1), "running", "a live process's job was failed");
        assert_eq!(status(2), "failed");
        assert_eq!(status(3), "failed");
        assert_eq!(status(4), "failed");
        assert_eq!(status(5), "succeeded");
        let error: Option<String> = conn
            .query_row("SELECT error FROM jobs WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert!(error.is_none(), "a kept row must not carry an error");
    }

    /// `is_orphan` on its own, so the boundary cases read as a table.
    #[test]
    fn the_orphan_predicate_reads_owner_and_liveness() {
        let alive = |pid: u32| pid == 10;
        assert!(is_orphan(None, 1, &alive), "no owner");
        assert!(is_orphan(Some(1), 1, &alive), "own pid at startup is a reused pid");
        assert!(is_orphan(Some(11), 1, &alive), "owner exited");
        assert!(is_orphan(Some(-3), 1, &alive), "garbage owner");
        assert!(!is_orphan(Some(10), 1, &alive), "owner alive");
    }

    /// The liveness probe against the kernel: this process is alive, a reaped
    /// child is not, and pid 0 (the process group) never counts.
    #[test]
    fn process_alive_answers_for_real_processes() {
        assert!(process_alive(std::process::id()));
        let mut child = Command::new("/usr/bin/true").spawn().expect("spawn");
        let pid = child.id();
        child.wait().expect("wait");
        assert!(!process_alive(pid), "a reaped child is not alive");
        assert!(!process_alive(0));
    }

    /// The guard the extract pipeline relies on across processes: an active row
    /// of the same kind for the same class refuses a second insert, while other
    /// classes, other kinds and settled rows are unaffected.
    #[test]
    fn a_unique_insert_yields_to_an_active_row_of_its_kind() {
        let conn = crate::db::memory_db();
        let first = insert_unique_job(&conn, "extract", Some(1), None, Some("{}"))
            .unwrap()
            .expect("the first extract is inserted");
        assert!(
            insert_unique_job(&conn, "extract", Some(1), None, Some("{}"))
                .unwrap()
                .is_none(),
            "a second extract for the class while one is active"
        );
        assert!(insert_unique_job(&conn, "extract", Some(2), None, None).unwrap().is_some());
        assert!(insert_unique_job(&conn, "sort_proposal", Some(1), None, None).unwrap().is_some());

        conn.execute("UPDATE jobs SET status = 'failed' WHERE id = ?1", [first]).unwrap();
        assert!(
            insert_unique_job(&conn, "extract", Some(1), None, None).unwrap().is_some(),
            "a settled row no longer blocks"
        );

        let owner: i64 = conn
            .query_row("SELECT owner_pid FROM jobs WHERE id = ?1", [first], |r| r.get(0))
            .unwrap();
        assert_eq!(owner, i64::from(std::process::id()), "the row is stamped with this process");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM jobs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 4, "the refused insert left no row behind");
    }

    /// The write-scope guard (SPEC §6) against the app's own moves: a rename
    /// during the run's window with an audit row behind it is not the job's,
    /// while the same rename with no row still is. Exclusion is by row, never
    /// by the shape of the change — the guard has to keep catching a job that
    /// moves a source.
    #[test]
    fn the_write_guard_excludes_only_the_moves_the_app_recorded() {
        let dir = std::env::temp_dir().join(format!(
            "classhub-app-moves-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Module 1")).unwrap();
        fs::create_dir_all(dir.join("_Inbox")).unwrap();
        fs::write(dir.join("_Inbox/deck.pdf"), "deck").unwrap();
        fs::write(dir.join("Module 1/notes.md"), "notes").unwrap();

        let conn = crate::db::memory_db();
        let window_start = now();
        let before = crate::scanner::fingerprint_sources(&dir);

        // The window: the app approves a move, a job also rewrites a source.
        // The rewrite changes the length on purpose — mtime is whole seconds,
        // so a same-length rewrite inside the same second would go unseen by
        // the fingerprint itself, which is not what this test is about.
        fs::rename(dir.join("_Inbox/deck.pdf"), dir.join("Module 1/deck.pdf")).unwrap();
        fs::write(dir.join("Module 1/notes.md"), "rewritten by the job").unwrap();
        let after = crate::scanner::fingerprint_sources(&dir);
        assert_ne!(before["Module 1/notes.md"].0, after["Module 1/notes.md"].0);
        let touched = crate::scanner::diff_fingerprints(&before, &after);
        assert_eq!(
            touched,
            vec!["Module 1/deck.pdf", "Module 1/notes.md", "_Inbox/deck.pdf"]
        );

        // No row yet: the rename is the job's as far as the guard can tell.
        let none = app_written_paths(&conn, 1, window_start).unwrap();
        assert_eq!(
            excluding_app_writes(touched.clone(), &none, &before, &after),
            touched
        );

        // The app's row clears both halves of the rename and nothing else.
        crate::db::audit(
            &conn,
            "sort.move",
            serde_json::json!({
                "classId": 1, "proposalId": 7,
                "from": "_Inbox/deck.pdf", "to": "Module 1/deck.pdf",
            }),
        )
        .unwrap();
        let writes = app_written_paths(&conn, 1, window_start).unwrap();
        assert_eq!(
            excluding_app_writes(touched.clone(), &writes, &before, &after),
            vec!["Module 1/notes.md"],
            "the job's own rewrite must survive the exclusion"
        );

        // The row clears the moved file only as the app left it: rewritten by
        // the job after the move, it is reported again.
        fs::write(dir.join("Module 1/deck.pdf"), "deck, rewritten by the job").unwrap();
        let after_rewrite = crate::scanner::fingerprint_sources(&dir);
        let touched_again = crate::scanner::diff_fingerprints(&before, &after_rewrite);
        assert_eq!(
            excluding_app_writes(touched_again, &writes, &before, &after_rewrite),
            vec!["Module 1/deck.pdf", "Module 1/notes.md"],
            "a job rewrite of a file the app moved must not hide behind the move"
        );

        // Another class's row, and a row from before the window, clear nothing.
        assert!(app_written_paths(&conn, 2, window_start).unwrap().is_empty());
        assert!(app_written_paths(&conn, 1, window_start + 3600).unwrap().is_empty());

        // A drop stages a list, a note write names one path: both shapes read.
        // A row whose payload is not JSON is skipped, not fatal.
        crate::db::audit(
            &conn,
            "sort.staged",
            serde_json::json!({ "classId": 1, "staged": ["_Inbox/a.pdf", "_Inbox/b.pdf"] }),
        )
        .unwrap();
        crate::db::audit(
            &conn,
            "ui.write_note",
            serde_json::json!({ "classId": 1, "relPath": "Notes/Today.md", "created": true }),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO audit_log (action, payload, created_at) VALUES ('sort.move', 'not json', ?1)",
            [window_start],
        )
        .unwrap();
        let writes = app_written_paths(&conn, 1, window_start).unwrap();
        for path in ["_Inbox/a.pdf", "_Inbox/b.pdf", "Notes/Today.md"] {
            assert_eq!(writes.get(path), Some(&AppWrite::Wrote), "{path} in {writes:?}");
        }
        assert_eq!(
            writes.get("Module 1/deck.pdf"),
            Some(&AppWrite::MovedFrom("_Inbox/deck.pdf".into()))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The shift's stop rule (SPEC §6): every event logged so far says
    /// `allowed`, and only a status that is not — whose reset, where named,
    /// is still ahead — is a reason to stop. An event that lifted hours ago
    /// must not stop tonight's shift.
    #[test]
    fn the_rate_limit_is_reached_on_a_status_other_than_allowed_with_its_reset_ahead() {
        let allowed = RateLimit {
            status: "allowed".into(),
            window: Some("five_hour".into()),
            resets_at: Some(2_000),
            seen_at: 500,
        };
        assert!(!limit_reached(&allowed, 1_000));
        let reached = RateLimit {
            status: "rejected".into(),
            window: Some("five_hour".into()),
            resets_at: Some(2_000),
            seen_at: 500,
        };
        assert!(limit_reached(&reached, 1_000));
        assert!(!limit_reached(&reached, 2_000), "lifted at its reset");
        let unsaid = RateLimit {
            status: "rejected".into(),
            window: None,
            resets_at: None,
            seen_at: 1_000,
        };
        assert!(limit_reached(&unsaid, 1_000), "no reset named: it stands for a while");
        assert!(
            !limit_reached(&unsaid, 1_000 + super::UNSAID_RESET),
            "and lifts on its own, never for the life of the process"
        );
        // Windows are kept apart: a five-hour allowance does not clear a
        // seven-day refusal.
        super::record_rate_limit(RateLimit {
            status: "rejected".into(),
            window: Some("seven_day".into()),
            resets_at: Some(now() + 3_600),
            seen_at: now(),
        });
        super::record_rate_limit(RateLimit {
            status: "allowed".into(),
            window: Some("five_hour".into()),
            resets_at: Some(now() + 3_600),
            seen_at: now(),
        });
        assert!(super::rate_limit_reached().is_some_and(|why| why.contains("seven-day")));
        super::record_rate_limit(RateLimit {
            status: "allowed".into(),
            window: Some("seven_day".into()),
            resets_at: Some(now() + 3_600),
            seen_at: now(),
        });
        assert!(super::rate_limit_reached().is_none());
    }

    /// The launch gate stands on the latest verdict, not the latest success: a
    /// day-old success has lapsed, a fresh one stands, a row still in flight is
    /// not a verdict, and a failure after a success is the verdict.
    #[test]
    fn the_self_check_gate_stands_on_the_latest_verdict_only() {
        let conn = crate::db::memory_db();
        let now = 1_000_000_i64;
        let insert = |id: i64, status: &str, finished_at: Option<i64>| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, created_at, finished_at, summary)
                 VALUES (?1, 'self_check', ?2, 0, ?3, 'verified')",
                rusqlite::params![id, status, finished_at],
            )
            .unwrap();
        };
        assert!(standing_self_check(&conn, now).unwrap().is_none(), "no check yet");
        insert(1, "succeeded", Some(now - 25 * 3600));
        assert!(
            standing_self_check(&conn, now).unwrap().is_none(),
            "a day-old success has lapsed"
        );
        insert(2, "succeeded", Some(now - 3600));
        assert_eq!(
            standing_self_check(&conn, now).unwrap().map(|(at, _)| at),
            Some(now - 3600),
            "a fresh success stands"
        );
        insert(3, "running", None);
        assert!(
            standing_self_check(&conn, now).unwrap().is_some(),
            "a check in flight is not a verdict"
        );
        insert(4, "failed", Some(now - 60));
        assert!(
            standing_self_check(&conn, now).unwrap().is_none(),
            "a later failure is the verdict, and the check re-runs"
        );
    }
    /// A unit scope is a row id, so the listing carries the division's name
    /// for the label; a scope from before ids shows what it carries.
    #[test]
    fn a_job_s_scope_is_labelled_by_the_division_s_name() {
        let conn = crate::db::memory_db();
        conn.execute(
            "INSERT INTO units (id, class_id, ordinal, kind, name, source)
             VALUES (24, 1, 2, 'week', 'Week 2 — Responsible AI', 'syllabus')",
            [],
        )
        .expect("unit");
        for scope in [
            "unit:24",
            "unit:Week 9",
            "master",
            "Module 1",
            "session:Weeks/Week 02/2026-09-01 — Lecture.md",
        ] {
            conn.execute(
                "INSERT INTO jobs (kind, class_id, scope, status, created_at, owner_pid)
                 VALUES ('module_guide', 1, ?1, 'succeeded', 1, 1)",
                [scope],
            )
            .expect("job");
        }
        let jobs = super::list_jobs(&conn).expect("list");
        let label = |scope: &str| {
            jobs.iter()
                .find(|j| j.scope.as_deref() == Some(scope))
                .and_then(|j| j.scope_label.clone())
                .expect(scope)
        };
        assert_eq!(label("unit:24"), "Week 2 — Responsible AI");
        assert_eq!(label("unit:Week 9"), "Week 9");
        assert_eq!(label("master"), "Semester Master");
        assert_eq!(label("Module 1"), "Module 1");
        assert_eq!(label("session:Weeks/Week 02/2026-09-01 — Lecture.md"), "2026-09-01 — Lecture");
    }
    /// The log reader (SPEC §6, §7 step 5): a Read counts, a Grep names the
    /// file it hit — the one it was given, or each line's path over a folder
    /// — a Glob is a listing, a Grep with no hits names nothing, a path
    /// outside the class folder is dropped, and a Write or Edit is a write.
    #[test]
    fn the_log_reader_names_reads_grep_hits_and_writes() {
        use super::log_paths;
        use serde_json::Value;
        use std::collections::BTreeSet;
        use std::fs;
        let dir = std::env::temp_dir().join(format!("classhub-log-reader-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        for rel in ["Notes/a.md", ".classhub/corpus/Week 3/note.md", "Slides/deck.pptx"] {
            let path = dir.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, rel).unwrap();
        }
        let abs = |rel: &str| dir.join(rel).to_string_lossy().into_owned();
        let call = |id: &str, name: &str, input: Value| {
            serde_json::json!({"type": "assistant", "message": {"content": [
                {"type": "tool_use", "id": id, "name": name, "input": input}]}})
            .to_string()
        };
        let result = |id: &str, text: &str| {
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": id, "content": text}]}})
            .to_string()
        };
        let lines = vec![
            call("r1", "Read", serde_json::json!({"file_path": abs("Notes/a.md")})),
            call("g1", "Glob", serde_json::json!({"pattern": "**/*"})),
            result("g1", "Notes/a.md\nSlides/deck.pptx"),
            call("gr1", "Grep", serde_json::json!({"pattern": "^#", "path": abs(".classhub/corpus/Week 3/note.md")})),
            result("gr1", "3:## Domains"),
            call("gr2", "Grep", serde_json::json!({"pattern": "x", "output_mode": "content"})),
            result("gr2", "Slides/deck.pptx:12:text\nNotes/a.md-3-context\n\nFound 2 total occurrences across 2 files."),
            call("gr3", "Grep", serde_json::json!({"pattern": "y", "path": abs("Notes")})),
            result("gr3", "No matches found"),
            call("gr4", "Grep", serde_json::json!({"pattern": "z", "path": abs("Slides/deck.pptx")})),
            serde_json::json!({"type": "user", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "gr4", "is_error": true, "content": "denied"}]}})
            .to_string(),
            call("r2", "Read", serde_json::json!({"file_path": "/etc/hosts"})),
            call("r3", "Read", serde_json::json!({"file_path": abs("../elsewhere/x.md")})),
            call("w1", "Write", serde_json::json!({"file_path": abs("Study Guides/g.html"), "content": "x"})),
            call("e1", "Edit", serde_json::json!({"file_path": abs("Study Guides/g.html")})),
            call("w2", "Write", serde_json::json!({"file_path": abs("Slides/deck.pptx")})),
            // The same path spelled longer, and a write tool this reader
            // does not know by name: both are writes.
            call("w3", "Write", serde_json::json!({"file_path": "./Notes/b.md"})),
            call("w4", "FancyWrite", serde_json::json!({"file_path": abs("Notes/c.md")})),
            "not json at all".to_string(),
        ];
        let paths = log_paths(lines.into_iter(), &dir);
        let set = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        assert_eq!(
            paths.read,
            set(&["Notes/a.md", ".classhub/corpus/Week 3/note.md", "Slides/deck.pptx"])
        );
        assert_eq!(
            paths.written,
            set(&["Study Guides/g.html", "Slides/deck.pptx", "Notes/b.md", "Notes/c.md"])
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The guard's third exclusion (SPEC §6): a changed path the log shows no
    /// write to was not the run's; one it does show a write to stays, and an
    /// empty log clears everything the run never wrote.
    #[test]
    fn the_guard_drops_a_change_the_log_shows_no_write_to() {
        use super::excluding_unlogged;
        use std::collections::BTreeSet;
        let touched = vec!["Module 1/notes.md".to_string(), "Slides/deck.pptx".to_string()];
        let written: BTreeSet<String> = ["Slides/deck.pptx".to_string()].into_iter().collect();
        assert_eq!(excluding_unlogged(touched.clone(), &written), vec!["Slides/deck.pptx"]);
        assert!(excluding_unlogged(touched, &BTreeSet::new()).is_empty());
    }
}
