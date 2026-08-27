//! SPEC §7.2 — reading course structure from Canvas.
//!
//! Canvas holds the authoritative version of everything this app otherwise
//! infers from hand-made folders: the course's own divisions, its files, its
//! assignments and their real due dates. Getting at it is the whole problem,
//! because UF has closed both credentialed paths (SPEC §1) — personal access
//! tokens are disabled in the UI, and OAuth2 needs an admin-issued developer
//! key. Neither is a fallback for the other; they are the same gate.
//!
//! What remains is the session the user already has. `/api/v1` honours the
//! browser's own cookie for same-origin GETs — it is how Canvas's own web UI
//! and in-Canvas theme JavaScript query it — so a webview the user signed in
//! becomes the client. That is the same posture `zoom.rs` takes: the remote
//! origin is read, and never granted IPC into the app.
//!
//! **In-page is the requirement, not a convenience.** The request has to
//! originate from a page Canvas served. Reading the cookies out with
//! `cookies_for_url` and replaying them from `reqwest` is the wrong shape and
//! invites a referer or CSRF refusal — and it buys nothing, since only reads
//! are ever performed. So every request goes through `eval_with_callback`
//! running `fetch(..., {credentials: 'same-origin'})` inside the page.
//!
//! Two consequences worth stating plainly:
//!
//! - **Nothing is stored.** No token is minted, nothing goes in the Keychain or
//!   the settings table. The app's reach expires with the session, which is a
//!   narrower exposure than the token the administrators disabled. It also
//!   means there is no "connected" state to show: a sync either finds a live
//!   session or asks for a sign-in, and the second is ordinary rather than an
//!   error.
//! - **This is undocumented.** Session auth appears nowhere in Instructure's
//!   OAuth2 or access-token references (SPEC §1). If a Canvas change breaks it,
//!   the answer is the syllabus fallback, not something cleverer.
//!
//! GETs need no `X-CSRF-Token`, and ClassHub never writes to Canvas, so that
//! dance never arises. REST rather than GraphQL for the same reason: GraphQL is
//! a POST and would need the token, while the REST rate limit (700 requests per
//! 10 minutes) is nowhere near binding for four courses.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const CANVAS_HOST: &str = "ufl.instructure.com";
const CANVAS_ORIGIN: &str = "https://ufl.instructure.com/";
const WINDOW_LABEL: &str = "canvas-session";

/// Long enough for Shibboleth plus a Duo push that goes to a phone in another
/// room — but bounded, so a forgotten window is not a hang.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SIGN_IN_POLL: Duration = Duration::from_millis(1_000);
/// One API call, including the poll for its in-page completion.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(90);
const REQUEST_POLL: Duration = Duration::from_millis(250);
/// One `eval` round-trip. Generous: the page is busy while Canvas loads.
const EVAL_TIMEOUT: Duration = Duration::from_secs(20);
/// How long the window stays hidden before being shown anyway.
///
/// A live session should be read without ever putting a window on screen, which
/// is the point of starting hidden. But macOS suspends an occluded WKWebView's
/// JavaScript under some conditions, and a webview that never runs the probe is
/// indistinguishable from one whose session expired. Showing the window after a
/// grace period costs a visible window in the slow case and turns that failure
/// mode into a working sync.
const HIDDEN_GRACE: Duration = Duration::from_secs(8);
/// Pagination backstop. Four courses cannot approach this; a `Link` header that
/// cycles could, and 700 requests / 10 minutes is a real limit to stay inside.
const MAX_PAGES: usize = 50;
/// A course file over whatever connection is to hand. Bounded, because a
/// stalled socket otherwise parks the sync thread indefinitely.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// The largest file worth pulling through the page.
///
/// File bytes come back base64 through `eval_with_callback` (see `download`),
/// which holds the whole thing in memory several times over — so this is a
/// real ceiling rather than a formality. Lecture decks run to single-digit
/// megabytes; anything past this is a video, which belongs in the lecture
/// ingest flow (SPEC §7.1) rather than in a file sync.
pub const MAX_DOWNLOAD_BYTES: i64 = 48 * 1024 * 1024;

