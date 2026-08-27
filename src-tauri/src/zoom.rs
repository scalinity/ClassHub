//! SPEC §7.1 — capturing a lecture transcript from a Zoom recording link.
//!
//! There is no supported way to pull a caption track from a share link. A
//! university recording sits behind institutional SSO and often a passcode, and
//! whether a *viewer* may see the transcript at all is the host's setting; the
//! Zoom API path needs host or admin OAuth, which a student account does not
//! have. So a plain HTTP GET lands on a login page, and the only thing that can
//! reach the recording is a browser session the user signed in themselves.
//!
//! That is what this does: open the link in a webview, let the user complete
//! whatever Zoom asks for, then read the transcript out of the page they landed
//! on. Extraction runs through `eval_with_callback`, which evaluates an
//! expression in the page and hands the result back — deliberately, because it
//! means zoom.us is never granted IPC access to the app and never becomes a
//! caller into it. The page is read; it does not get to speak.
//!
//! When the host disabled viewer transcripts there is nothing to read, so the
//! recording itself is downloaded — with the same session's cookies, since the
//! media URL is authenticated too — and handed to Parakeet. That fallback is
//! why on-device transcription earns its place instead of duplicating Zoom.
//!
//! This reads an undocumented page structure, so it is written to fail
//! *legibly*: every probe reports what it did and did not find, and a miss
//! tells the user which manual step to take rather than dead-ending.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// Long enough to sign in through SSO, fetch a passcode out of an email, and
/// let the player settle — but bounded, so a forgotten window is not a hang.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const POLL_INTERVAL: Duration = Duration::from_millis(1_500);
/// One probe round-trip. Generous: the page is busy while the player loads.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
const WINDOW_LABEL: &str = "zoom-capture";
/// A lecture recording runs to gigabytes over whatever connection is to hand,
/// so this is sized for a slow one rather than a fast one.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// Reads the transcript for a Zoom recording link, returning the caption text
/// and a display name for where it came from.
pub fn fetch_caption(
    app: &AppHandle,
    url: &str,
    on_stage: &dyn Fn(&str),
) -> Result<(String, String)> {
    let parsed = tauri::Url::parse(url).ok();
    let host = parsed
        .as_ref()
        .and_then(|u| u.host_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !is_zoom_host(&host) {
        bail!("{url} is not a Zoom recording link");
    }

    on_stage("Opening Zoom — sign in if prompted…");
    let window = open_window(app, url)?;

    let deadline = Instant::now() + CAPTURE_TIMEOUT;
    let mut last_state = String::new();
    let mut last_found = String::new();
    let mut last_error: Option<String> = None;
    let outcome = loop {
        if Instant::now() >= deadline {
            let _ = window.close();
            // The probe's own account of what it found is the whole diagnostic
            // here: this reads an undocumented page, so "timed out" alone would
            // leave nothing to act on. Its errors are part of that account —
            // when the eval itself is what keeps failing, state and found are
            // empty precisely when they are most needed.
            bail!(
                "timed out waiting for the Zoom recording ({host}). Last state: {} · \
                 page had: {}{}",
                if last_state.is_empty() { "nothing yet" } else { &last_state },
                if last_found.is_empty() { "nothing recognizable" } else { &last_found },
                last_error.map(|e| format!(" · last probe error: {e}")).unwrap_or_default()
            );
        }

        // A closed window is the user cancelling, not a failure to report —
        // and closing it is the test. Visibility is not: macOS reports a
        // minimized window and a hidden app as invisible, so Cmd-Tabbing away
        // to fetch a passcode read as a cancel that never happened, while a
        // probe throwing mid-SSO-navigation is entirely ordinary.
        if app.get_webview_window(WINDOW_LABEL).is_none() {
            bail!("the Zoom window was closed before the transcript was captured");
        }
        let probe = match probe(&window, PROBE) {
            Ok(v) => v,
            Err(e) => {
                last_error = Some(format!("{e:#}"));
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }
        };

        let state = probe["state"].as_str().unwrap_or("waiting").to_string();
        if state != last_state {
            on_stage(stage_label(&state));
            last_state = state.clone();
        }
        if let Some(found) = probe["found"].as_array() {
            if !found.is_empty() {
                last_found = found
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
            }
        }
        match state.as_str() {
            "ready" => {
                let text = probe["text"].as_str().unwrap_or_default().to_string();
                if !text.trim().is_empty() {
                    break Outcome::Caption(text);
                }
            }
            // Nothing to read: the host turned viewer transcripts off. The
            // recording itself is still reachable with this session.
            "no-transcript" => {
                if let Some(media) = probe["mp4"].as_str() {
                    break Outcome::Media(media.to_string());
                }
                let _ = window.close();
                bail!(
                    "this recording exposes no transcript and no downloadable media — \
                     the host has both switched off. Download the transcript from the \
                     Zoom page yourself and add it as a file instead."
                );
            }
            _ => {}
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let name = format!("Zoom recording ({})", host);
    match outcome {
        Outcome::Caption(text) => {
            let _ = window.close();
            on_stage("Transcript captured");
            Ok((text, name))
        }
        Outcome::Media(media_url) => {
            on_stage("No transcript published — downloading the recording…");
            // Both origins: Zoom serves recordings off a media host of its own,
            // so the page's cookies alone leave the request unauthenticated.
            let cookies = cookie_header(&window, &[url, &media_url]);
            let referer = origin_of(url);
            let _ = window.close();
            let media = download(app, &media_url, &cookies, &referer, on_stage)?;
            let vtt = crate::transcribe::to_vtt(app, &media, on_stage);
            // Kept when transcription fails: re-fetching gigabytes because
            // Parakeet was misconfigured is a long way to go for a retry.
            if vtt.is_ok() {
                let _ = std::fs::remove_file(&media);
            }
            Ok((vtt?, name))
        }
    }
}

enum Outcome {
    Caption(String),
    Media(String),
}

/// Zoom, and not merely a host whose name ends in it — `notzoom.us` is
/// registrable, and a window opened on it would have the probe injected and its
/// answer written into the class tree as a transcript.
fn is_zoom_host(host: &str) -> bool {
    ["zoom.us", "zoom.com"]
        .iter()
        .any(|z| host == *z || host.ends_with(&format!(".{z}")))
}

fn stage_label(state: &str) -> &'static str {
    match state {
        "login" => "Waiting for you to sign in…",
        "passcode" => "Waiting for the recording passcode…",
        "fetching" => "Reading the transcript…",
        "ready" => "Transcript captured",
        "no-transcript" => "No transcript published for this recording",
        _ => "Waiting for the recording to load…",
    }
}

// ---------------------------------------------------------------------------
// Webview

fn open_window(app: &AppHandle, url: &str) -> Result<WebviewWindow> {
    if let Some(existing) = app.get_webview_window(WINDOW_LABEL) {
        // `close` posts to the event loop and returns; the label is only freed
        // once the main thread has processed it. Building immediately fails
        // with "a webview with label zoom-capture already exists", and once it
        // does, every later attempt fails the same way.
        let _ = existing.close();
        let give_up = Instant::now() + Duration::from_secs(3);
        while app.get_webview_window(WINDOW_LABEL).is_some() {
            if Instant::now() >= give_up {
                bail!("the previous Zoom window is still open — close it and try again");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let parsed = tauri::Url::parse(url).context("parsing the recording link")?;
    WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::External(parsed))
        .title("Sign in to Zoom — ClassHub is reading the transcript")
        .inner_size(1100.0, 820.0)
        .build()
        .context("opening the Zoom window")
}

/// Evaluates an expression in the page and returns its JSON result.
///
/// The callback fires once; a script that throws never calls back (Tauri
/// swallows the exception), which the timeout covers.
fn probe(window: &WebviewWindow, js: &str) -> Result<Value> {
    let (tx, rx) = mpsc::channel();
    window
        .eval_with_callback(js, move |result| {
            let _ = tx.send(result);
        })
        .context("evaluating in the Zoom window")?;
    let raw = rx
        .recv_timeout(PROBE_TIMEOUT)
        .context("the Zoom page did not answer")?;
    serde_json::from_str(&raw).with_context(|| format!("probe returned {raw:?}"))
}

/// `name=value; …` across the given URLs, so Rust's own request carries the
/// session the user just established in the window. Deduplicated by name, first
/// URL winning, since the page's own origin is the one that holds the session.
fn cookie_header(window: &WebviewWindow, urls: &[&str]) -> String {
    let mut seen: Vec<String> = Vec::new();
    let mut pairs: Vec<String> = Vec::new();
    for url in urls {
        let Ok(parsed) = tauri::Url::parse(url) else {
            continue;
        };
        for cookie in window.cookies_for_url(parsed).unwrap_or_default() {
            if !seen.iter().any(|n| n == cookie.name()) {
                seen.push(cookie.name().to_string());
                pairs.push(format!("{}={}", cookie.name(), cookie.value()));
            }
        }
    }
    pairs.join("; ")
}

/// The recording page's own origin, for the `Referer` a media host expects.
fn origin_of(url: &str) -> String {
    tauri::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| format!("{}://{h}/", u.scheme())))
        .unwrap_or_else(|| "https://zoom.us/".into())
}

/// Streams the recording to a scratch file. Lecture media runs to gigabytes, so
/// it never lands in memory.
fn download(
    app: &AppHandle,
    url: &str,
    cookies: &str,
    referer: &str,
    on_stage: &dyn Fn(&str),
) -> Result<PathBuf> {
    // The page hands back this URL, so it is checked against the same allowlist
    // the pasted link was — cookies and a session referer are not attached to
    // wherever a page asks.
    let host = tauri::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    if !is_zoom_host(&host) {
        bail!("the recording page pointed at {host}, which is not Zoom");
    }

    let dir = app
        .path()
        .app_data_dir()
        .context("resolving app data dir")?
        .join("zoom");
    std::fs::create_dir_all(&dir)?;
    // Per-run, so two captures cannot truncate one another's download — and so
    // transcribe::workspace, which keys its scratch dir off this name, keeps
    // them apart too.
    let path = dir.join(format!("recording-{}.mp4", crate::db::now()));

    // Bounded, because a stalled socket — a dropped VPN, sleep/wake, a captive
    // portal — otherwise parks this thread forever with the dialog stuck on
    // "Downloading". The blocking client offers no per-read bound, so this is
    // whole-request and correspondingly generous: a backstop, not a budget.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .context("building the download client")?;
    let mut request = client.get(url).header("Referer", referer);
    if !cookies.is_empty() {
        request = request.header("Cookie", cookies);
    }
    let mut response = request.send().context("requesting the recording")?;
    if !response.status().is_success() {
        bail!("Zoom refused the recording download ({})", response.status());
    }

    on_stage("Downloading the recording…");
    let mut file = std::fs::File::create(&path)
        .with_context(|| format!("creating {}", path.display()))?;
    if let Err(e) = response.copy_to(&mut file) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(anyhow::Error::new(e).context("saving the recording"));
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// The page probe
//
// Zoom's recording player is a Vue 2 app mounted on `#app`, and its Vuex store
// is where the transcript actually lives:
//
//   ccUrl          the caption file the player offers for download
//   transcriptList the loaded transcript, {username, ts, endTs, text}
//   viewMp4Url     the recording itself
//
// Reading the store rather than the rendered panel is not a shortcut, it is the
// only correct route: the panel is a `vue-recycle-scroller`, so the DOM holds
// only the rows currently on screen. Scraping it would return twenty cues of a
// three-hour lecture and look like it had worked.
//
// Undocumented and subject to change, so every route reports which one paid off
// and a miss says what the page did have.

const PROBE: &str = r#"
(function () {
  var out = { state: "waiting", text: null, mp4: null, found: [] };

  function clock(v) {
    if (v === null || v === undefined || v === "") return null;
    var s = String(v).trim();
    var sec;
    if (/^\d+(\.\d+)?$/.test(s)) {
      sec = parseFloat(s);
    } else {
      var parts = s.split(":");
      sec = 0;
      for (var i = 0; i < parts.length; i++) {
        var n = parseFloat(parts[i]);
        if (isNaN(n)) return null;
        sec = sec * 60 + n;
      }
    }
    if (isNaN(sec)) return null;
    var ms = Math.max(0, Math.round(sec * 1000));
    var h = Math.floor(ms / 3600000);
    var m = Math.floor((ms % 3600000) / 60000);
    var ss = Math.floor((ms % 60000) / 1000);
    var mmm = ms % 1000;
    function p(n, w) { var t = String(n); while (t.length < w) t = "0" + t; return t; }
    return p(h, 2) + ":" + p(m, 2) + ":" + p(ss, 2) + "." + p(mmm, 3);
  }

  try {
    if (window.__classhub_vtt) { out.state = "ready"; out.text = window.__classhub_vtt; return out; }
    if (window.__classhub_fetching) { out.state = "fetching"; return out; }

    var root = document.querySelector('#app');
    var vue = root && root.__vue__;
    var s = vue && vue.$store && vue.$store.state;

    if (!s) {
      if (document.querySelector('input[type="password"], [name="passcode"]')) {
        out.state = "passcode"; return out;
      }
      if (/signin|login|sso|saml/i.test(location.pathname)) { out.state = "login"; return out; }
      out.found.push("no-store");
      return out;
    }
    out.found.push("store");
    // Pushed before the routes, not after them: every route below returns, so
    // reporting this at the end meant the one diagnostic that explains a failed
    // caption fetch could never reach the caller.
    if (window.__classhub_cc_failed) out.found.push("cc-fetch-failed:" + window.__classhub_cc_failed);

    // 1. The caption file the player itself offers. Real WebVTT, with the
    //    speaker names Parakeet could never recover.
    //
    //    Tried once. Without the spent-marker this branch re-fires on every
    //    poll — the caption token expires, the host restricted the track, the
    //    body comes back empty — and since it returns each time, routes 2 and 3
    //    are never reached and the capture spins out its whole timeout.
    if (s.ccUrl && !window.__classhub_cc_failed) {
      out.found.push("ccUrl");
      window.__classhub_fetching = 1;
      fetch(s.ccUrl, { credentials: "include" })
        .then(function (r) { return r.ok ? r.text() : Promise.reject(r.status); })
        .then(function (t) {
          // An expired session answers a caption URL with a login page: a 200
          // whose body is not a caption track. Filed as source markdown it
          // would look exactly like a successful capture.
          if (t && t.indexOf("-->") !== -1) window.__classhub_vtt = t;
          else window.__classhub_cc_failed = t ? "not-a-caption-track" : "empty";
          window.__classhub_fetching = 0;
        })
        .catch(function (e) { window.__classhub_fetching = 0; window.__classhub_cc_failed = String(e); });
      out.state = "fetching";
      return out;
    }

    // 2. The store's own transcript array. Complete — unlike the rendered
    //    panel, which is virtualized and holds only what is on screen.
    var list = s.transcriptList;
    var rows = [];
    var anyTimed = false;
    if (list && list.length) {
      out.found.push("transcriptList:" + list.length);
      for (var i = 0; i < list.length; i++) {
        var it = list[i];
        var text = (it.text || it.originLangText || "").trim();
        if (!text) continue;
        var who = (it.username || it.name || "").trim();
        var a = clock(it.ts), b = clock(it.endTs);
        if (a) anyTimed = true;
        rows.push({ a: a, b: b, line: who ? who + ": " + text : text });
      }
    }
    // Keyed on the rows that survived, not on the list's length: a list whose
    // entries are all empty would otherwise report "ready" with no text and
    // stall here instead of falling through to the recording.
    if (rows.length) {
      var lines = [];
      if (anyTimed) {
        lines.push("WEBVTT", "");
        var lastEnd = "00:00:00.000";
        for (var j = 0; j < rows.length; j++) {
          // A row missing its own timing is placed at the previous row's end
          // rather than emitted bare: a cue with no timestamp line is not a
          // cue at all to the parser, and would be dropped silently.
          var start = rows[j].a || lastEnd;
          var end = rows[j].b || start;
          lines.push(start + " --> " + end, rows[j].line, "");
          lastEnd = end;
        }
      } else {
        // Blank lines separate turns, so an entirely untimed list still keeps
        // its speakers rather than collapsing into one block.
        out.found.push("untimed");
        for (var k = 0; k < rows.length; k++) lines.push(rows[k].line, "");
      }
      out.state = "ready";
      out.text = lines.join("\n");
      return out;
    }

    // 3. Nothing published. Offer the recording instead — same session, so the
    //    media URL is reachable.
    var mp4 = s.viewMp4Url || s.shareMp4Url || s.gallaryMp4Url || null;
    if (!mp4) {
      var v = document.querySelector("video");
      if (v && v.src && /^https?:/.test(v.src)) mp4 = v.src;
    }
    if (mp4) { out.mp4 = mp4; out.state = "no-transcript"; out.found.push("media"); return out; }

    if (s.accessLevel) out.found.push("access:" + s.accessLevel);
    if (s.isLogin === false) out.found.push("anonymous");
    // Both switched off, and the recording's own metadata has loaded — so this
    // is settled rather than pending. Said plainly, because the caller's job
    // here is to name the manual step; staying "waiting" polls out ten minutes
    // on a page that will never produce either.
    if (window.__classhub_cc_failed || s.accessLevel) out.state = "no-transcript";
    // The player is mounted but still loading its data.
    return out;

  } catch (e) {
    out.found.push("error:" + (e && e.message ? e.message : e));
    return out;
  }
})()
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe itself, actually executed.
    ///
    /// Everything else in this module tests the Rust around the script; this
    /// runs the script. It reads a page nothing here can stand up, so a mock
    /// player under node is the only way to find out whether the routes, the
    /// timestamp conversion and the VTT assembly work at all — and a probe that
    /// is wrong does not crash, it polls for ten minutes and reports that the
    /// page had nothing.
    ///
    /// Skipped rather than failed without node: it is the frontend's toolchain,
    /// not the backend's, and `cargo test` must still pass without it.
    #[test]
    fn the_probe_behaves_against_a_mock_player() {
        let out = std::process::Command::new("node")
            .args(["tests/zoom_probe.mjs", "src/zoom.rs"])
            .output();
        let Ok(out) = out else {
            eprintln!("skipping the probe harness: node is not on PATH");
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
    fn accepts_zoom_and_its_subdomains_only() {
        assert!(is_zoom_host("zoom.us"));
        assert!(is_zoom_host("ufl.zoom.us"));
        assert!(is_zoom_host("ssrweb.zoom.us"));
        assert!(is_zoom_host("zoom.com"));
        // Registrable lookalikes, which a suffix match would have accepted.
        assert!(!is_zoom_host("notzoom.us"));
        assert!(!is_zoom_host("evilzoom.com"));
        assert!(!is_zoom_host("zoom.us.example.com"));
        assert!(!is_zoom_host(""));
    }

    #[test]
    fn derives_the_referer_from_the_page() {
        assert_eq!(origin_of("https://ufl.zoom.us/rec/share/abc"), "https://ufl.zoom.us/");
        assert_eq!(origin_of("not a url"), "https://zoom.us/");
    }

    /// The probe lives in a raw string, so a `"` immediately followed by a `#`
    /// anywhere in the JS would close it early and truncate the script into
    /// something that still compiles. It has happened once.
    #[test]
    fn the_probe_survives_its_raw_string() {
        assert!(!PROBE.contains("\"#"), "the JS terminates its own raw string");
        assert!(PROBE.trim_end().ends_with("})()"), "the probe is not a complete expression");
        for route in ["ccUrl", "transcriptList", "viewMp4Url"] {
            assert!(PROBE.contains(route), "route {route} is missing");
        }
    }

    /// Every route reports itself, and a failed caption fetch is reported
    /// before the routes rather than after them — the branch that returns is
    /// what made the diagnostic unreachable the first time round.
    #[test]
    fn a_failed_caption_fetch_is_reported_and_not_retried() {
        let gate = PROBE.find("if (s.ccUrl").expect("the ccUrl route");
        let diagnostic = PROBE.find("cc-fetch-failed").expect("the failure diagnostic");
        assert!(diagnostic < gate, "the diagnostic is behind a branch that returns");
        assert!(
            PROBE[gate..gate + 60].contains("!window.__classhub_cc_failed"),
            "a failed caption fetch would re-fire on every poll"
        );
    }

    /// The probe's whole output contract is that `transcripts::parse` can read
    /// it. Both shapes it emits are pinned here, because the page it reads from
    /// cannot be stood up in a test and this is the seam that would break.
    #[test]
    fn assembles_vtt_the_parser_can_read() {
        let timed = "WEBVTT\n\n\
            00:00:01.500 --> 00:00:04.000\nEsra Adiyeke: The mean.\n\n\
            00:00:04.000 --> 00:00:04.000\nEsra Adiyeke: And the median.\n\n\
            00:00:09.000 --> 00:00:12.000\nDaniel Escalante: Is that on the exam?\n";
        let cues = crate::transcripts::parse(timed);
        assert_eq!(cues.len(), 3, "{cues:?}");
        assert_eq!(cues[0].speaker.as_deref(), Some("Esra Adiyeke"));
        // The carry-forward shape: a row with no timing of its own lands on the
        // previous row's end, which must still read back as a cue.
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (4_000, 4_000));
        assert_eq!(cues[2].speaker.as_deref(), Some("Daniel Escalante"));

        let untimed = "Esra Adiyeke: The mean.\n\nDaniel Escalante: Is that on the exam?\n\n";
        let cues = crate::transcripts::parse(untimed);
        assert_eq!(cues.len(), 2, "{cues:?}");
        assert_eq!(cues[1].speaker.as_deref(), Some("Daniel Escalante"), "{cues:?}");
    }
}
