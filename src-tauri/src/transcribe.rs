//! On-device lecture transcription via Parakeet (MLX), for recordings that
//! arrive without a caption track — the host had viewer transcripts switched
//! off, or the file is a plain audio export.
//!
//! Like the LibreOffice dependency in `extract.rs`, this is an external tool
//! invoked as a subprocess: a media file goes in, a VTT comes out, and
//! `transcripts.rs` takes it from there. Nothing about the model lives in this
//! codebase.
//!
//! Two things about the host environment are worth knowing:
//!
//! - The model weights (`parakeet-tdt-0.6b-v3`) and the `parakeet_mlx` package
//!   ship inside LocalFlow's bundled venv. LocalFlow keeps a copy of the model
//!   resident, but exposes no socket or port, so there is no way to borrow it —
//!   every run here pays its own model load. Fine for a batch job; it is the
//!   reason this is never on an interactive path.
//! - That venv's `parakeet-mlx` console script carries a stale shebang from the
//!   machine it was built on, so it cannot be executed directly. The module
//!   entry point is invoked instead, which is what the console script does
//!   anyway.
//!
//! Parakeet does no speaker diarization: its output is unattributed text. When
//! Zoom's own caption track exists it is always the better input, because it
//! carries speaker names — and in a lecture, professor-versus-student is most
//! of what makes a transcript worth reading.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::Connection;
use tauri::AppHandle;

use crate::db::{setting, with_conn};

/// LocalFlow's bundled interpreter. Overridable in Settings so that updating or
/// removing LocalFlow is a settings fix rather than a broken feature.
pub const DEFAULT_INTERPRETER: &str =
    "/Applications/LocalFlow.app/Contents/Resources/venv/bin/python3.14";
pub const INTERPRETER_SETTING: &str = "parakeet_python";

const MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v3";
/// What the packaged console script runs; invoked directly because the script
/// itself is unusable (see the module note).
const CLI_ENTRY: &str = "from parakeet_mlx.cli import app; app()";

/// Media containers Parakeet can be handed. Decoding is ffmpeg's job, so this
/// is about what a lecture recording plausibly arrives as, not a codec list.
pub const MEDIA_EXTS: &[&str] = &[
    "m4a", "mp3", "wav", "aac", "flac", "ogg", "opus", "mp4", "m4v", "mov", "webm",
];

/// Past this a transcription is wedged rather than slow. Parakeet runs many
/// times faster than realtime, so even a three-hour lecture lands inside a few
/// minutes; this is a backstop, not a budget.
const TRANSCRIBE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

pub fn is_media(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|ext| MEDIA_EXTS.contains(&ext.as_str()))
}

/// Transcribes a recording and returns the VTT text.
///
/// Progress is reported in stages rather than streamed: over a pipe the CLI's
/// progress rendering collapses to almost nothing, so a stage line is the
/// honest signal. Captured stderr is kept for the error path, where it is the
/// only thing that explains a failure.
pub fn to_vtt(app: &AppHandle, media: &Path, on_stage: &dyn Fn(&str)) -> Result<String> {
    if !media.is_file() {
        bail!("no recording at {}", media.display());
    }
    // Absolute, so the positional argument can never begin with a dash and be
    // read as a flag. Everything already reaching here is absolute; this is
    // what keeps that true rather than assumed.
    let media = &fs::canonicalize(media)
        .with_context(|| format!("resolving {}", media.display()))?;
    let interpreter = interpreter(app);
    if !Path::new(&interpreter).is_file() {
        bail!(
            "no Parakeet interpreter at {} — install LocalFlow or set the path in Settings",
            interpreter.display()
        );
    }

    let out_dir = workspace(app, media)?;
    // A stale directory from an interrupted run would leave its .vtt behind for
    // the single-output lookup below to pick up as if it were this run's.
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir)
        .with_context(|| format!("creating {}", out_dir.display()))?;
    // Held rather than removed at each exit: a spawn failure, a timeout, or a
    // missing output all left the directory — and whatever partial the run had
    // written into it — behind on disk.
    let scratch = Scratch(out_dir);
    let out_dir = &scratch.0;

    on_stage("Loading Parakeet…");
    let child = Command::new(&interpreter)
        .arg("-c")
        .arg(CLI_ENTRY)
        .arg(&media)
        .args(["--model", MODEL, "--output-format", "vtt", "--output-dir"])
        .arg(&out_dir)
        .env("HF_HUB_DISABLE_TELEMETRY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {}", interpreter.display()))?;

    on_stage("Transcribing…");
    let output = crate::jobs::wait_bounded(child, TRANSCRIBE_TIMEOUT).with_context(|| {
        format!(
            "Parakeet did not finish {} within {}h, or could not be waited on — it was stopped",
            media.display(),
            TRANSCRIBE_TIMEOUT.as_secs() / 3600
        )
    })?;
    if !output.status.success() {
        bail!(
            "Parakeet exited {}: {}",
            output.status,
            crate::db::truncate(String::from_utf8_lossy(&output.stderr).trim(), 400)
        );
    }

    // The directory is created fresh per run and holds nothing else, so the one
    // .vtt in it is this run's — more robust than predicting how the CLI
    // derived the name from the source file.
    let produced = sole_vtt(out_dir)?;
    let vtt = fs::read_to_string(&produced)
        .with_context(|| format!("reading {}", produced.display()))?;

    if vtt.trim().is_empty() {
        bail!("Parakeet produced an empty transcript for {}", media.display());
    }
    on_stage("Transcribed");
    Ok(vtt)
}