/// A live Canvas session — in practice, the webview the reads are issued from.
///
/// The window *is* the client. It stays open for the duration of a sync and is
/// closed after, because there is no credential to keep: closing it is what
/// ends the app's reach.
///
/// That makes closing it an obligation rather than a courtesy, so it belongs to
/// the value's lifetime. Dropping a `Session` closes its window — including on
/// the paths where `open` itself fails, which would otherwise leave a signed-in
/// webview alive with no way for anyone to see or close it.
pub struct Session {
    app: AppHandle,
    window: WebviewWindow,
    shown: AtomicBool,
    counter: AtomicU64,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.window.close();
    }
}

/// The page reporting that a fetch never completed — WebKit's bare "Load
/// failed" and its kin, which carry no status because no response arrived.
///
/// A type rather than a phrase to recognize later. The retry decision used to
/// re-read the rendered error chain, which by then also carried the request
/// path and up to 200 characters of Canvas's own response body — so an
/// unrelated failure whose text happened to contain "network" earned a retry,
/// and a WebKit rewording would have quietly stopped one that deserved it.
/// Anything that reaches `fetch`'s `catch` is a transport failure by
/// construction; a refusal resolves instead, with a status.
#[derive(Debug)]
pub struct PageFailure(pub String);

impl std::fmt::Display for PageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PageFailure {}

enum Outcome {
    Json { value: Value, next: Option<String> },
    /// A file's bytes, base64 as the page encoded them.
    Binary(String),
    /// Canvas served the page but refused the call — the session lapsed.
    Unauthorized(u64),
    /// The window is somewhere else entirely, i.e. mid-SSO at login.ufl.edu.
    /// No fetch is attempted: it would be cross-origin, and the answer to a
    /// cross-origin failure would look like a Canvas problem rather than an
    /// unfinished sign-in.
    Offsite(String),
}

/// What a request is for. The three differ in how the answer is read and in
/// whether a slow one is worth showing the window for.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Polling `users/self` while waiting for a sign-in. Revealing the window
    /// is already the deliberate step there, so the grace timer stays out of it.
    SignIn,
    Json,
    Binary,
}

impl Session {
    /// Opens the Canvas window and returns once `/api/v1/users/self` answers.
    ///
    /// Starts hidden, so a sync with a live session never puts a window on
    /// screen. The window is shown only when Canvas actually wants a sign-in.
    pub fn open(app: &AppHandle, on_stage: &dyn Fn(&str)) -> Result<Session> {
        close_existing(app)?;
        let url = tauri::Url::parse(CANVAS_ORIGIN).context("parsing the Canvas origin")?;
        let window = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(url))
            .title("Canvas — sign in so ClassHub can read your courses")
            .inner_size(1100.0, 860.0)
            .visible(false)
            .build()
            .context("opening the Canvas window")?;

