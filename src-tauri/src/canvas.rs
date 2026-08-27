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
//! - **Nothing is minted, and the cookie is kept.** No token is created and
//!   `POST /users/:id/tokens` stays off-limits (SPEC §1); what the Keychain
//!   holds is a copy of the cookie Canvas already set for its own host, so the
//!   app's reach stays the reach the sign-in granted. Keeping it is what makes
//!   a relaunch cheap: Canvas issues that cookie with no expiry, WKWebView
//!   keeps expiry-less cookies in memory only, and quitting the app was
//!   therefore ending the session every time — see `remember`. Canvas decides
//!   when it dies, and a refusal is what throws the copy away.
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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::webview::cookie::Cookie;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const CANVAS_HOST: &str = "ufl.instructure.com";
const CANVAS_ORIGIN: &str = "https://ufl.instructure.com/";
const WINDOW_LABEL: &str = "canvas-session";

/// Long enough for Shibboleth plus a Duo push that goes to a phone in another
/// room — but bounded, so a forgotten window is not a hang.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SIGN_IN_POLL: Duration = Duration::from_millis(1_000);
/// How long a window that has not navigated anywhere yet is given before it is
/// treated as a Canvas that wants a sign-in.
///
/// A window spends its first moments on the initial empty document, which
/// reports no host at all. Read as "somewhere other than Canvas", that
/// transient reveals a window and throws away a restored cookie a moment before
/// that cookie would have worked. This covers only that reading: a 401 from
/// Canvas, or a page genuinely at login.ufl.edu, is acted on at once. Waiting
/// on *those* would be actively harmful — a hidden webview has its JavaScript
/// throttled by macOS, and Duo's prompt boots into a painted shell with no body
/// if it starts up off screen.
const SIGN_IN_SETTLE: Duration = Duration::from_secs(5);
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
/// The window *is* the client, but it is not the session: cookies live in
/// WKWebView's process-wide store, so closing the window ends a sync and leaves
/// the session where `remember` can find it.
///
/// Closing it is still an obligation rather than a courtesy, so it belongs to
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

/// Canvas serving the page but refusing one collection, with the session
/// demonstrably live. A course whose Files tab the professor disabled answers
/// 401 for `/files` while `/users/self` answers 200 — a course setting rather
/// than a lapsed sign-in, and the two want different things from the reader.
#[derive(Debug)]
pub struct Refused {
    pub path: String,
    pub status: u64,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the session is live but Canvas will not share {} for this course (status {})",
            self.path, self.status
        )
    }
}

impl std::error::Error for Refused {}

/// What a page that is not on Canvas's origin actually means.
#[derive(Debug, PartialEq)]
enum Offsite {
    /// The window has not navigated anywhere yet. Not an answer — wait.
    Loading,
    /// Still nowhere, for longer than a first navigation should take. Worth
    /// showing, so a stall is not hidden; not evidence of anything refused.
    Stalled,
    /// A real host: Canvas sent us to SSO. Conclusive.
    Bounced,
}

/// How to read an off-origin sighting.
///
/// Pure and tested because the condition it replaced was invertible in place:
/// it still compiled, still read plausibly, and failed silently in opposite
/// directions — either the sign-in window never appears, or a live session is
/// deleted a moment before it would have worked. The initial empty document
/// reports no host at all, which is what makes waiting the only way to tell a
/// slow first navigation from a page that really did land somewhere else.
fn read_offsite(host: &str, waited: Duration) -> Offsite {
    if !host.is_empty() {
        Offsite::Bounced
    } else if waited >= SIGN_IN_SETTLE {
        Offsite::Stalled
    } else {
        Offsite::Loading
    }
}

