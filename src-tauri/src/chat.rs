//! SPEC §9 — the agent chat: Keychain-backed API key, live model list, and the
//! hand-rolled streaming tool loop against the Anthropic Messages API.
//!
//! Chat deliberately does NOT go through the job runner: it uses the direct
//! pay-per-token API with a console key so its availability never depends on
//! the subscription limits heavy synthesis consumes (SPEC §1). The key lives in
//! the macOS Keychain only — never in the database, never in a file.
//!
//! The loop is: send → stream text and tool calls → run each `tool_use`
//! locally → append `tool_result` → send again, until the model stops asking
//! for tools. Every content block is persisted to `chat_messages` in
//! Messages-API shape, so rebuilding the request after a relaunch is just
//! replaying the rows.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{lock, now, set_setting, setting, truncate, with_conn};
use crate::KEYCHAIN_SERVICE;

const API_BASE: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
/// SPEC §9: the key lives in the macOS Keychain, under the app's own service
/// and this account.
const KEYCHAIN_USER: &str = "anthropic-api-key";
const MODEL_SETTING: &str = "chat_model";
const EFFORT_SETTING: &str = "chat_effort";
const SYSTEM_TEMPLATE: &str = include_str!("../prompts/chat_system.md");
const FOLLOWUP_TEMPLATE: &str = include_str!("../prompts/chat_followups.md");

/// `output_config.effort` — how many tokens Claude spends on a response, tool
/// calls included. Omitting it is the API's `high` default; not every model
/// accepts every level, and one that doesn't rejects the request outright.
const EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The answer's ceiling is the model's own, read from `/v1/models` and
/// remembered per model; effort is what decides how much of it gets used. This
/// is only the floor to fall back on when the list doesn't say.
const FALLBACK_MAX_TOKENS: u64 = 8192;
const MAX_TOKENS_SETTING: &str = "chat_max_tokens";
/// Follow-ups are three short lines; the budget only has to cover a tiny JSON
/// array plus whatever thinking the model does first.
const FOLLOWUP_MAX_TOKENS: u32 = 1024;
/// How much of an answer the follow-up pass reads back.
const FOLLOWUP_ANSWER_CHARS: usize = 6_000;
/// Hard stop on the tool loop — chat is pay-per-token (SPEC §15). Raised from
/// 8 when the write tools landed (M8): a real turn now reads before it writes
/// (search → read → act → confirm), and each round can still carry several
/// parallel tool calls.
const MAX_TOOL_ROUNDS: usize = 12;
/// Tool results dominate a chat's context; keep each one bounded.
const MAX_TOOL_RESULT_CHARS: usize = 24_000;
/// What the sidebar shows behind a chip's disclosure.
const MAX_DETAIL_CHARS: usize = 4_000;

/// In-flight runs, keyed by session — one answer per chat at a time, and the
/// flag is how STOP reaches the streaming thread.
#[derive(Default)]
pub struct ChatState {
    running: Mutex<HashMap<i64, Arc<AtomicBool>>>,
}

// ---------------------------------------------------------------------------
// Payloads

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatSettings {
    pub has_key: bool,
    pub model: Option<String>,
    /// One of `EFFORT_LEVELS`, or `None` for the model's own default.
    pub effort: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    pub id: String,
    pub display_name: String,
    /// The largest `max_tokens` this model accepts, when it publishes one.
    pub max_tokens: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelList {
    pub models: Vec<ModelOption>,
    pub selected: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: i64,
    pub title: String,
    pub created_at: i64,
    pub message_count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredMessage {
    pub id: i64,
    pub role: String,
    /// JSON array of Messages-API content blocks (SPEC §5).
    pub content: String,
    pub created_at: i64,
}

/// Live turn events for the sidebar: `chat-event` carries the session id, so a
/// single global listener covers every chat (mirrors the jobs store).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChatEvent {
    session_id: i64,
    /// The full set, mirrored by `ChatEventPayload` in src/lib/chat.ts:
    /// started | text | thinking | thinking_end | tool | tool_result |
    /// suggestions | done | error. A kind added here without a matching case
    /// there is a silent no-op — the frontend switch has no default arm.
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
    /// Whether this tool changes something — served rather than mirrored.
    #[serde(skip_serializing_if = "Option::is_none")]
    is_write: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    suggestions: Option<Vec<String>>,
}

impl ChatEvent {
    fn new(session_id: i64, kind: &str) -> Self {
        Self {
            session_id,
            kind: kind.to_string(),
            text: None,
            tool_id: None,
            name: None,
            input: None,
            summary: None,
            detail: None,
            is_error: None,
            is_write: None,
            suggestions: None,
        }
    }
}

fn emit(app: &AppHandle, event: ChatEvent) {
    let _ = app.emit("chat-event", event);
}

// ---------------------------------------------------------------------------
// Keychain (SPEC §13: the API key never touches the DB or a file)

fn keychain() -> Result<keyring::v1::Entry> {
    keyring::v1::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER).map_err(|e| {
        anyhow!("the macOS Keychain is unavailable ({e}); store status: {:?}",
            keyring::v1::Entry::store_status())
    })
}