        let session = Session {
            app: app.clone(),
            window,
            shown: AtomicBool::new(false),
            counter: AtomicU64::new(0),
        };
        // Built before the probe, so a refusal here drops `session` and its
        // `Drop` closes the window. Returning the error without that would
        // strand a hidden, signed-in webview the user cannot even see.
        session.await_session(on_stage)?;
        Ok(session)
    }

    /// Every page of a paginated collection, concatenated.
    ///
    /// Canvas paginates through the `Link` header and defaults to 10 items per
    /// page. Taking the first page for the whole collection looks exactly like
    /// success — a class with 50 files would report 10 — so following `next` is
    /// not an optimization, it is the difference between right and quietly
    /// wrong. `per_page=100` keeps that to one request in practice.
    pub fn get_all(&self, path: &str, on_stage: &dyn Fn(&str)) -> Result<Vec<Value>> {
        let mut url = with_per_page(path);
        let mut items: Vec<Value> = Vec::new();
        for _ in 0..MAX_PAGES {
            let (value, next) = self.get_page(&url, on_stage)?;
            match value {
                Value::Array(page) => items.extend(page),
                other => bail!("{path} returned {} rather than a list", kind_of(&other)),
            }
            match next {
                Some(n) => url = n,
                None => return Ok(items),
            }
        }
        bail!("{path} kept paginating past {MAX_PAGES} pages — stopping rather than looping")
    }

    fn get_page(&self, path: &str, on_stage: &dyn Fn(&str)) -> Result<(Value, Option<String>)> {
        match self.request(path, Mode::Json)? {
            Outcome::Json { value, next } => Ok((value, next)),
            // The session lapsed partway through a sync. Expected rather than
            // exceptional — nothing is stored, so this is simply what time
            // passing looks like. Sign in again and repeat the request.
            Outcome::Unauthorized(_) | Outcome::Offsite(_) => {
                self.await_session(on_stage)?;
                match self.request(path, Mode::Json)? {
                    Outcome::Json { value, next } => Ok((value, next)),
                    Outcome::Unauthorized(status) => {
                        bail!("Canvas refused {path} with {status} even after signing in")
                    }
                    // Worth naming: landing on login.ufl.edu means SSO bounced
                    // rather than that Canvas is broken, and those need
                    // different things from the reader.
                    Outcome::Offsite(host) => {
                        bail!("the Canvas window ended up on {host} rather than {CANVAS_HOST}")
                    }
                    Outcome::Binary(_) => bail!("{path} answered with a file rather than data"),
                }
            }
            Outcome::Binary(_) => bail!("{path} answered with a file rather than data"),
        }
    }

    /// Polls `/api/v1/users/self` until it answers, showing the window and
    /// asking for a sign-in as soon as Canvas indicates it wants one.
    fn await_session(&self, on_stage: &dyn Fn(&str)) -> Result<()> {
        let deadline = Instant::now() + SIGN_IN_TIMEOUT;
        let mut asked = false;
        loop {
            match self.request("/api/v1/users/self", Mode::SignIn)? {
                Outcome::Json { .. } | Outcome::Binary(_) => {
                    if asked {
                        on_stage("Signed in — reading your courses…");
                    }
                    return Ok(());
                }
                Outcome::Unauthorized(_) | Outcome::Offsite(_) => {
                    if !asked {
                        asked = true;
                        self.reveal();
                        on_stage("Waiting for you to sign in to Canvas…");
                    }
                }
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for the Canvas sign-in. Nothing was read, and \
                     nothing is stored — starting the sync again reopens the window."
                );
            }
            std::thread::sleep(SIGN_IN_POLL);
        }
    }

    /// Issues one in-page fetch and polls until it settles.
    ///
    /// The slot the page parks the answer in belongs to this call either way,
    /// so it is released here rather than by the script that reads it — see
    /// `release`.
    fn request(&self, path: &str, mode: Mode) -> Result<Outcome> {
        let id = format!("q{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let outcome = self.poll(&id, path, mode);
        self.release(&id);
        outcome
    }

    /// Frees a request's slot on the page, best-effort and without waiting.
    ///
    /// Deleting inside the script that returns the answer looked simpler and
    /// was wrong: the value still has to survive being marshalled out of the
    /// page, and if that round trip exceeds `EVAL_TIMEOUT` the result is gone
    /// while Rust believes nothing was ever started — so the next poll refetches
    /// a file that had already arrived. Clearing from this side means the slot
    /// is released exactly when the answer is in hand.
    ///
    /// Also the only cleanup a timed-out request gets. For a download the slot
    /// holds the whole file base64, which is not something to leave on the page
    /// for the rest of the sync.
    fn release(&self, id: &str) {
        let js = RELEASE_JS.replace("__ID__", &js_string(id));
        let _ = self.window.eval_with_callback(&js, |_| {});
    }

    fn poll(&self, id: &str, path: &str, mode: Mode) -> Result<Outcome> {
        let script = REQUEST_JS
            // `__BINARY__` and `__MAXBYTES__` first: substituting `__PATH__`
            // ahead of them would let a URL containing one of those literals
            // rewrite itself inside its own string.
            .replace("__BINARY__", if mode == Mode::Binary { "true" } else { "false" })
            .replace("__MAXBYTES__", &MAX_DOWNLOAD_BYTES.to_string())
            .replace("__ID__", &js_string(id))
            .replace("__HOST__", &js_string(CANVAS_HOST))
            .replace("__PATH__", &js_string(path));

        let started = Instant::now();
        // A file has to be read, encoded and marshalled whole, none of which a
        // 90-second API budget covers.
        let budget = if mode == Mode::Binary {
            DOWNLOAD_TIMEOUT
        } else {
            REQUEST_TIMEOUT
        };
        let deadline = started + budget;
        // Kept for the timeout message. When the eval itself is what keeps
        // failing — a page that never finished loading, a script that threw —
        // the state is empty precisely when knowing why would help most.
        let mut last_error: Option<String> = None;
        loop {
            self.ensure_open()?;
            match self.eval(&script) {
                // A page mid-navigation cannot answer, and SSO navigates
                // several times. Ordinary, so it is polled through rather than
                // reported — the deadline is what ends this loop.
                Err(e) => last_error = Some(format!("{e:#}")),
                Ok(mut v) => match v["state"].as_str().unwrap_or_default().to_string().as_str() {
                    "pending" => {}
                    "offsite" => {
                        return Ok(Outcome::Offsite(
                            v["host"].as_str().unwrap_or("somewhere else").to_string(),
                        ))
                    }
                    "error" => {
                        let message = v["message"].as_str().unwrap_or("no reason given");
                        return Err(PageFailure(message.to_string()))
                            .with_context(|| format!("the Canvas page could not request {path}"));
                    }
                    // Refused before the bytes were read, so the ceiling costs
                    // one header rather than the memory it exists to bound.
                    "toolarge" => {
                        let bytes = v["bytes"].as_i64().unwrap_or(-1);
                        bail!(
                            "the file is {} — past the {} that can be pulled through the Canvas \
                             page; download it from Canvas directly",
                            if bytes < 0 {
                                "larger than the ceiling".to_string()
                            } else {
                                crate::tools::format_size(bytes)
                            },
                            crate::tools::format_size(MAX_DOWNLOAD_BYTES)
                        );
                    }
                    "response" => {
                        let status = v["status"].as_u64().unwrap_or(0);
                        if status == 401 || status == 403 {
                            return Ok(Outcome::Unauthorized(status));
                        }
                        if !(200..300).contains(&status) {
                            bail!(
                                "Canvas answered {status} for {path}: {}",
                                crate::db::truncate(
                                    v["body"].as_str().unwrap_or_default().trim(),
                                    200
                                )
                            );
                        }
                        if mode == Mode::Binary {
                            // Taken rather than cloned: this string *is* the
                            // file, so a copy of it costs another whole file.
                            return Ok(Outcome::Binary(match v["b64"].take() {
                                Value::String(encoded) => encoded,
                                _ => bail!("the page returned no bytes for {path}"),
                            }));
                        }
                        let next = next_link(v["link"].as_str().unwrap_or_default());
                        return Ok(Outcome::Json {
                            value: parse_body(v["body"].as_str().unwrap_or_default())
                                .with_context(|| format!("reading Canvas's answer to {path}"))?,
                            next,
                        });
                    }
                    other => bail!("the Canvas page reported an unknown state: {other}"),
                },
            }

            if Instant::now() >= deadline {
                match last_error {
                    Some(why) => bail!(
                        "Canvas did not answer {path} within {}s — the page kept failing to run \
                         the request: {why}",
                        budget.as_secs()
                    ),
                    None => bail!("Canvas did not answer {path} within {}s", budget.as_secs()),
                }
            }
            // A read taking this long is not a live session behaving normally,
            // so stop hiding what is happening.
            if mode != Mode::SignIn && started.elapsed() >= HIDDEN_GRACE {
                self.reveal();
            }
            std::thread::sleep(REQUEST_POLL);
        }
    }

    /// Evaluates an expression in the page and returns its JSON result.
    ///
    /// The callback fires once; a script that throws never calls back (Tauri
    /// swallows the exception), which the timeout covers.
    fn eval(&self, js: &str) -> Result<Value> {
        let (tx, rx) = mpsc::channel();
        self.window
            .eval_with_callback(js, move |result| {
                let _ = tx.send(result);
            })
            .context("evaluating in the Canvas window")?;
        let raw = rx
            .recv_timeout(EVAL_TIMEOUT)
            .context("the Canvas page did not answer")?;
        serde_json::from_str(&raw).with_context(|| format!("the page returned {raw:?}"))
    }

    /// A closed window is the user cancelling, not a failure to report.
    /// Presence is the test, never visibility — this window is deliberately
    /// invisible for most of its life.
    fn ensure_open(&self) -> Result<()> {
        if self.app.get_webview_window(WINDOW_LABEL).is_none() {
            bail!("the Canvas window was closed — nothing was synced");
        }
        Ok(())
    }

    fn reveal(&self) {
        if self.shown.swap(true, Ordering::Relaxed) {
            return;
        }
        let _ = self.window.show();
        let _ = self.window.set_focus();
    }

    /// Writes a course file to `dest`, fetched by the page like everything else.
    ///
    /// Reading the cookies out with `cookies_for_url` and downloading from
    /// `reqwest` was tried first, on the theory that `/files/:id/download` is a
    /// plain authenticated resource rather than an API call. Canvas answered
    /// **403 to every file** — the session lives in cookies the webview does
    /// not hand out, so the replayed request is not the signed-in user's. This
    /// is the refusal SPEC §7.2 predicts for anything issued outside the page,
    /// and the fix is the same rule the API reads already follow: ask the page.
    ///
    /// The cost is that bytes come back base64 through `eval_with_callback`
    /// instead of streaming, so this holds a whole file in memory a few times
    /// over. `MAX_DOWNLOAD_BYTES` is what keeps that bounded. The page refuses
    /// anything past it before reading the body (see `REQUEST_JS`), which is
    /// where the memory would actually be committed; the check here is what
    /// makes the constant true for every caller regardless.
    ///
    /// `expected` is Canvas's own byte count for the file when it published
    /// one. A short read otherwise writes a truncated deck that looks like a
    /// successful download and gets proposed into the tree as real material.
    pub fn download(
        &self,
        url: &str,
        dest: &Path,
        expected: Option<i64>,
        on_stage: &dyn Fn(&str),
    ) -> Result<u64> {
        let host = tauri::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
            .unwrap_or_default();
        if host != CANVAS_HOST {
            bail!("the file link points at {host} rather than {CANVAS_HOST}");
        }

        let encoded = match self.request(url, Mode::Binary)? {
            Outcome::Binary(encoded) => encoded,
            Outcome::Json { .. } => bail!("Canvas answered with data rather than the file"),
            Outcome::Unauthorized(_) | Outcome::Offsite(_) => {
                self.await_session(on_stage)?;
                match self.request(url, Mode::Binary)? {
                    Outcome::Binary(encoded) => encoded,
                    _ => bail!("Canvas would not serve the file even after signing in"),
                }
            }
        };

        let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &encoded)
            .context("the page returned something that is not the file")?;
        drop(encoded);
        let written = bytes.len() as i64;
        if written > MAX_DOWNLOAD_BYTES {
            bail!(
                "the file is {} — past the {} that can be pulled through the Canvas page",
                crate::tools::format_size(written),
                crate::tools::format_size(MAX_DOWNLOAD_BYTES)
            );
        }
        if let Some(expected) = expected.filter(|e| *e != written) {
            bail!(
                "only {} of the {} Canvas lists for this file came back",
                crate::tools::format_size(written),
                crate::tools::format_size(expected)
            );
        }
        write_atomic(dest, &bytes)?;
        Ok(bytes.len() as u64)
    }
}