/// The run's output directory, removed however the run ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn sole_vtt(dir: &Path) -> Result<PathBuf> {
    let found = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("vtt")));
    found.with_context(|| {
        format!("Parakeet reported success but wrote no .vtt into {}", dir.display())
    })
}

fn interpreter(app: &AppHandle) -> PathBuf {
    with_conn(app, |conn| Ok(interpreter_of(conn))).unwrap_or_else(|e| {
        eprintln!("transcribe: {INTERPRETER_SETTING} unreadable ({e:#}) — using the default");
        PathBuf::from(DEFAULT_INTERPRETER)
    })
}

/// The one place the setting is turned into a path. The settings screen reports
/// this and `to_vtt` spawns it, so resolving it twice risks a screen that says
/// the interpreter is present while the run reaches for a different one.
pub fn interpreter_of(conn: &Connection) -> PathBuf {
    setting(conn, INTERPRETER_SETTING)
        .unwrap_or_else(|e| {
            eprintln!("transcribe: {INTERPRETER_SETTING} unreadable ({e:#}) — using the default");
            None
        })
        .filter(|v| !v.trim().is_empty())
        .map(|v| PathBuf::from(v.trim()))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_INTERPRETER))
}

/// A per-recording scratch directory in app data. Keyed by the source name so
/// two concurrent transcriptions cannot land in one another's output.
fn workspace(app: &AppHandle, media: &Path) -> Result<PathBuf> {
    let name = media
        .file_name()
        .map(|n| n.to_string_lossy().replace(['/', '\\', ':'], "_"))
        .unwrap_or_else(|| "recording".into());
    Ok(crate::data_dir(app)?.join("transcribe").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim Parakeet output, captured from a real run of exactly the
    /// invocation above. It is the one input shape this module produces, and
    /// nothing else in the test suite has ever seen it: `parse` must read it,
    /// and — since Parakeet does no diarization — must find no speakers in it.
    const PARAKEET_VTT: &str = "WEBVTT\n\n\
        00:00:00.000 --> 00:00:04.800\n\
        Today, we are going to look at transformers in clinical natural language processing.\n\n\
        00:00:04.800 --> 00:00:07.840\n\
        Remember, the null hypothesis is what we reject.\n";

    #[test]
    fn parses_what_parakeet_actually_writes() {
        let cues = crate::transcripts::parse(PARAKEET_VTT);
        assert_eq!(cues.len(), 2, "{cues:?}");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 4_800));
        assert!(
            cues.iter().all(|c| c.speaker.is_none()),
            "Parakeet writes no speakers: {cues:?}"
        );
        assert!(cues[1].text.starts_with("Remember, the null hypothesis"), "{cues:?}");
    }

    /// The output directory has to go however the run ends — a spawn failure, a
    /// timeout and a missing `.vtt` all used to leave it, and whatever partial
    /// output was in it, on disk.
    #[test]
    fn the_scratch_directory_does_not_outlive_the_run() {
        let dir = std::env::temp_dir().join("classhub-scratch-drop");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create");
        fs::write(dir.join("partial.vtt"), "WEBVTT\n").expect("write");
        {
            let _scratch = Scratch(dir.clone());
            assert!(dir.is_dir());
        }
        assert!(!dir.exists(), "the scratch directory survived the run");
    }

    #[test]
    fn recognises_media_by_extension() {
        assert!(is_media(Path::new("GMT20260824-210000_Recording.m4a")));
        assert!(is_media(Path::new("lecture.MP4")), "extension match is case-insensitive");
        assert!(!is_media(Path::new("transcript.vtt")));
        assert!(!is_media(Path::new("slides.pdf")));
        assert!(!is_media(Path::new("no-extension")));
    }
}