/// One-time migration marker: set once the Keychain item is known to have
/// been created by this signed app (see stored_key's re-own).
const KEY_REOWNED_SETTING: &str = "keychain_reowned";

/// Serializes the re-own's delete→re-add window against concurrent readers,
/// so no overlapping `stored_key` call can observe the item mid-migration
/// and report "no key saved".
static REOWN_GUARD: Mutex<()> = Mutex::new(());

/// Reads the key, self-healing item ownership on the way: an item created by
/// anything else (older builds wrote it through the `security` CLI, whose
/// items land in the Apple-tools protection partition) prompts on every read
/// no matter its ACL; an item this signed app created reads silently, and
/// stays readable across rebuilds because dev-sign.sh keeps the signing
/// identity stable.
///
/// The re-own's delete→re-add is the one moment the Keychain holds no copy
/// of a key the console only ever displays once — so it runs once ever
/// (persistent settings flag, not once per run), retries the write-back, and
/// surfaces a final failure as an error instead of a stderr note.
pub fn stored_key(app: &AppHandle) -> Result<Option<String>> {
    let entry = keychain()?;
    let _guard = lock(&REOWN_GUARD);
    match entry.get_password() {
        Ok(key) => {
            let reowned = with_conn(app, |conn| {
                Ok(setting(conn, KEY_REOWNED_SETTING)?.is_some())
            })
            // An unreadable flag must not open the destructive window.
            .unwrap_or(true);
            if !reowned {
                reown(app, &entry, &key)?;
            }
            Ok(Some(key))
        }
        Err(keyring::v1::Error::NoEntry) => Ok(None),
        Err(e) => Err(anyhow!("reading the API key from the Keychain: {e}")),
    }
}

/// Delete-then-add under REOWN_GUARD. Between the two calls this process's
/// memory holds the only copy, so the add is retried and a final failure
/// returns Err — the current turn fails loudly while the key is still in
/// hand, rather than the next launch discovering an empty Keychain.
fn reown(app: &AppHandle, entry: &keyring::v1::Entry, key: &str) -> Result<()> {
    let _ = entry.delete_credential();
    let mut wrote = entry.set_password(key);
    for _ in 0..2 {
        if wrote.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
        wrote = entry.set_password(key);
    }
    wrote.map_err(|e| {
        anyhow!(
            "the Keychain item was removed for re-owning and could not be \
             written back ({e}) — re-save the API key in chat settings"
        )
    })?;
    mark_reowned(app);
    Ok(())
}

/// Best-effort: a failed flag write only means the (now no-op) re-own runs
/// again on a later read.
fn mark_reowned(app: &AppHandle) {
    let _ = with_conn(app, |conn| set_setting(conn, KEY_REOWNED_SETTING, "1"));
}

/// The app creates its own item, so the ACL pins to the app's code signature
/// — and only holds across rebuilds because dev builds carry a stable Apple
/// Development signature (dev-sign.sh, wired as the cargo runner in
/// `.cargo/config.toml`); an ad-hoc dev build is a new identity every
/// compile and would invalidate the grant. Delete-then-add, never update:
/// updating an existing item keeps the old creator's ACL, which is exactly
/// what a re-save must replace.
pub fn save_key(app: &AppHandle, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        bail!("paste an API key first");
    }
    let entry = keychain()?;
    let _guard = lock(&REOWN_GUARD);
    let _ = entry.delete_credential();
    entry
        .set_password(key)
        .map_err(|e| anyhow!("saving the API key to the Keychain: {e}"))?;
    // A UI save IS an app-created item — no re-own migration needed for it.
    mark_reowned(app);
    Ok(())
}