/// Writes through a temporary sibling, so a failure partway leaves the
/// destination absent rather than truncated.
///
/// `fs::write` truncates in place, which for a download means a half-written
/// slide deck sitting at the real name — indistinguishable from a complete one
/// to the scanner that indexes it next. The temporary is dot-prefixed so a
/// leftover from a killed process stays invisible to the scanner too.
fn write_atomic(dest: &Path, bytes: &[u8]) -> Result<()> {
    let name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let tmp = dest.with_file_name(format!(".{name}.part"));
    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", tmp.display()));
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("moving into place at {}", dest.display()));
    }
    Ok(())
}

fn close_existing(app: &AppHandle) -> Result<()> {
    let Some(existing) = app.get_webview_window(WINDOW_LABEL) else {
        return Ok(());
    };
    // `close` posts to the event loop and returns; the label is only freed once
    // the main thread has processed it. Building immediately fails with "a
    // webview with label canvas-session already exists", and once it does,
    // every later attempt fails the same way.
    let _ = existing.close();
    let give_up = Instant::now() + Duration::from_secs(3);
    while app.get_webview_window(WINDOW_LABEL).is_some() {
        if Instant::now() >= give_up {
            bail!("the previous Canvas window is still open — close it and try again");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Response handling

/// Canvas prefixes API JSON with `while(1);` so a response cannot be executed
/// as script by a page that tricked a browser into loading it. Harmless here,
/// and fatal to a parser that is not expecting it.
fn parse_body(body: &str) -> Result<Value> {
    let trimmed = body.trim_start();
    let json = trimmed.strip_prefix("while(1);").unwrap_or(trimmed);
    serde_json::from_str(json.trim_start()).with_context(|| {
        format!(
            "Canvas returned something that is not JSON: {}",
            crate::db::truncate(trimmed, 200)
        )
    })
}

/// The `rel="next"` URL from a `Link` header, as a same-origin path.
///
/// Returned as a path rather than the absolute URL Canvas sends, so the next
/// fetch stays same-origin by construction: a `Link` pointing at another host
/// ends the walk instead of sending the session's cookies somewhere new.
fn next_link(header: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let Some(raw) = segments.next() else { continue };
        let url = raw
            .trim()
            .strip_prefix('<')
            .and_then(|u| u.strip_suffix('>'))
            .unwrap_or_default();
        let is_next = segments.any(|s| {
            let s = s.trim().trim_end_matches(';');
            s.eq_ignore_ascii_case("rel=\"next\"") || s.eq_ignore_ascii_case("rel=next")
        });
        if !is_next || url.is_empty() {
            continue;
        }
        let Ok(parsed) = tauri::Url::parse(url) else {
            continue;
        };
        if parsed.host_str() != Some(CANVAS_HOST) {
            continue;
        }
        let mut path = parsed.path().to_string();
        if let Some(query) = parsed.query() {
            path.push('?');
            path.push_str(query);
        }
        return Some(path);
    }
    None
}

/// Canvas serves 10 items per page by default and caps `per_page` at 100.
/// Asking for the cap turns most collections into a single request.
fn with_per_page(path: &str) -> String {
    if path.contains("per_page=") {
        return path.to_string();
    }
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{path}{separator}per_page=100")
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Object(_) => "an object",
        Value::String(_) => "a string",
        Value::Null => "null",
        _ => "a value",
    }
}

/// A JS string literal for `value`, quotes included.
fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

// ---------------------------------------------------------------------------
// The in-page request
//
// `eval_with_callback` returns a *synchronous* result, and `fetch` is async, so
// the script cannot simply hand back a response. Instead each request gets an
// id: the first evaluation starts the fetch and parks its outcome on the
// window, and later evaluations of the same script collect it. This is the same
// start-then-poll shape `zoom.rs` uses for the caption fetch.
//
// The host check comes first and is load-bearing. During SSO the window is on
// login.ufl.edu, where this fetch would be cross-origin — and a CORS failure
// reads as a broken Canvas integration rather than as a sign-in that has not
// finished yet. Off-origin, the script reports where it is instead.

const REQUEST_JS: &str = r#"
(function () {
  var S = (window.__classhub_canvas = window.__classhub_canvas || {});
  var id = __ID__;
  var done = S[id];
  // Returned without clearing the slot. The answer still has to survive being
  // marshalled out of the page, and deleting it here would mean an eval that
  // times out mid-flight loses the result *and* the record that anything was
  // started — so the next poll would refetch a file that had already arrived.
  // Rust releases the slot once it holds the answer.
  if (done) return done;
  if (S["p:" + id]) return { state: "pending" };
  if (location.host !== __HOST__) return { state: "offsite", host: location.host };

  var binary = __BINARY__;
  var max = __MAXBYTES__;
  S["p:" + id] = 1;
  fetch(__PATH__, {
    credentials: "same-origin",
    headers: binary ? {} : { Accept: "application/json" }
  })
    .then(function (res) {
      var link = res.headers.get("Link") || "";
      // A refusal has a readable body and no file in it, so both modes take
      // the text path for anything that is not a success.
      if (!binary || !res.ok) {
        return res.text().then(function (body) {
          S[id] = { state: "response", status: res.status, body: body, link: link };
        });
      }
      // The ceiling is checked here because here is where the memory would be
      // committed: every copy this function makes is a multiple of the file.
      var declared = Number(res.headers.get("Content-Length") || -1);
      if (declared > max) {
        S[id] = { state: "toolarge", bytes: declared };
        return;
      }
      return res.arrayBuffer().then(function (buffer) {
        var bytes = new Uint8Array(buffer);
        if (bytes.length > max) {
          S[id] = { state: "toolarge", bytes: bytes.length };
          return;
        }
        var chunks = [];
        // btoa takes a string, and String.fromCharCode.apply overflows the
        // argument limit on anything megabyte-sized — hence the chunking.
        for (var i = 0; i < bytes.length; i += 0x8000) {
          chunks.push(String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000)));
        }
        S[id] = {
          state: "response",
          status: res.status,
          b64: btoa(chunks.join("")),
          link: link
        };
      });
    })
    .catch(function (e) {
      S[id] = { state: "error", message: String((e && e.message) || e) };
    });
  return { state: "pending" };
})()
"#;

