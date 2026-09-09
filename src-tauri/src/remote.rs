//! The transport the remote pages share (SPEC §7.1, §7.2): the Canvas
//! session window, the Zoom capture window and the Zoom tool's recordings
//! window are each read through `eval_with_callback` and never granted IPC
//! into the app, and the three have one way of evaluating a script with a
//! bound, one way of freeing a window label before reusing it, and one
//! allowlist of Zoom hosts. One place, so a fix to the shape lands once and
//! the allowlist's tests guard every caller.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tauri::{AppHandle, Manager, WebviewWindow};

/// Evaluates an expression in the page and returns its JSON result within
/// `timeout`. The callback fires once; a script that throws never calls
/// back (Tauri swallows the exception), which the timeout covers. `page`
/// names the window in the error, since every caller reads it the same way.
pub fn eval(window: &WebviewWindow, js: &str, timeout: Duration, page: &str) -> Result<Value> {
    let (tx, rx) = mpsc::channel();
    window
        .eval_with_callback(js, move |result| {
            let _ = tx.send(result);
        })
        .with_context(|| format!("evaluating in the {page} window"))?;
    let raw = rx
        .recv_timeout(timeout)
        .with_context(|| format!("the {page} page did not answer"))?;
    serde_json::from_str(&raw).with_context(|| format!("the page returned {raw:?}"))
}

/// How long a closed window's label is waited on before reusing it.
const LABEL_FREED_WITHIN: Duration = Duration::from_secs(3);

/// Closes the window under `label`, if one is open, and waits for the label
/// to free. `close` posts to the event loop and returns; the label is only
/// freed once the main thread has processed it, and building a window on a
/// taken label fails — every later attempt the same way.
pub fn close_label(app: &AppHandle, label: &str, page: &str) -> Result<()> {
    let Some(existing) = app.get_webview_window(label) else {
        return Ok(());
    };
    let _ = existing.close();
    let give_up = Instant::now() + LABEL_FREED_WITHIN;
    while app.get_webview_window(label).is_some() {
        if Instant::now() >= give_up {
            bail!("the previous {page} window is still open — close it and try again");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Zoom, and not merely a host whose name ends in it — `notzoom.us` is
/// registrable, and a window opened on it would have the probe injected and
/// its answer written into the class tree as a transcript.
pub fn is_zoom_host(host: &str) -> bool {
    ["zoom.us", "zoom.com"]
        .iter()
        .any(|z| host == *z || host.ends_with(&format!(".{z}")))
}

/// A JS string literal for `value`, quotes included.
pub fn js_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_zoom_and_its_subdomains_only() {
        assert!(is_zoom_host("zoom.us"));
        assert!(is_zoom_host("ufl.zoom.us"));
        assert!(is_zoom_host("ssrweb.zoom.us"));
        assert!(is_zoom_host("applications.zoom.us"));
        assert!(is_zoom_host("zoom.com"));
        // Registrable lookalikes, which a suffix match would have accepted.
        assert!(!is_zoom_host("notzoom.us"));
        assert!(!is_zoom_host("evilzoom.com"));
        assert!(!is_zoom_host("zoom.us.example.com"));
        assert!(!is_zoom_host(""));
    }

    #[test]
    fn a_js_string_is_quoted_and_escaped() {
        assert_eq!(js_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(js_string("</script>"), "\"</script>\"");
    }
}