enum Outcome {
    Json { value: Value, next: NextPage },
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
    /// screen. The window is shown only when Canvas actually wants a sign-in —
    /// which, with the last session's cookies put back first, means Canvas has
    /// ended it rather than merely that the app was restarted.
    pub fn open(app: &AppHandle, on_stage: &dyn Fn(&str)) -> Result<Session> {
        close_existing(app)?;
        restore(app, on_stage);
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
                NextPage::Follow(next) => url = next,
                NextPage::End => return Ok(items),
                // Bailing rather than returning what was collected. A short
                // list handed back as a complete one is the failure mode here;
                // an error the reader can see is not.
                NextPage::Refused(why) => bail!(
                    "{path} has more pages, but Canvas offered {why} — stopping rather than \
                     reporting a partial list as the whole collection"
                ),
            }
        }
        bail!("{path} kept paginating past {MAX_PAGES} pages — stopping rather than looping")
    }

    fn get_page(&self, path: &str, on_stage: &dyn Fn(&str)) -> Result<(Value, NextPage)> {
        match self.request(path, Mode::Json)? {
            Outcome::Json { value, next } => Ok((value, next)),
            // The session lapsed partway through a sync. Expected rather than
            // exceptional — Canvas ends a session on its own schedule, so this
            // is what time passing looks like. Sign in again and repeat.
            Outcome::Unauthorized(_) | Outcome::Offsite(_) => {
                self.await_session(on_stage)?;
                match self.request(path, Mode::Json)? {
                    Outcome::Json { value, next } => Ok((value, next)),
                    // `await_session` just proved the session is live, so a
                    // second refusal is this course's setting rather than a
                    // sign-in problem — and blaming the sign-in sends the
                    // reader after the wrong thing.
                    Outcome::Unauthorized(status) => Err(Refused {
                        path: path.to_string(),
                        status,
                    }
                    .into()),
                    // Worth naming: landing on login.ufl.edu means SSO bounced
                    // rather than that Canvas is broken, and those need
                    // different things from the reader.
                    // An empty host is the initial empty document rather than a
                    // place, and naming it renders a blank where a hostname
                    // should be.
                    Outcome::Offsite(host) if host.is_empty() => {
                        bail!("the Canvas window never finished loading")
                    }
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
        let started = Instant::now();
        let deadline = started + SIGN_IN_TIMEOUT;
        let mut asked = false;
        loop {
            match self.request("/api/v1/users/self", Mode::SignIn)? {
                Outcome::Json { .. } => {
                    if asked {
                        on_stage("Signed in — reading your courses…");
                    }
                    // Here rather than in `open`, so a session that lapses
                    // mid-sync is re-remembered too — and so the stored copy's
                    // age is refreshed by every sync that uses it.
                    self.remember();
                    return Ok(());
                }
                // `Mode::SignIn` never sets binary mode, so this cannot happen
                // — and reading it as a successful sign-in would have been the
                // wrong way to be wrong about it.
                Outcome::Binary(_) => bail!("the sign-in probe answered with a file"),
                // Canvas served the page and refused the call. A 401 is a
                // lapsed session; a 403 is Canvas declining one call with the
                // session perfectly alive — its rate limiter answers 403, and
                // `Refused` further down exists because a course can too. Only
                // the first is allowed to throw the stored copy away.
                Outcome::Unauthorized(status) => self.ask(&mut asked, status == 401, on_stage),
                Outcome::Offsite(host) => match read_offsite(&host, started.elapsed()) {
                    Offsite::Loading => {}
                    Offsite::Stalled => self.ask(&mut asked, false, on_stage),
                    Offsite::Bounced => self.ask(&mut asked, true, on_stage),
                },
            }
            if Instant::now() >= deadline {
                bail!(
                    "timed out waiting for the Canvas sign-in. Nothing was read — starting \
                     the sync again reopens the window."
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

    /// Puts the sign-in in front of the user, once.
    ///
    /// `refused` is whether Canvas actually turned the stored copy down. The
    /// two halves of this are not equally reversible: showing a window costs a
    /// window, while deleting the session costs a Duo round on the next launch
    /// — so only a conclusive refusal does the second. A window that has not
    /// navigated anywhere yet has refused nothing, and the copy it may be about
    /// to prove has no business being thrown away first.
    fn ask(&self, asked: &mut bool, refused: bool, on_stage: &dyn Fn(&str)) {
        if *asked {
            return;
        }
        *asked = true;
        if refused {
            forget();
        }
        self.reveal();
        on_stage("Waiting for you to sign in to Canvas…");
    }

    /// Stores the cookies Canvas has set for its own host, so the next launch
    /// does not begin with a Duo push.
    ///
    /// This is the whole of what makes a session outlive the app. Canvas sends
    /// `canvas_session` and `log_session_id` with neither `Expires` nor
    /// `Max-Age`, which makes them session cookies; WKWebView keeps those in
    /// memory and never writes them to its own jar, so quitting the app was
    /// what kept ending the session — not a Tauri setting and not an unflushed
    /// write. Duo's month-long device-trust cookie persisted through the same
    /// runs, which is how that was told apart.
    ///
    /// Everything Canvas set for the host is kept except the CSRF token, which
    /// only matters for writes. Listing the two by name instead would go stale
    /// the day Canvas renames one, and would go stale silently.
    ///
    /// Best-effort: a Keychain that will not answer costs a sign-in next
    /// launch, not this sync.
    fn remember(&self) {
        // The one webview touch in this file that `poll` has not already
        // guarded. Cheap, and it turns the ordinary "closed the window straight
        // after signing in" case into a return rather than a caught panic.
        if self.ensure_open().is_err() {
            return;
        }
        let Some(cookies) = read_jar(&self.window) else {
            eprintln!(
                "canvas: could not read the session out of the window — the next launch \
                 will ask for a sign-in"
            );
            return;
        };
        let cookies = worth_remembering(&cookies);
        // Remembering nothing is not the same as having nothing to remember:
        // an empty list would replace a working item with a useless one.
        if cookies.is_empty() {
            return;
        }
        let session = Remembered {
            captured_at: crate::db::now(),
            cookies,
        };
        let Some(entry) = keychain() else {
            return;
        };
        let Ok(json) = serde_json::to_string(&session) else {
            return;
        };
        // Still best-effort — a Keychain that will not answer must not fail a
        // sync that otherwise worked. But saying so matters: without it, a
        // permanently broken save is indistinguishable from Canvas expiring the
        // session, and "why does Duo ask me every launch again" is exactly the
        // question this mechanism exists to answer.
        if let Err(e) = entry.set_password(&json) {
            eprintln!(
                "canvas: could not save the session ({e}) — the next launch will ask \
                 for a sign-in"
            );
        }
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

// ---------------------------------------------------------------------------
// Remembering the session across launches

/// The Keychain account holding the Canvas cookies, under the same service as
/// the API key — a credential belongs there and nowhere else (SPEC §13).
const KEYCHAIN_USER: &str = "canvas-session-cookies";

/// Canvas's CSRF token, excluded deliberately: it is read only for mutating
/// verbs, and ClassHub never writes to Canvas.
const CSRF_COOKIE: &str = "_csrf_token";

/// How long a remembered session may sit in the Keychain unused.
///
/// Canvas is what actually ends a session, and a refusal is what normally
/// clears the item; this is the backstop for the case where no later sync ever
/// asks. Thirty days is the window Duo's own device-trust cookie keeps, so a
/// session that outlived this would have needed the full sign-in regardless.
const REMEMBERED_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// How many times reading the cookie jar is worth attempting, and how long to
/// wait between. wry's own budget for the read is one second (see `read_jar`),
/// which is generous until the main thread is busy — and giving up after one
/// try means silently storing nothing.
const JAR_READ_TRIES: u32 = 3;
const JAR_READ_RETRY: Duration = Duration::from_millis(150);

#[derive(Serialize, Deserialize)]
struct Remembered {
    /// When these were read out of the webview — what `REMEMBERED_FOR` is
    /// measured against. Refreshed by every sync that finds the session live.
    captured_at: i64,
    cookies: Vec<RememberedCookie>,
}

/// One cookie, minus its domain: everything stored here is `CANVAS_HOST` by
/// construction (`worth_remembering` enforces it), and writing it down would
/// only create a way for the two to disagree.
///
/// `SameSite` and the expiry are dropped on purpose, and both drops are exact
/// rather than lossy. Canvas sends `canvas_session` as `SameSite=None`, which
/// wry serializes to no attribute at all — the same bytes as storing nothing —
/// and it maps a missing policy back to `None`, so the round trip is faithful
/// in both directions. The missing expiry is the whole design: a cookie with no
/// expiry is a session cookie, which is exactly what WKWebView refuses to write
/// to its own on-disk jar, leaving the Keychain as the only copy at rest.
/// Widening either of these would trade that property away for nothing.
#[derive(PartialEq, Serialize, Deserialize)]
struct RememberedCookie {
    name: String,
    value: String,
    path: String,
    secure: bool,
    http_only: bool,
}

impl std::fmt::Debug for RememberedCookie {
    /// Renders the value's length rather than the value. This is a bearer
    /// credential, and a `{:?}` added later — in a log line, an error chain, a
    /// test failure — should not be able to print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberedCookie")
            .field("name", &self.name)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("http_only", &self.http_only)
            .finish()
    }
}

/// `Option` rather than `Result`: every caller here is best-effort and none has
/// anywhere to report a reason, so a `Result` only built a message that was
/// discarded three times over. `chat.rs` keeps its error because its caller
/// shows it to the user.
fn keychain() -> Option<keyring::v1::Entry> {
    keyring::v1::Entry::new(crate::KEYCHAIN_SERVICE, KEYCHAIN_USER).ok()
}

/// Which of Canvas's cookies are worth keeping.
///
/// The host filter is not redundant with the caller's `cookies_for_url`, which
/// is what makes it true today: `RememberedCookie` documents "everything here
/// is `CANVAS_HOST` by construction", and until now nothing in this file made
/// that so. `restore` re-stamps `CANVAS_HOST` onto whatever it reads back, so
/// one slip upstream — reading the whole jar instead of Canvas's slice — would
/// turn a wide capture into cross-host injection. Enforcing it here costs a
/// line and removes that whole shape.
fn worth_remembering(cookies: &[Cookie<'_>]) -> Vec<RememberedCookie> {
    cookies
        .iter()
        .filter(|cookie| cookie.domain() == Some(CANVAS_HOST))
        .filter(|cookie| cookie.name() != CSRF_COOKIE)
        .map(|cookie| RememberedCookie {
            name: cookie.name().to_string(),
            value: cookie.value().to_string(),
            // The hazard is an empty path rather than an absent one — wry always
            // sets it, and `NSHTTPCookie` refuses to build from "".
            path: cookie.path().filter(|path| !path.is_empty()).unwrap_or("/").to_string(),
            // Canvas sets both, and defaulting the other way would hand a
            // session cookie to a plaintext request or to `document.cookie`.
            secure: cookie.secure().unwrap_or(true),
            http_only: cookie.http_only().unwrap_or(true),
        })
        .collect()
}

/// Reads Canvas's cookies out of `window`, or nothing if the jar will not answer.
///
/// Both hazards here live in Tauri's dispatcher rather than in this file.
///
/// It answers with `rx.recv().unwrap()`, and a webview destroyed before the
/// main thread drains the message drops the reply channel — so a window closed
/// at the wrong instant panics the calling thread. For a best-effort cookie
/// save that would unwind an otherwise finished sync and report it to the user
/// as "the sync stopped unexpectedly", which is both wrong and worse than the
/// message a closed window already has.
///
/// And wry gives the underlying `getAllCookies` one second, measured at exactly
/// the moment WebKit's network process is busiest — right after a whole SSO
/// chain finished loading. A single failed read is worth retrying, because
/// treating it as an answer means silently storing nothing, whose symptom is a
/// full sign-in on the next launch: indistinguishable from the bug all of this
/// exists to fix.
///
/// The one wait in this file that is not bounded from here. Every other is —
/// `EVAL_TIMEOUT`, `REQUEST_TIMEOUT`, `DOWNLOAD_TIMEOUT`, `SIGN_IN_TIMEOUT`,
/// even `close_existing`'s three seconds — because a stalled call otherwise
/// parks the sync thread. Tauri owns this channel and offers no deadline, and
/// buying one back means routing the read through `run_on_main_thread` and
/// re-implementing the reply. wry's own one-second cap bounds it in practice,
/// so this is recorded rather than worked around.
fn read_jar(window: &WebviewWindow) -> Option<Vec<Cookie<'static>>> {
    let url = tauri::Url::parse(CANVAS_ORIGIN).ok()?;
    for attempt in 0..JAR_READ_TRIES {
        if attempt > 0 {
            std::thread::sleep(JAR_READ_RETRY);
        }
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            window.cookies_for_url(url.clone())
        }));
        match read {
            Ok(Ok(cookies)) => return Some(cookies),
            // The window is gone. Retrying cannot help, and the next attempt
            // would panic the same way.
            Err(_) => return None,
            Ok(Err(_)) => {}
        }
    }
    None
}

/// The remembered session, or nothing when there is none worth replaying.
///
/// The two ways of failing want opposite responses. A *read* failure is
/// transient — a locked Keychain, a denied prompt — and must never delete
/// anything, since the cost of being wrong is one sign-in. A *parse* failure is
/// not transient: this build can never use that item again, and leaving it
/// would strand a live cookie at rest with nothing able to remove it, because
/// the age check below sits downstream of the parse.
fn stored_session() -> Option<Remembered> {
    let stored = keychain()?.get_password().ok()?;
    let Ok(session) = serde_json::from_str::<Remembered>(&stored) else {
        forget();
        return None;
    };
    let age = crate::db::now().saturating_sub(session.captured_at);
    // Both ends, because `saturating_sub` on `i64` clamps at `i64::MIN` rather
    // than at zero: a `captured_at` ahead of the clock reads as a negative age,
    // which would sit below the cap forever and quietly disable it. A timestamp
    // from the future is its own reason to distrust the copy.
    if !(0..REMEMBERED_FOR.as_secs() as i64).contains(&age) {
        forget();
        return None;
    }
    Some(session)
}

/// Drops the stored cookies.
///
/// Only the copy at rest. Whatever is in the live jar stays there until Canvas
/// overwrites it at the next sign-in, which is the moment it becomes wrong
/// anyway — deleting it here would be another fire-and-forget hop to the main
/// thread buying nothing.
fn forget() {
    if let Some(entry) = keychain() {
        let _ = entry.delete_credential();
    }
}

/// Puts the remembered cookies back, before the Canvas window is built.
///
/// Two invariants hold this up, and only one of them is about the data store.
///
/// WKWebView gives every non-incognito webview the same process-wide
/// `defaultDataStore`, which is why *which* window the cookie goes through does
/// not matter. What matters more is *ordering*: off the main thread
/// `set_cookie` queues a message and returns `Ok(())` before the cookie exists
/// anywhere, so nothing is written by the time this returns. Window creation
/// rides the same event-loop queue, and that queue is drained in order and
/// cannot be drained re-entrantly, so the writes are handled first — and the
/// blocking read at the end is what turns "handled first" from an argument
/// about someone else's internals into something this function observes.
/// Moving this after `build()`, or onto the main thread, breaks it in a way
/// that looks exactly like Canvas expiring the session.
///
/// Best-effort throughout: with nothing restored, the sync asks for a sign-in,
/// which is what it did before any of this existed.
fn restore(app: &AppHandle, on_stage: &dyn Fn(&str)) {
    // `close_existing` has already run, so this is never the stale Canvas
    // window. By name rather than whichever the map happens to yield first:
    // any window is correct, but a deterministic one keeps a failure
    // reproducible instead of varying per run.
    let windows = app.webview_windows();
    let Some(window) = windows.get("main").or_else(|| windows.values().next()) else {
        return;
    };
    // Named before the Keychain read, which is the one call here that can put a
    // system prompt in front of the user. It happens with no Canvas window on
    // screen yet, so an unnamed stall would read as a hang.
    on_stage("Checking for a saved Canvas session…");
    let Some(session) = stored_session() else {
        return;
    };
    // Whatever the jar already holds came from this run, so it is at least as
    // fresh as a copy captured when some earlier sync opened. Restoring repairs
    // a cold jar; writing over a live cookie could only move the session
    // backwards. Compared by name rather than by "is the jar empty", since
    // Canvas keeps a non-session cookie for its host too and a blanket check
    // would skip the restore entirely.
    let live: Vec<String> = read_jar(window)
        .unwrap_or_default()
        .iter()
        .map(|cookie| cookie.name().to_string())
        .collect();
    let mut restored = 0;
    for cookie in session.cookies {
        // An empty name or path reaches an `expect` inside wry — `NSHTTPCookie`
        // refuses to build one — and that runs on the main thread, where the
        // sync's own `catch_unwind` cannot reach it. It would also fire before
        // anything could delete the offending item, so every later sync would
        // take the app down the same way. Skipping is the whole fix.
        if cookie.name.is_empty() || live.iter().any(|name| *name == cookie.name) {
            continue;
        }
        let path = if cookie.path.is_empty() {
            "/".to_string()
        } else {
            cookie.path
        };
        let _ = window.set_cookie(
            Cookie::build((cookie.name, cookie.value))
                .domain(CANVAS_HOST)
                .path(path)
                .secure(cookie.secure)
                .http_only(cookie.http_only)
                .build(),
        );
        restored += 1;
    }
    if restored == 0 {
        return;
    }
    // The barrier described above. This read rides the same queue as the writes
    // and blocks for its answer, so it cannot come back before every one of them
    // has been handled — and the answer says whether they actually landed,
    // which `set_cookie` alone can never report.
    let landed =
        read_jar(window).is_some_and(|jar| jar.iter().any(|cookie| cookie.name() != CSRF_COOKIE));
    if !landed {
        eprintln!("canvas: the saved session did not reach the webview — expect a sign-in");
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

/// What a response's `Link` header says about the rest of the collection.
#[derive(Debug, PartialEq)]
enum NextPage {
    /// No `rel="next"`: this was the last page.
    End,
    /// A same-origin path to fetch next.
    Follow(String),
    /// There is a next page and it cannot be followed. Distinct from `End` on
    /// purpose — treating the two alike is what turns a truncated collection
    /// into one that looks complete, which SPEC §7.2 names as the failure this
    /// whole walk exists to avoid.
    Refused(String),
}

/// The `rel="next"` URL from a `Link` header, as a same-origin path.
///
/// Returned as a path rather than the absolute URL Canvas sends, so the next
/// fetch stays same-origin by construction: nothing downstream can be handed a
/// URL that would send this session's cookies to another host.
fn next_link(header: &str) -> NextPage {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let Some(raw) = segments.next() else { continue };
        let trimmed = raw.trim();
        let is_next = segments.any(|s| {
            let s = s.trim().trim_end_matches(';');
            s.eq_ignore_ascii_case("rel=\"next\"") || s.eq_ignore_ascii_case("rel=next")
        });
        if !is_next {
            continue;
        }
        let url = trimmed
            .strip_prefix('<')
            .and_then(|u| u.strip_suffix('>'))
            .unwrap_or_default();
        if url.is_empty() {
            return NextPage::Refused(format!("a next link that is not a URL ({trimmed})"));
        }
        let Ok(parsed) = tauri::Url::parse(url) else {
            return NextPage::Refused(format!("a next URL that will not parse ({url})"));
        };
        if parsed.host_str() != Some(CANVAS_HOST) {
            return NextPage::Refused(format!(
                "a next page at {} rather than {CANVAS_HOST}",
                parsed.host_str().unwrap_or("nowhere")
            ));
        }
        let mut path = parsed.path().to_string();
        if let Some(query) = parsed.query() {
            path.push('?');
            path.push_str(query);
        }
        return NextPage::Follow(path);
    }
    NextPage::End
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

    /// Drives the script itself against a mock page.
    ///
    /// Nothing in Rust can execute it, and asserting that a phrase appears in
    /// the source passes just as happily when the branch is inverted — writing
    /// `!binary || res.ok` keeps the substring and encodes a 403's HTML body
    /// into the inbox as though it were the slide deck. The harness covers the
    /// behaviours that actually matter: the host check before the fetch, a
    /// refusal read as text in both modes, the chunking across its 0x8000
    /// boundary, the ceiling refused before the body is read, and the result
    /// surviving being read.
    #[test]
    fn the_request_script_behaves_against_a_mock_page() {
        let out = std::process::Command::new("node")
            .args(["tests/canvas_request.mjs", "src/canvas.rs"])
            .output();
        let Ok(out) = out else {
            eprintln!("skipping the request harness: node is not on PATH");
            return;
        };
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn follows_only_same_origin_next_links() {
        let header = "<https://ufl.instructure.com/api/v1/courses?page=2&per_page=100>; rel=\"next\",\
                      <https://ufl.instructure.com/api/v1/courses?page=9>; rel=\"last\"";
        assert_eq!(
            next_link(header),
            NextPage::Follow("/api/v1/courses?page=2&per_page=100".into())
        );

        // Only `next` — `current`, `first` and `last` are on every response, and
        // following `last` or `first` would loop forever.
        let no_next = "<https://ufl.instructure.com/api/v1/courses?page=1>; rel=\"current\",\
                       <https://ufl.instructure.com/api/v1/courses?page=1>; rel=\"first\"";
        assert_eq!(next_link(no_next), NextPage::End);
        assert_eq!(next_link(""), NextPage::End);
    }

    /// A next page that cannot be followed is not the end of the collection.
    /// Read as one, the walk would hand back a short list as a complete one —
    /// which is the whole failure mode following `next` exists to prevent.
    #[test]
    fn an_unusable_next_link_is_not_the_end_of_the_collection() {
        // Pointing elsewhere: the walk stops rather than sending this session's
        // cookies to another host, and says so rather than claiming completion.
        let offsite = "<https://evil.example.com/api/v1/courses?page=2>; rel=\"next\"";
        assert!(matches!(next_link(offsite), NextPage::Refused(_)));
        assert!(matches!(
            next_link("<not a url>; rel=\"next\""),
            NextPage::Refused(_)
        ));
        assert!(matches!(next_link("; rel=\"next\""), NextPage::Refused(_)));
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

    /// The reading that decides whether a stored session survives. A real host
    /// is Canvas bouncing us to SSO; an empty one is a window that has not
    /// navigated yet, which must never be read as a refusal however long it
    /// takes — the session is deleted on a refusal, and a slow Canvas is not one.
    #[test]
    fn only_a_real_host_is_canvas_turning_the_session_away() {
        assert_eq!(read_offsite("login.ufl.edu", Duration::ZERO), Offsite::Bounced);
        // Waiting does not turn "still loading" into "refused" — it only earns
        // a visible window, so a stall is not hidden behind one.
        assert_eq!(read_offsite("", Duration::ZERO), Offsite::Loading);
        assert_eq!(read_offsite("", SIGN_IN_SETTLE - Duration::from_millis(1)), Offsite::Loading);
        assert_eq!(read_offsite("", SIGN_IN_SETTLE), Offsite::Stalled);
        assert_eq!(read_offsite("", SIGN_IN_SETTLE * 100), Offsite::Stalled);
    }

    /// The CSRF token is the one cookie Canvas sets for its host that is not
    /// wanted: it exists for mutating verbs, which never happen. Everything
    /// else is kept by rule rather than by name, so a Canvas rename cannot
    /// quietly drop the cookie the whole thing rests on.
    #[test]
    fn keeps_canvas_cookies_but_never_the_csrf_token() {
        let cookies = vec![
            Cookie::build(("canvas_session", "opaque"))
                .domain(CANVAS_HOST)
                .path("/")
                .secure(true)
                .http_only(true)
                .build(),
            Cookie::build((CSRF_COOKIE, "not wanted"))
                .domain(CANVAS_HOST)
                .path("/")
                .build(),
            Cookie::build(("log_session_id", "6f0c"))
                .domain(CANVAS_HOST)
                .path("/")
                .build(),
            // `restore` re-stamps CANVAS_HOST onto everything it reads back, so
            // anything captured from another host would be re-labelled as
            // Canvas's. Nothing upstream should hand these over — but the
            // filter is what makes that a property of this file rather than of
            // someone else's.
            Cookie::build(("browsertrust", "duo")).domain("api.duosecurity.com").build(),
            Cookie::build(("_shibsession", "sso")).domain("login.ufl.edu").build(),
            Cookie::build(("wide", "parent")).domain("instructure.com").build(),
        ];
        let kept = worth_remembering(&cookies);
        assert_eq!(
            kept.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["canvas_session", "log_session_id"]
        );
        // A cookie that declares neither flag is stored with both, never with
        // a session cookie downgraded to plaintext or to `document.cookie`.
        assert!(kept[1].secure && kept[1].http_only);
        // An empty path is what `NSHTTPCookie` refuses to build from, and a
        // refusal there panics the main thread rather than this one.
        let blank = vec![Cookie::build(("canvas_session", "v"))
            .domain(CANVAS_HOST)
            .path("")
            .build()];
        assert_eq!(worth_remembering(&blank)[0].path, "/");
    }

    /// What a *future* build does with *this* build's item — the contract that
    /// actually matters, since the Keychain outlives the binary that wrote it.
    /// Every drift must land on the sign-in path rather than on a half-filled
    /// struct, which is what having no `#[serde(default)]` anywhere buys.
    #[test]
    fn a_changed_schema_falls_back_to_signing_in() {
        // A renamed or dropped field fails closed rather than defaulting.
        assert!(serde_json::from_str::<Remembered>(r#"{"cookies":[]}"#).is_err());
        assert!(serde_json::from_str::<Remembered>(r#"{"captured_at":1}"#).is_err());
        // An added field does not, so widening the struct later costs nothing.
        assert!(
            serde_json::from_str::<Remembered>(r#"{"captured_at":1,"cookies":[],"later":true}"#)
                .is_ok()
        );
    }

    /// The Keychain stores a string, so a cookie value carrying `/`, `+` or `=`
    /// has to survive serde before it can survive the Keychain. The I/O around
    /// this is deliberately untested — it needs a real Keychain — so the name
    /// says JSON rather than promising more than it proves.
    #[test]
    fn a_remembered_session_survives_the_json_the_keychain_stores() {
        let session = Remembered {
            captured_at: 1_756_000_000,
            cookies: worth_remembering(&[Cookie::build(("canvas_session", "opaque.value-with_/+="))
                .domain(CANVAS_HOST)
                .path("/")
                .secure(true)
                .http_only(true)
                .build()]),
        };
        let json = serde_json::to_string(&session).expect("serializes");
        let back: Remembered = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back.captured_at, session.captured_at);
        assert_eq!(back.cookies, session.cookies);
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