/// Frees one request's slot. Separate from `REQUEST_JS` because the page must
/// not forget an answer until Rust has actually received it.
const RELEASE_JS: &str = r#"
(function () {
  var S = window.__classhub_canvas;
  if (!S) return { state: "gone" };
  var id = __ID__;
  delete S[id];
  delete S["p:" + id];
  return { state: "released" };
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The script lives in a raw string, so a `"` immediately followed by a `#`
    /// anywhere in the JS would close it early and truncate it into something
    /// that still compiles.
    #[test]
    fn the_request_script_survives_its_raw_string() {
        assert!(!REQUEST_JS.contains("\"#"), "the JS terminates its own raw string");
        assert!(
            REQUEST_JS.trim_end().ends_with("})()"),
            "the script is not a complete expression"
        );
        for placeholder in ["__ID__", "__HOST__", "__PATH__", "__BINARY__"] {
            assert!(REQUEST_JS.contains(placeholder), "{placeholder} is missing");
        }
    }

    /// A refusal must be read as text in both modes. Read as a file, a 403's
    /// HTML body would be base64-encoded and written into the inbox as though
    /// it were the slide deck.
    #[test]
    fn a_refused_download_is_read_as_text_not_as_a_file() {
        assert!(
            REQUEST_JS.contains("!binary || !res.ok"),
            "a non-OK binary response would be encoded as file bytes"
        );
    }

    /// The host check must precede the fetch. Reversed, an unfinished SSO
    /// produces a CORS error that reads as a broken Canvas integration.
    #[test]
    fn the_script_checks_its_origin_before_fetching() {
        let check = REQUEST_JS.find("location.host").expect("the host check");
        let fetch = REQUEST_JS.find("fetch(").expect("the fetch");
        assert!(check < fetch, "the fetch can fire off-origin");
    }

    #[test]
    fn follows_only_same_origin_next_links() {
        let header = "<https://ufl.instructure.com/api/v1/courses?page=2&per_page=100>; rel=\"next\",\
                      <https://ufl.instructure.com/api/v1/courses?page=9>; rel=\"last\"";
        assert_eq!(
            next_link(header).as_deref(),
            Some("/api/v1/courses?page=2&per_page=100")
        );

        // Only `next` — `current`, `first` and `last` are on every response, and
        // following `last` or `first` would loop forever.
        let no_next = "<https://ufl.instructure.com/api/v1/courses?page=1>; rel=\"current\",\
                       <https://ufl.instructure.com/api/v1/courses?page=1>; rel=\"first\"";
        assert_eq!(next_link(no_next), None);

        // A Link header pointing elsewhere ends the walk rather than sending
        // this session's cookies to another host.
        let offsite = "<https://evil.example.com/api/v1/courses?page=2>; rel=\"next\"";
        assert_eq!(next_link(offsite), None);

        assert_eq!(next_link(""), None);
    }

    #[test]
    fn asks_for_full_pages_without_duplicating_the_parameter() {
        assert_eq!(with_per_page("/api/v1/courses"), "/api/v1/courses?per_page=100");
        assert_eq!(
            with_per_page("/api/v1/courses?enrollment_state=active"),
            "/api/v1/courses?enrollment_state=active&per_page=100"
        );
        // A `next` URL already carries Canvas's own paging parameters.
        let paged = "/api/v1/courses?page=2&per_page=100";
        assert_eq!(with_per_page(paged), paged);
    }

    #[test]
    fn strips_the_json_hijacking_prefix() {
        let value = parse_body("while(1);[{\"id\":1}]").expect("parses");
        assert_eq!(value[0]["id"], 1);
        // And reads a plain body unchanged.
        assert_eq!(parse_body("{\"id\":7}").expect("parses")["id"], 7);
        // A login page served with a 200 must not pass as data.
        assert!(parse_body("<!DOCTYPE html><html>").is_err());
    }
}