pub fn delete_key(app: &AppHandle) -> Result<()> {
    match keychain()?.delete_credential() {
        Ok(()) | Err(keyring::v1::Error::NoEntry) => {
            // A future item may come from anywhere (e.g. the old CLI path);
            // clearing the flag keeps the self-heal available for it.
            let _ = with_conn(app, |conn| {
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    [KEY_REOWNED_SETTING],
                )?;
                Ok(())
            });
            Ok(())
        }
        Err(e) => Err(anyhow!("removing the API key from the Keychain: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Settings and models

pub fn settings(app: &AppHandle) -> Result<ChatSettings> {
    let (model, effort) = with_conn(app, |conn| {
        Ok((setting(conn, MODEL_SETTING)?, stored_effort(conn)?))
    })?;
    Ok(ChatSettings {
        has_key: stored_key(app)?.is_some(),
        model,
        effort,
    })
}

pub fn set_model(app: &AppHandle, model: &str) -> Result<()> {
    with_conn(app, |conn| set_setting(conn, MODEL_SETTING, model))
}

/// An empty level means "leave it to the model" — the request then carries no
/// `output_config` at all, which is what older models require.
pub fn set_effort(app: &AppHandle, effort: &str) -> Result<()> {
    if !effort.is_empty() && !EFFORT_LEVELS.contains(&effort) {
        bail!("unknown effort level '{effort}'");
    }
    with_conn(app, |conn| set_setting(conn, EFFORT_SETTING, effort))
}

fn stored_effort(conn: &Connection) -> Result<Option<String>> {
    Ok(setting(conn, EFFORT_SETTING)?
        .filter(|level| EFFORT_LEVELS.contains(&level.as_str())))
}

/// One client for every call, so connections are pooled across the rounds of a
/// turn and across turns — `answer`, `followups` and `fetch_models` share it.
fn http_client() -> Result<&'static reqwest::blocking::Client> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                // An answer takes as long as it takes, so there is no total
                // deadline — but a half-open socket must not park the worker
                // forever inside a read it can never finish, which is what
                // keepalive turns into an ordinary I/O error instead.
                .timeout(None::<Duration>)
                .connect_timeout(Duration::from_secs(20))
                .tcp_keepalive(Duration::from_secs(30))
                .build()
                .ok()
        })
        .as_ref()
        .context("building the HTTP client")
}

fn fetch_models(key: &str) -> Result<Vec<ModelOption>> {
    let response = http_client()?
        .get(format!("{API_BASE}/v1/models?limit=100"))
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .send()
        .context("asking Anthropic for the model list")?;
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        bail!("{}", api_error(status, &body));
    }
    let parsed: Value = serde_json::from_str(&body).context("parsing the model list")?;
    let empty = Vec::new();
    let models: Vec<ModelOption> = parsed["data"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .filter_map(|model| {
            let id = model["id"].as_str()?.to_string();
            let display_name = model["display_name"].as_str().unwrap_or(&id).to_string();
            Some(ModelOption {
                id,
                display_name,
                // Null on models that don't publish one, and 0 in at least one
                // documented response; neither is a usable ceiling.
                max_tokens: model["max_tokens"].as_u64().filter(|n| *n > 0),
            })
        })
        .collect();
    if models.is_empty() {
        bail!("Anthropic returned no models for this key");
    }
    Ok(models)
}

/// SPEC §9: default to the current mid-tier (Sonnet-class) model. `/v1/models`
/// lists newest first, so the first Sonnet in the list is the current one.
fn default_model(models: &[ModelOption]) -> String {
    models
        .iter()
        .find(|m| m.id.to_lowercase().contains("sonnet"))
        .unwrap_or(&models[0])
        .id
        .clone()
}

/// The Settings picker's data: the live list plus the model chat will use.
pub fn list_models(app: &AppHandle) -> Result<ModelList> {
    let key = stored_key(app)?.context("save your Anthropic API key first")?;
    let models = fetch_models(&key)?;
    let stored = with_conn(app, |conn| setting(conn, MODEL_SETTING))?;
    let selected = match stored {
        Some(model) if models.iter().any(|m| m.id == model) => model,
        _ => {
            let picked = default_model(&models);
            with_conn(app, |conn| set_setting(conn, MODEL_SETTING, &picked))?;
            picked
        }
    };
    remember_ceiling(app, &models, &selected)?;
    Ok(ModelList { models, selected })
}

/// The ceiling is stored against the model it belongs to, so switching models
/// can never leave an answer capped at the old one's limit.
fn remember_ceiling(app: &AppHandle, models: &[ModelOption], selected: &str) -> Result<()> {
    let Some(ceiling) = models
        .iter()
        .find(|m| m.id == selected)
        .and_then(|m| m.max_tokens)
    else {
        return Ok(());
    };
    let value = format!("{selected} {ceiling}");
    with_conn(app, |conn| set_setting(conn, MAX_TOKENS_SETTING, &value))
}

/// What a run may spend, which is the whole of what the model allows. Learned
/// from `/v1/models` on first use of a model and remembered after that.
fn max_tokens(app: &AppHandle, key: &str, model: &str) -> u64 {
    let stored = with_conn(app, |conn| setting(conn, MAX_TOKENS_SETTING)).ok().flatten();
    if let Some((id, ceiling)) = stored.as_deref().and_then(|s| s.split_once(' ')) {
        if id == model {
            if let Ok(ceiling) = ceiling.parse::<u64>() {
                return ceiling;
            }
        }
    }
    // Unknown model: one list fetch settles it. A failure here is not worth
    // failing the question over — the fallback still answers, just shorter.
    match fetch_models(key) {
        Ok(models) => {
            let _ = remember_ceiling(app, &models, model);
            models
                .iter()
                .find(|m| m.id == model)
                .and_then(|m| m.max_tokens)
                .unwrap_or(FALLBACK_MAX_TOKENS)
        }
        Err(e) => {
            eprintln!("chat: could not read the model's token ceiling: {e:#}");
            FALLBACK_MAX_TOKENS
        }
    }
}

/// The model for a run: the stored choice, or the Sonnet-class default picked
/// (and remembered) on first use.
fn resolve_model(app: &AppHandle, key: &str) -> Result<String> {
    if let Some(model) = with_conn(app, |conn| setting(conn, MODEL_SETTING))? {
        return Ok(model);
    }
    let models = fetch_models(key)?;
    let picked = default_model(&models);
    with_conn(app, |conn| set_setting(conn, MODEL_SETTING, &picked))?;
    remember_ceiling(app, &models, &picked)?;
    Ok(picked)
}

fn api_error(status: u16, body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v["error"]["message"].as_str())
        .unwrap_or_else(|| body.trim());
    let message = truncate(message, 400);
    match status {
        401 | 403 => format!("Anthropic rejected the API key ({status}) — {message}"),
        429 => format!("Anthropic rate-limited this key (429) — {message}"),
        400 => format!("Anthropic rejected the request (400) — {message}"),
        500..=599 => format!("Anthropic is having trouble ({status}) — {message}"),
        _ => format!("Anthropic returned {status} — {message}"),
    }
}

// ---------------------------------------------------------------------------
// Sessions and history (SPEC §5: chat_sessions / chat_messages)

pub fn list_sessions(conn: &Connection) -> Result<Vec<SessionInfo>> {
    let mut stmt = conn.prepare(
        "SELECT s.id, s.title, s.created_at, COUNT(m.id)
         FROM chat_sessions s LEFT JOIN chat_messages m ON m.session_id = s.id
         GROUP BY s.id ORDER BY s.id DESC LIMIT 50",
    )?;
    let sessions = stmt
        .query_map([], |row| {
            Ok(SessionInfo {
                id: row.get(0)?,
                title: row.get(1)?,
                created_at: row.get(2)?,
                message_count: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(sessions)
}

pub fn history(conn: &Connection, session_id: i64) -> Result<Vec<StoredMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, role, content, created_at FROM chat_messages
         WHERE session_id = ?1 ORDER BY id",
    )?;
    let messages = stmt
        .query_map([session_id], |row| {
            Ok(StoredMessage {
                id: row.get(0)?,
                role: row.get(1)?,
                content: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(messages)
}

fn create_session(conn: &Connection, title: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO chat_sessions (title, created_at) VALUES (?1, ?2)",
        params![title, now()],
    )?;
    Ok(conn.last_insert_rowid())
}

fn insert_message(conn: &Connection, session_id: i64, role: &str, content: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO chat_messages (session_id, role, content, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![session_id, role, content, now()],
    )?;
    Ok(())
}

/// Replays the stored blocks as a Messages-API `messages` array.
fn api_messages(conn: &Connection, session_id: i64) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM chat_messages WHERE session_id = ?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([session_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut messages = Vec::with_capacity(rows.len());
    for (role, content) in rows {
        let blocks: Value =
            serde_json::from_str(&content).context("parsing stored chat content")?;
        messages.push(json!({ "role": role, "content": blocks }));
    }
    Ok(messages)
}

/// A run stopped mid-loop leaves `tool_use` blocks with no results, which the
/// API rejects on the next send. Close them out so the session stays sendable.
fn close_dangling_tool_uses(conn: &Connection, session_id: i64) -> Result<()> {
    let last = conn
        .query_row(
            "SELECT role, content FROM chat_messages WHERE session_id = ?1
             ORDER BY id DESC LIMIT 1",
            [session_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?;
    let Some((role, content)) = last else {
        return Ok(());
    };
    if role != "assistant" {
        return Ok(());
    }
    let blocks: Vec<Value> = serde_json::from_str(&content).unwrap_or_default();
    let results: Vec<Value> = blocks
        .iter()
        .filter(|block| block["type"] == "tool_use")
        .filter_map(|block| block["id"].as_str())
        .map(|id| {
            json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": "Stopped before this tool ran.",
                "is_error": true
            })
        })
        .collect();
    if results.is_empty() {
        return Ok(());
    }
    insert_message(conn, session_id, "user", &Value::Array(results).to_string())
}

fn title_from(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("New chat")
        .trim();
    truncate(line, 64)
}

// ---------------------------------------------------------------------------
// The run

/// Records the question, then answers it on a background thread. Returns the
/// session id (new sessions are created here) so the UI can follow the stream.
///
/// `class_id` is the workspace open when the question was asked, if one was
/// (SPEC §9): it rides the system prompt for this turn only, so a question
/// that names no class means that one. Nothing about it is persisted.
pub fn send(
    app: &AppHandle,
    session_id: Option<i64>,
    text: &str,
    today: &str,
    today_iso: &str,
    class_id: Option<i64>,
) -> Result<i64> {
    let text = text.trim();
    if text.is_empty() {
        bail!("type a question first");
    }
    let key = stored_key(app)?
        .context("no API key yet — add your Anthropic key in the chat settings")?;

    let (session_id, system, effort) = with_conn(app, |conn| {
        let session_id = match session_id {
            Some(id) => id,
            None => create_session(conn, &title_from(text))?,
        };
        // Before the new question goes in, not after: the repair inspects the
        // last row, and once this user message lands the dangling assistant
        // row is no longer last. A crash between persisting a tool_use and its
        // result would otherwise wedge the session permanently — every later
        // send rejected by the API, with no way back from the UI.
        close_dangling_tool_uses(conn, session_id)?;
        // SPEC §9: identity, today's date, the open class, and the injected
        // hub context.
        let system = SYSTEM_TEMPLATE
            .replace("{today}", today)
            .replace("{open_class}", &open_class_line(conn, class_id)?)
            .replace("{context}", &crate::tools::overview_text(conn, false, today_iso)?);
        insert_message(
            conn,
            session_id,
            "user",
            &json!([{ "type": "text", "text": text }]).to_string(),
        )?;
        Ok((session_id, system, stored_effort(conn)?))
    })?;

    let cancel = {
        let state = app.state::<ChatState>();
        let mut running = lock(&state.running);
        if running.contains_key(&session_id) {
            bail!("this chat is still answering — stop it first");
        }
        let flag = Arc::new(AtomicBool::new(false));
        running.insert(session_id, flag.clone());
        flag
    };

    // The question rides the event so the sidebar renders the turn from the
    // stream alone — no optimistic copy to reconcile with the persisted row.
    let mut started = ChatEvent::new(session_id, "started");
    started.text = Some(text.to_string());
    emit(app, started);

    let app = app.clone();
    let question = text.to_string();
    let today = today.to_string();
    let today_iso = today_iso.to_string();
    std::thread::spawn(move || {
        // Model resolution can hit /v1/models on first use; it happens here so
        // the command returns immediately and no HTTP runs on a runtime thread.
        let outcome = resolve_model(&app, &key).and_then(|model| {
            let run = Run {
                session_id,
                key: &key,
                max_tokens: max_tokens(&app, &key, &model),
                model: &model,
                system: &system,
                effort: effort.as_deref(),
                question: &question,
                today: &today,
                today_iso: &today_iso,
            };
            answer(&app, &run, &cancel)
        });
        if let Err(e) = with_conn(&app, |conn| close_dangling_tool_uses(conn, session_id)) {
            eprintln!("chat session {session_id}: closing tool calls failed: {e:#}");
        }
        {
            let state = app.state::<ChatState>();
            let mut running = lock(&state.running);
            // Only clear our own entry: `stop` removes it too, and a fresh run
            // may already hold the slot by the time this thread unwinds.
            if running.get(&session_id).is_some_and(|f| Arc::ptr_eq(f, &cancel)) {
                running.remove(&session_id);
            }
        }
        if let Err(e) = outcome {
            let mut event = ChatEvent::new(session_id, "error");
            event.text = Some(format!("{e:#}"));
            emit(&app, event);
        }
        emit(&app, ChatEvent::new(session_id, "done"));
    });

    Ok(session_id)
}

/// The system prompt's one line about where the question was asked from. An
/// id no class has (a workspace closed by a build that renumbered nothing
/// should not happen, but a stale id must not fail the turn) reads as the
/// dashboard.
fn open_class_line(conn: &Connection, class_id: Option<i64>) -> Result<String> {
    let name: Option<String> = match class_id {
        Some(id) => conn
            .query_row("SELECT display_name FROM classes WHERE id = ?1", [id], |row| row.get(0))
            .optional()?,
        None => None,
    };
    Ok(match name {
        Some(name) => format!(
            "Daniel is looking at the **{name}** workspace. A question that names no class is \
             about this one — pass it as `class` to the tools, and never ask which class he means."
        ),
        None => "Daniel is on the dashboard, looking at no class in particular.".to_string(),
    })
}

pub fn stop(app: &AppHandle, session_id: i64) {
    let state = app.state::<ChatState>();
    // Bind the guard so it drops before `state` does.
    let mut running = lock(&state.running);
    // Removed, not just flagged. The flag is only read between decoded lines,
    // so a stream that stops delivering bytes parks the worker inside the read
    // and it never reaches its own cleanup — leaving the session permanently
    // "still answering" and unsendable. Dropping the entry here frees the
    // session immediately; the flag still tells the orphaned thread to exit if
    // its stream ever resumes, and it holds the last Arc either way.
    if let Some(flag) = running.remove(&session_id) {
        flag.store(true, Ordering::SeqCst);
    }
}

/// Everything one answer needs beyond the app handle.
struct Run<'a> {
    session_id: i64,
    key: &'a str,
    /// The model's own output ceiling — thinking included, per the API.
    max_tokens: u64,
    model: &'a str,
    system: &'a str,
    /// `output_config.effort`, or None for the model's default.
    effort: Option<&'a str>,
    /// The question being answered — context for the follow-up suggestions.
    question: &'a str,
    /// Today, display-formatted and as YYYY-MM-DD — the write tools stamp
    /// job prompts and practice file names with these (tools::ToolCtx).
    today: &'a str,
    today_iso: &'a str,
}

/// The thirteen tool schemas, with a cache breakpoint on the last one.
///
/// A breakpoint covers everything before it, so marking the final schema makes
/// the whole block cacheable — roughly two thousand tokens that would otherwise
/// be re-billed on every round of the loop.
fn cacheable_tools() -> Value {
    let mut tools = crate::tools::definitions();
    if let Some(last) = tools.as_array_mut().and_then(|t| t.last_mut()) {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }
    tools
}

/// SPEC §9 tool loop: send → run `tool_use` locally → append `tool_result` →
/// send again, until the model answers without asking for a tool.
fn answer(app: &AppHandle, run: &Run, cancel: &AtomicBool) -> Result<()> {
    let session_id = run.session_id;
    let client = http_client()?;
    let mut thinking = true;
    for round in 0..=MAX_TOOL_ROUNDS {
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        let messages = with_conn(app, |conn| api_messages(conn, session_id))?;
        let mut body = json!({
            "model": run.model,
            "max_tokens": run.max_tokens,
            // The system prompt is byte-identical across every round of a turn
            // and every turn of a session, and chat is explicitly pay-per-token
            // (SPEC §15) — so it is marked cacheable rather than re-billed up
            // to thirteen times for one question. The tool schemas below get
            // the same treatment for the same reason.
            "system": [{
                "type": "text",
                "text": run.system,
                "cache_control": { "type": "ephemeral" },
            }],
            "messages": messages,
            "stream": true,
        });
        if let Some(effort) = run.effort {
            body["output_config"] = json!({ "effort": effort });
        }
        // Adaptive thinking lets the model decide when to reason, and
        // `summarized` is what makes that reasoning visible in the sidebar;
        // on models that predate it the request is retried without it.
        if thinking {
            body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
        }
        body["tools"] = cacheable_tools();
        // Out of tool budget: answer from what has already been read instead
        // of retrieving forever. `tool_choice: none` is how that is said —
        // dropping `tools` outright is rejected, because by this round the
        // replayed history necessarily carries tool_use/tool_result blocks,
        // and the API requires tools to be defined whenever it does. Doing it
        // that way threw away the answer this guard exists to obtain.
        if round == MAX_TOOL_ROUNDS {
            body["tool_choice"] = json!({ "type": "none" });
        }

        let turn = match stream_turn(app, &client, session_id, run.key, &body, cancel) {
            Err(e) if thinking && rejects_thinking(&e) => {
                thinking = false;
                if let Some(fields) = body.as_object_mut() {
                    fields.remove("thinking");
                }
                stream_turn(app, &client, session_id, run.key, &body, cancel)?
            }
            other => other?,
        };
        if turn.stop_reason.as_deref() == Some("max_tokens") {
            let mut event = ChatEvent::new(session_id, "error");
            event.text = Some(
                "The answer hit the length limit and stopped mid-sentence. \
                 Ask for the rest, or ask something narrower."
                    .to_string(),
            );
            emit(app, event);
        }
        let answered = turn
            .blocks
            .iter()
            .filter(|block| block["type"] == "text")
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if !turn.blocks.is_empty() {
            let content = Value::Array(turn.blocks).to_string();
            with_conn(app, |conn| {
                insert_message(conn, session_id, "assistant", &content)
            })?;
        }
        if cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        if turn.tool_calls.is_empty() {
            // The turn is over; ask for the three next questions worth asking.
            // A failure here costs nothing but the chips, so it never fails
            // the answer that already landed.
            if !answered.trim().is_empty() {
                match followups(&client, run, &answered) {
                    Ok(suggestions) if !suggestions.is_empty() => {
                        let mut event = ChatEvent::new(session_id, "suggestions");
                        event.suggestions = Some(suggestions);
                        emit(app, event);
                    }
                    Ok(_) => {}
                    Err(e) => eprintln!("chat session {session_id}: follow-ups failed: {e:#}"),
                }
            }
            return Ok(());
        }

        let mut results = Vec::with_capacity(turn.tool_calls.len());
        for call in &turn.tool_calls {
            // Executed WITHOUT holding the DB lock: write tools that enqueue
            // jobs re-enter the connection through the job runner, and a held
            // guard here would deadlock. Each tool locks its own window.
            let ctx = crate::tools::ToolCtx {
                today: run.today,
                today_iso: run.today_iso,
            };
            let outcome = crate::tools::execute(app, &call.name, &call.input, &ctx);
            let mut event = ChatEvent::new(session_id, "tool_result");
            event.tool_id = Some(call.id.clone());
            event.summary = Some(outcome.summary.clone());
            event.detail = Some(truncate(&outcome.text, MAX_DETAIL_CHARS));
            event.is_error = Some(outcome.is_error);
            emit(app, event);
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": call.id,
                "content": truncate(&outcome.text, MAX_TOOL_RESULT_CHARS),
                "is_error": outcome.is_error,
            }));
        }
        let content = Value::Array(results).to_string();
        with_conn(app, |conn| insert_message(conn, session_id, "user", &content))?;
    }
    Ok(())
}

/// Older models reject `thinking: {type: "adaptive"}` outright rather than
/// ignoring it, and the model list is whatever the account can see — so the
/// first refusal is taken as the answer and the turn is sent again plainly.
fn rejects_thinking(error: &anyhow::Error) -> bool {
    let message = error.to_string().to_lowercase();
    message.contains("thinking") && (message.contains("400") || message.contains("invalid"))
}

/// One short non-streaming call for the three follow-up questions the sidebar
/// offers under a finished answer. It deliberately re-sends only the last
/// exchange rather than the whole conversation: the suggestions come out just
/// as good, and the turn costs a rounding error.
fn followups(
    client: &reqwest::blocking::Client,
    run: &Run,
    answered: &str,
) -> Result<Vec<String>> {
    let mut body = json!({
        "model": run.model,
        "max_tokens": FOLLOWUP_MAX_TOKENS,
        "system": FOLLOWUP_TEMPLATE,
        "messages": [{
            "role": "user",
            "content": format!(
                "The student asked:\n{}\n\nThe assistant answered:\n{}",
                run.question,
                truncate(answered, FOLLOWUP_ANSWER_CHARS)
            ),
        }],
    });
    // Thinking counts against `max_tokens` on current models, and three short
    // questions do not need it. Asking for low effort keeps the reasoning out
    // of the budget — but only where the answer itself proved the model takes
    // the parameter at all.
    if run.effort.is_some() {
        body["output_config"] = json!({ "effort": "low" });
    }
    let response = client
        .post(format!("{API_BASE}/v1/messages"))
        .header("x-api-key", run.key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .context("asking for follow-up suggestions")?;
    let status = response.status().as_u16();
    let text = response.text().unwrap_or_default();
    if !(200..300).contains(&status) {
        bail!("{}", api_error(status, &text));
    }

    let parsed: Value = serde_json::from_str(&text).context("parsing the follow-up reply")?;
    let reply = parsed["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    // The model was asked for a bare JSON array; tolerate it wrapping the
    // array in prose or a code fence.
    let (start, end) = match (reply.find('['), reply.rfind(']')) {
        (Some(start), Some(end)) if end > start => (start, end),
        _ => bail!("follow-up reply was not a JSON array"),
    };
    let suggestions: Vec<String> = serde_json::from_str(&reply[start..=end])
        .context("parsing the follow-up array")?;
    Ok(suggestions
        .into_iter()
        .map(|s| truncate(s.trim(), 120))
        .filter(|s| !s.is_empty())
        .take(3)
        .collect())
}

#[derive(Default)]
struct Turn {
    blocks: Vec<Value>,
    tool_calls: Vec<ToolCall>,
    stop_reason: Option<String>,
}

struct ToolCall {
    id: String,
    name: String,
    input: Value,
}

enum OpenBlock {
    Text(String),
    Thinking {
        text: String,
        signature: String,
    },
    /// Encrypted reasoning. Nothing here can read it, but the API expects it
    /// back verbatim alongside the tool results of the same turn — dropping it
    /// truncated the assistant message that the very next round replays.
    Redacted(String),
    Tool {
        id: String,
        name: String,
        input_json: String,
    },
}

/// One request/response turn: streams SSE, emitting text deltas and tool chips
/// as they arrive, and returns the assembled content blocks.
fn stream_turn(
    app: &AppHandle,
    client: &reqwest::blocking::Client,
    session_id: i64,
    key: &str,
    body: &Value,
    cancel: &AtomicBool,
) -> Result<Turn> {
    let response = client
        .post(format!("{API_BASE}/v1/messages"))
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(body.to_string())
        .send()
        .context("calling the Anthropic Messages API")?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().unwrap_or_default();
        bail!("{}", api_error(status, &body));
    }

    let mut turn = Turn::default();
    let mut open: Option<OpenBlock> = None;
    // What the round cost, from the API's own accounting: the prompt at
    // `message_start`, the answer at `message_delta`. Chat is pay-per-token
    // (SPEC §15), and the cached system prompt is the one number that says
    // whether the overview is still short enough to ride every turn.
    let mut usage = Value::Null;
    for line in BufReader::new(response).lines() {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let line = line.context("reading the response stream")?;
        // SSE frames also carry `event:` names; the data payload repeats the
        // type, so the names are redundant here.
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        match value["type"].as_str().unwrap_or("") {
            "message_start" => {
                usage = value["message"]["usage"].clone();
            }
            "content_block_start" => {
                let block = &value["content_block"];
                open = match block["type"].as_str() {
                    Some("text") => Some(OpenBlock::Text(String::new())),
                    Some("thinking") => Some(OpenBlock::Thinking {
                        text: String::new(),
                        signature: String::new(),
                    }),
                    Some("redacted_thinking") => Some(OpenBlock::Redacted(
                        block["data"].as_str().unwrap_or_default().to_string(),
                    )),
                    Some("tool_use") => Some(OpenBlock::Tool {
                        id: block["id"].as_str().unwrap_or_default().to_string(),
                        name: block["name"].as_str().unwrap_or_default().to_string(),
                        input_json: String::new(),
                    }),
                    _ => None,
                };
            }
            "content_block_delta" => {
                let delta = &value["delta"];
                match (&mut open, delta["type"].as_str()) {
                    (Some(OpenBlock::Text(buffer)), Some("text_delta")) => {
                        if let Some(chunk) = delta["text"].as_str() {
                            buffer.push_str(chunk);
                            let mut event = ChatEvent::new(session_id, "text");
                            event.text = Some(chunk.to_string());
                            emit(app, event);
                        }
                    }
                    (Some(OpenBlock::Thinking { text, .. }), Some("thinking_delta")) => {
                        if let Some(chunk) = delta["thinking"].as_str() {
                            text.push_str(chunk);
                            let mut event = ChatEvent::new(session_id, "thinking");
                            event.text = Some(chunk.to_string());
                            emit(app, event);
                        }
                    }
                    // The encrypted full reasoning, which has to go back to the
                    // API unchanged for the tool loop to keep its train of
                    // thought (and for the request to validate at all).
                    (Some(OpenBlock::Thinking { signature, .. }), Some("signature_delta")) => {
                        if let Some(chunk) = delta["signature"].as_str() {
                            signature.push_str(chunk);
                        }
                    }
                    (Some(OpenBlock::Tool { input_json, .. }), Some("input_json_delta")) => {
                        if let Some(chunk) = delta["partial_json"].as_str() {
                            input_json.push_str(chunk);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => match open.take() {
                Some(OpenBlock::Text(text)) => {
                    if !text.is_empty() {
                        turn.blocks.push(json!({ "type": "text", "text": text }));
                    }
                }
                Some(OpenBlock::Thinking { text, signature }) => {
                    if !signature.is_empty() {
                        turn.blocks.push(json!({
                            "type": "thinking",
                            "thinking": text,
                            "signature": signature,
                        }));
                    }
                    emit(app, ChatEvent::new(session_id, "thinking_end"));
                }
                Some(OpenBlock::Redacted(data)) => {
                    if !data.is_empty() {
                        turn.blocks
                            .push(json!({ "type": "redacted_thinking", "data": data }));
                    }
                }
                Some(OpenBlock::Tool {
                    id,
                    name,
                    input_json,
                }) => {
                    let input = serde_json::from_str::<Value>(input_json.trim())
                        .unwrap_or_else(|_| json!({}));
                    turn.blocks.push(json!({
                        "type": "tool_use",
                        "id": id.clone(),
                        "name": name.clone(),
                        "input": input.clone(),
                    }));
                    let mut event = ChatEvent::new(session_id, "tool");
                    event.tool_id = Some(id.clone());
                    event.name = Some(name.clone());
                    event.input = Some(input.to_string());
                    event.is_write = Some(crate::tools::is_write(&name));
                    emit(app, event);
                    turn.tool_calls.push(ToolCall { id, name, input });
                }
                None => {}
            },
            "message_delta" => {
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    turn.stop_reason = Some(reason.to_string());
                }
                // Inserted through the map rather than by index: indexing a
                // non-object `Value` panics, and this thread has nothing to
                // catch one — the session would stay "still answering".
                if let (Some(output), Some(fields)) =
                    (value["usage"]["output_tokens"].as_u64(), usage.as_object_mut())
                {
                    fields.insert("output_tokens".to_string(), json!(output));
                }
            }
            "message_stop" => {
                eprintln!(
                    "chat session {session_id}: round used input {} · cache write {} · \
                     cache read {} · output {}",
                    usage["input_tokens"].as_u64().unwrap_or(0),
                    usage["cache_creation_input_tokens"].as_u64().unwrap_or(0),
                    usage["cache_read_input_tokens"].as_u64().unwrap_or(0),
                    usage["output_tokens"].as_u64().unwrap_or(0),
                );
                break;
            }
            "error" => {
                let message = value["error"]["message"]
                    .as_str()
                    .unwrap_or("the stream ended with an error");
                bail!("{}", truncate(message, 400));
            }
            _ => {}
        }
    }
    Ok(turn)
}

