//! SPEC §7 — lecture transcript normalization. Zero tokens, zero dependencies.
//!
//! Zoom hands out a caption track (`.vtt`, `.srt`, or the in-meeting "Save
//! Transcript" `.txt`) and Parakeet writes `.vtt`; all three are the same shape
//! once parsed — a run of cues, each a timestamp range plus a line of speech.
//!
//! Cue *merging* is the load-bearing part. A raw Zoom track carries one cue per
//! couple of seconds, so a lecture arrives as thousands of two-second fragments:
//! unreadable on its own, and token-hostile as the input to a digest job. Merging
//! consecutive cues from one speaker back into paragraphs is what turns a caption
//! track into prose a reader — or a synthesis prompt — can actually work through.
//!
//! The output is markdown written into the class tree as ordinary source
//! material, so the scanner indexes it, `extract::route` sends it down
//! `Route::Text` at zero tokens, and guides and chat pick it up unchanged.

use std::fmt::Write as _;

/// One caption cue: a timestamp range, an optional speaker, and the words.
#[derive(Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
}

/// A merged run of consecutive cues from one speaker — one markdown paragraph.
#[derive(Debug, PartialEq, Eq)]
pub struct Paragraph {
    pub start_ms: i64,
    pub speaker: Option<String>,
    pub text: String,
}

/// What the header line says about where this transcript came from.
pub struct Meta<'a> {
    pub title: &'a str,
    /// ISO `YYYY-MM-DD`.
    pub date: &'a str,
    pub source_name: &'a str,
}

// ---------------------------------------------------------------------------
// Parsing

/// A gap longer than this between two cues from the same speaker reads as a
/// pause rather than a breath, so the paragraph breaks there.
const GAP_BREAK_MS: i64 = 4_000;
/// Soft cap on paragraph length. Only ever applied at a sentence boundary, so
/// a long uninterrupted stretch still breaks somewhere a reader would.
const MAX_PARAGRAPH_CHARS: usize = 1_500;
/// How often a `## HH:MM` anchor is emitted, so a digest can cite a timestamp
/// and a reader can find the moment in the recording.
const SECTION_MS: i64 = 5 * 60 * 1_000;

/// Parses VTT, SRT, or Zoom's saved-caption TXT — they differ only in the
/// millisecond separator and the header noise, and this keys off the `-->`
/// lines, so cue identifiers, `WEBVTT`, `NOTE` blocks and cue settings all fall
/// away without needing to be understood.
///
/// A file with no timestamps at all (someone pasted plain text) is not an
/// error: it comes back as a single untimed cue.
pub fn parse(source: &str) -> Vec<Cue> {
    let mut raw = scan_cues(source);
    if raw.is_empty() {
        raw = untimed_blocks(source);
    }
    let seen = attribution(&raw);

    let mut cues: Vec<Cue> = raw
        .into_iter()
        .filter_map(|(start_ms, end_ms, payload)| {
            let (speaker, text) = split_speaker(&payload, &seen);
            (!text.is_empty()).then_some(Cue { start_ms, end_ms, speaker, text })
        })
        .collect();
    // Merging measures the gap between one cue and the last, and anchors assume
    // the clock only moves forward. Two concatenated tracks or an unsorted
    // player list break both — a backwards gap is negative so the pause rule
    // never fires, and the section anchors come out non-monotonic.
    if cues.iter().any(|c| c.end_ms > 0) {
        cues.sort_by_key(|c| c.start_ms);
    }
    cues
}

/// A transcript with no timing grid at all: Zoom's plain-text save, text pasted
/// out of a page, or a player's in-memory list whose timestamps were unusable.
/// Blank lines separate turns, so each block is its own cue — collapsing the
/// whole file into one would throw away every speaker change in it.
fn untimed_blocks(source: &str) -> Vec<(i64, i64, String)> {
    // Split on blank lines via `lines()` rather than on a literal "\n\n": a
    // CRLF file contains no "\n\n" at all, so the whole transcript would come
    // back as one block with every speaker change in it thrown away.
    let mut out = Vec::new();
    let mut block = String::new();
    for line in source.lines() {
        if line.trim().is_empty() {
            if !block.trim().is_empty() {
                out.push((0, 0, collapse_ws(&block)));
            }
            block.clear();
        } else {
            block.push(' ');
            block.push_str(line);
        }
    }
    if !block.trim().is_empty() {
        out.push((0, 0, collapse_ws(&block)));
    }
    out
}

/// Raw `(start, end, payload)` triples, before any speaker interpretation.
fn scan_cues(source: &str) -> Vec<(i64, i64, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let Some((start_ms, end_ms)) = parse_timing(lines[i]) else {
            i += 1;
            continue;
        };
        i += 1;
        // Payload runs to the blank line; a `-->` before that means the cue was
        // truncated mid-write, so it ends there and the next one starts clean.
        let mut payload = String::new();
        while i < lines.len() && !lines[i].trim().is_empty() && parse_timing(lines[i]).is_none() {
            if !payload.is_empty() {
                payload.push(' ');
            }
            payload.push_str(lines[i].trim());
            i += 1;
        }
        out.push((start_ms, end_ms, payload));
    }
    out
}

/// A one-word `Name:` prefix is genuinely ambiguous in isolation — `Danny:` and
/// `Remember:` are the same shape. Across a whole lecture they are not: a
/// display name recurs cue after cue, a rhetorical lead-in appears once. So a
/// single-word candidate has to earn its place by repeating.
const MIN_SINGLE_WORD_HITS: usize = 2;
/// A file is speaker-labeled when at least this fraction of its cues open with
/// a name-shaped prefix. Generous, because some tracks carry the name only on a
/// change of speaker — but three orders of magnitude above the rate at which a
/// sentence happens to start with one.
const LABELED_CUES_IN: usize = 4;

/// What the file as a whole says about attribution.
struct Attribution {
    /// Candidates that appear often enough to be somebody's name.
    recurring: std::collections::HashSet<String>,
    /// Whether this file carries speaker labels at all.
    labeled: bool,
}

/// Whether a `Name:` prefix is a name cannot be settled from the prefix alone:
/// `Law of Large Numbers:` and `Maria de la Cruz:` pass every structural test
/// there is, and taking the first for a speaker *deletes the phrase* from the
/// transcript. What separates them is the file around them. A caption track
/// labels its cues — Zoom writes the name on every one — so a name-shaped
/// prefix there is a name. Parakeet writes no speakers at all, so in its output
/// the handful of cues opening with a capitalized phrase are sentences, and the
/// whole file has to be read that way.
fn attribution(raw: &[(i64, i64, String)]) -> Attribution {
    let mut hits: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut candidates = 0usize;
    for (_, _, payload) in raw {
        if let Some(head) = speaker_candidate(payload) {
            candidates += 1;
            *hits.entry(head).or_default() += 1;
        }
    }
    Attribution {
        recurring: hits
            .into_iter()
            .filter(|(_, n)| *n >= MIN_SINGLE_WORD_HITS)
            .map(|(head, _)| head)
            .collect(),
        labeled: !raw.is_empty() && candidates * LABELED_CUES_IN >= raw.len(),
    }
}

/// The `Name:` prefix of a payload, if it has the shape of one at all.
fn speaker_candidate(payload: &str) -> Option<String> {
    let plain = strip_tags(payload.trim());
    let (head, _) = plain.split_once(':')?;
    looks_like_speaker(head).then(|| head.trim().to_string())
}

/// `00:00:01.500 --> 00:00:04.000 align:start` → `(1500, 4000)`. Trailing cue
/// settings are ignored; SRT's comma separator is accepted alongside VTT's dot.
fn parse_timing(line: &str) -> Option<(i64, i64)> {
    let (left, right) = line.split_once("-->")?;
    let start = parse_timestamp(left.trim())?;
    let end = parse_timestamp(right.trim().split_whitespace().next()?)?;
    Some((start, end))
}

/// `HH:MM:SS.mmm`, `MM:SS.mmm`, or either with SRT's `,mmm`.
fn parse_timestamp(stamp: &str) -> Option<i64> {
    let (clock, frac) = match stamp.split_once(['.', ',']) {
        Some((c, f)) => (c, f),
        None => (stamp, ""),
    };
    let mut seconds = 0i64;
    let mut parts = 0;
    for part in clock.split(':') {
        seconds = seconds.checked_mul(60)?.checked_add(part.trim().parse::<i64>().ok()?)?;
        parts += 1;
    }
    if !(2..=3).contains(&parts) {
        return None;
    }
    // Pad or clip to exactly three digits so `.5` is 500ms, not 5ms.
    let millis: i64 = if frac.is_empty() {
        0
    } else {
        let digits: String = frac.chars().filter(char::is_ascii_digit).take(3).collect();
        format!("{digits:0<3}").parse().ok()?
    };
    Some(seconds * 1_000 + millis)
}

/// Pulls the speaker off a cue payload. Zoom writes `Name: text`; the VTT spec
/// writes `<v Name>text</v>`. Both appear in the wild, sometimes in one file.
fn split_speaker(payload: &str, seen: &Attribution) -> (Option<String>, String) {
    let payload = payload.trim();
    // A voice span is explicit markup, not a guess — it needs no corroboration.
    if let Some(rest) = payload.strip_prefix("<v ") {
        if let Some((name, text)) = rest.split_once('>') {
            let name = name.trim().trim_end_matches('.').trim();
            return (
                (!name.is_empty()).then(|| name.to_string()),
                collapse_ws(&strip_tags(text)),
            );
        }
    }
    let plain = strip_tags(payload);
    if let Some((_, tail)) = plain.split_once(':') {
        if let Some(name) = speaker_candidate(payload) {
            // Counted on the same stripped form `looks_like_speaker` judged, or
            // a pronoun parenthetical would pass this as "multi-word" while
            // having been judged as the single word it really is.
            let multi = strip_parentheticals(&name).split_whitespace().count() > 1;
            if seen.labeled && (multi || seen.recurring.contains(&name)) {
                return (Some(name), collapse_ws(tail));
            }
        }
    }
    (None, collapse_ws(&plain))
}

/// Lowercase particles that belong inside a name ("Maria de la Cruz").
const NAME_PARTICLES: &[&str] = &[
    "van", "von", "de", "del", "della", "da", "di", "du", "la", "le", "bin", "ibn", "al",
    "ter", "ten", "of", "der", "den",
];

/// A name, not a mid-sentence colon. Length and word count alone are far too
/// weak — "So here's the thing:" passes both — so the real test is
/// capitalization: every word of a display name starts uncased or uppercase,
/// while an English clause is full of lowercase function words.
///
/// `is_lowercase` rather than `is_uppercase` is deliberate, so that digits
/// ("Speaker 1") and uncased scripts pass instead of being rejected for having
/// no capital form.
fn looks_like_speaker(head: &str) -> bool {
    let head = head.trim();
    if head.is_empty() || head.chars().count() > 48 || head.contains(['!', '?', ';']) {
        return false;
    }
    // Zoom display names often carry a pronoun parenthetical, which is not part
    // of the name and is lowercase.
    let bare = strip_parentheticals(head);
    let words: Vec<&str> = bare.split_whitespace().collect();
    if words.is_empty() || words.len() > 5 {
        return false;
    }
    words.iter().all(|word| {
        NAME_PARTICLES.contains(&word.to_ascii_lowercase().as_str())
            || word
                .chars()
                .find(|c| c.is_alphanumeric())
                .is_some_and(|c| !c.is_lowercase())
    })
}

fn strip_parentheticals(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

/// Drops VTT inline markup (`<i>`, `<c.colorE5E5E5>`, `</v>`) and leaves text.
///
/// A lone `<` is *not* markup. "if n < 30 we use the t distribution, but if
/// n > 30" is a sentence a statistics lecturer says out loud, and treating the
/// span between the two as a tag deletes forty characters of it silently. So a
/// tag has to look like one: a letter or `/` immediately after the `<`, and a
/// closing `>` within the length any VTT cue setting actually runs to.
fn strip_tags(text: &str) -> String {
    /// `<c.colorE5E5E5.bg_black>` is about as long as these legitimately get.
    const MAX_TAG_LEN: usize = 64;

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        let after = &rest[open + 1..];
        let tag_like = after
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/');
        match after.find('>').filter(|end| tag_like && *end <= MAX_TAG_LEN) {
            Some(end) => {
                out.push_str(&rest[..open]);
                rest = &after[end + 1..];
            }
            // Not markup — keep the `<` and carry on past it.
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    decode_entities(&out)
}

/// The escapes a spec-conformant VTT uses. Zoom rarely emits them; a caption
/// track converted by anything else routinely does, and `&amp;` reaching the
/// filed markdown verbatim is the kind of wrong nothing downstream can undo.
fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        // Last, so `&amp;lt;` decodes to `&lt;` rather than to `<`.
        .replace("&amp;", "&")
}

fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Merging

/// Collapses a caption track back into paragraphs. A new paragraph starts when
/// the speaker changes, when the pause runs long, or when a paragraph has grown
/// past the soft cap and the previous cue landed on a sentence boundary.
pub fn merge(cues: &[Cue]) -> Vec<Paragraph> {
    // Without timings there is no gap to measure and no length worth capping —
    // the blank-line turns `untimed_blocks` preserved are the only structure
    // the file has, so each stays its own paragraph instead of running back
    // together into one.
    let timed = cues.iter().any(|c| c.end_ms > 0);
    let mut paragraphs: Vec<Paragraph> = Vec::new();
    let mut last_end = 0i64;

    for cue in cues {
        let split = match paragraphs.last() {
            None => true,
            Some(current) => {
                !timed
                    || current.speaker != cue.speaker
                    || cue.start_ms - last_end > GAP_BREAK_MS
                    // Crossing a section boundary, so every section that has
                    // speech in it starts a paragraph and therefore gets an
                    // anchor. Without this a speaker on a long unbroken run
                    // takes ninety minutes of lecture into one paragraph under
                    // a single `## 00:00`.
                    || current.start_ms / SECTION_MS != cue.start_ms / SECTION_MS
                    || over_length(&current.text)
            }
        };
        if split {
            paragraphs.push(Paragraph {
                start_ms: cue.start_ms,
                speaker: cue.speaker.clone(),
                text: cue.text.clone(),
            });
        } else if let Some(current) = paragraphs.last_mut() {
            current.text.push(' ');
            current.text.push_str(&cue.text);
        }
        last_end = cue.end_ms.max(cue.start_ms);
    }
    paragraphs
}

/// Past the soft cap at a sentence boundary, or past a hard ceiling regardless.
/// Auto-captions routinely carry no sentence-final punctuation at all, and
/// waiting for one that never comes produces a single unreadable block.
fn over_length(text: &str) -> bool {
    let len = text.len();
    len >= MAX_PARAGRAPH_CHARS && (ends_sentence(text) || len >= 2 * MAX_PARAGRAPH_CHARS)
}

fn ends_sentence(text: &str) -> bool {
    matches!(text.trim_end().chars().last(), Some('.' | '!' | '?'))
}

// ---------------------------------------------------------------------------
// Rendering

/// Renders parsed cues as the markdown that gets written into the class tree.
pub fn to_markdown(cues: &[Cue], meta: &Meta<'_>) -> String {
    let paragraphs = merge(cues);
    let duration_ms = cues.iter().map(|c| c.end_ms.max(c.start_ms)).max().unwrap_or(0);
    let timed = cues.iter().any(|c| c.end_ms > 0);

    let mut out = String::with_capacity(cues.iter().map(|c| c.text.len() + 16).sum());
    let _ = writeln!(out, "# {}\n", meta.title.trim());

    // The date is absent when a caption track is normalized in place during
    // extraction, where nothing has established which session it belongs to.
    let mut facts = Vec::new();
    if !meta.date.is_empty() {
        facts.push(format!("Recorded {}", meta.date));
    }
    if duration_ms > 0 {
        facts.push(human_duration(duration_ms));
    }
    facts.push(format!("source: {}", meta.source_name));
    let _ = writeln!(out, "{}\n", facts.join(" · "));

    // Speakers up front: a digest prompt reading this benefits from knowing who
    // is in the room before it meets them mid-transcript.
    let mut speakers: Vec<&str> = Vec::new();
    for p in &paragraphs {
        if let Some(name) = p.speaker.as_deref() {
            if !speakers.contains(&name) {
                speakers.push(name);
            }
        }
    }
    if !speakers.is_empty() {
        let _ = writeln!(out, "Speakers: {}\n", speakers.join(", "));
    }

    let mut section = -1i64;
    for paragraph in &paragraphs {
        if timed {
            let current = paragraph.start_ms / SECTION_MS;
            if current != section {
                section = current;
                // The paragraph's own start, not the section boundary it falls
                // in. A digest citing an anchor is telling the reader where to
                // scrub to, and the boundary can be five minutes early.
                let _ = writeln!(out, "## {}\n", clock(paragraph.start_ms));
            }
        }
        match paragraph.speaker.as_deref() {
            Some(name) => {
                let _ = writeln!(out, "**{name}:** {}\n", paragraph.text);
            }
            None => {
                let _ = writeln!(out, "{}\n", paragraph.text);
            }
        }
    }
    out
}

/// A one-line characterization of a filed transcript, or `None` when the
/// markdown is not one of ours.
///
/// The sorter uses this. A lecture's file name carries almost no routing signal
/// — every one of them is a date and the word "Lecture" — so what tells the
/// model which module it belongs to is the subject matter, and this is what
/// puts a little of that in front of it without making it open every file.
pub fn describe(markdown: &str) -> Option<String> {
    let mut lines = markdown.lines();
    let title = lines.find(|l| !l.trim().is_empty())?.strip_prefix("# ")?;

    let head: Vec<&str> = lines.take(6).collect();
    // The facts line is what identifies the file as a transcript we wrote.
    head.iter().find(|l| l.contains("source: "))?;

    let mut parts = vec![format!("lecture transcript \"{}\"", title.trim())];
    if let Some(speakers) = head.iter().find_map(|l| l.strip_prefix("Speakers: ")) {
        parts.push(format!("speakers: {}", speakers.trim()));
    }
    // The first spoken paragraph says what the session was about far more
    // reliably than any of the header does — so the search starts past the
    // header. Both the facts line and the speaker list run well over the length
    // test below, and a real Zoom source name is long enough that the facts
    // line would otherwise always win and hand the sorter back a file name.
    if let Some(opening) = markdown
        .lines()
        .skip_while(|l| !l.contains("source: "))
        .skip(1)
        .find(|l| {
            !l.starts_with("Speakers: ")
                && (l.starts_with("**") || (!l.starts_with(['#', '*']) && l.len() > 60))
        })
    {
        parts.push(format!("opens: \"{}\"", truncate_words(opening.trim(), 24)));
    }
    Some(parts.join(" · "))
}

fn truncate_words(text: &str, words: usize) -> String {
    let taken: Vec<&str> = text.split_whitespace().take(words).collect();
    let joined = taken.join(" ");
    if text.split_whitespace().count() > words {
        format!("{joined}…")
    } else {
        joined
    }
}

/// `HH:MM` for section anchors, so a digest's citation matches the recording's
/// own scrubber.
fn clock(ms: i64) -> String {
    let total_minutes = ms / 60_000;
    format!("{:02}:{:02}", total_minutes / 60, total_minutes % 60)
}

fn human_duration(ms: i64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZOOM_VTT: &str = "WEBVTT\n\n1\n00:00:01.500 --> 00:00:04.000\n\
        Benjamin Shickel: Today we're going to look\n\n\
        2\n00:00:04.000 --> 00:00:06.500\n\
        Benjamin Shickel: at transformers in clinical NLP.\n\n\
        3\n00:00:07.000 --> 00:00:09.000\n\
        Daniel Escalante: Is that the same as BERT?\n";

    #[test]
    fn parses_a_zoom_vtt_with_speakers() {
        let cues = parse(ZOOM_VTT);
        assert_eq!(cues.len(), 3, "{cues:?}");
        assert_eq!(cues[0].start_ms, 1_500);
        assert_eq!(cues[0].end_ms, 4_000);
        assert_eq!(cues[0].speaker.as_deref(), Some("Benjamin Shickel"));
        assert_eq!(cues[0].text, "Today we're going to look");
    }

    /// The whole point of the module: three thousand fragments become prose.
    #[test]
    fn merges_consecutive_cues_from_one_speaker() {
        let merged = merge(&parse(ZOOM_VTT));
        assert_eq!(merged.len(), 2, "{merged:?}");
        assert_eq!(
            merged[0].text,
            "Today we're going to look at transformers in clinical NLP."
        );
        assert_eq!(merged[1].speaker.as_deref(), Some("Daniel Escalante"));
    }

    #[test]
    fn breaks_a_paragraph_on_a_long_pause() {
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nOne.\n\n\
                   00:00:30.000 --> 00:00:32.000\nTwo.\n";
        assert_eq!(merge(&parse(vtt)).len(), 2);
    }

    #[test]
    fn accepts_srt_comma_milliseconds() {
        let srt = "1\n00:00:01,250 --> 00:00:03,000\nHello there.\n";
        let cues = parse(srt);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 1_250);
    }

    #[test]
    fn accepts_two_part_timestamps_and_cue_settings() {
        let vtt = "WEBVTT\n\n00:01.500 --> 00:04.000 align:start position:0%\nWords.\n";
        let cues = parse(vtt);
        assert_eq!(cues.len(), 1, "{cues:?}");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (1_500, 4_000));
    }

    #[test]
    fn reads_vtt_voice_spans() {
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n<v Esra Adiyeke>The mean.</v>\n";
        let cues = parse(vtt);
        assert_eq!(cues[0].speaker.as_deref(), Some("Esra Adiyeke"));
        assert_eq!(cues[0].text, "The mean.");
    }

    /// A colon inside a sentence is not a speaker label — getting this wrong
    /// silently shreds the transcript into fake speakers.
    #[test]
    fn does_not_mistake_a_mid_sentence_colon_for_a_speaker() {
        for line in [
            "So here's the thing: we need a baseline.",
            "Remember: the null hypothesis is what we reject.",
            "One caveat: this only holds for large n.",
        ] {
            let cues = parse(&format!("WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n{line}\n"));
            assert_eq!(cues[0].speaker, None, "{line:?} → {cues:?}");
            assert_eq!(cues[0].text, line);
        }
    }

    /// A one-word name earns its status by recurring through the lecture; a
    /// one-off rhetorical lead-in of the identical shape does not.
    #[test]
    fn single_word_speakers_need_corroboration() {
        let vtt = "WEBVTT\n\n\
            00:00:00.000 --> 00:00:02.000\nDanny: First question.\n\n\
            00:00:10.000 --> 00:00:12.000\nRemember: this is not a name.\n\n\
            00:00:20.000 --> 00:00:22.000\nDanny: Second question.\n";
        let cues = parse(vtt);
        assert_eq!(cues[0].speaker.as_deref(), Some("Danny"), "{cues:?}");
        assert_eq!(cues[1].speaker, None, "{cues:?}");
        assert_eq!(cues[1].text, "Remember: this is not a name.");
        assert_eq!(cues[2].speaker.as_deref(), Some("Danny"), "{cues:?}");
    }

    /// The failure this rule exists for, and the expensive direction of it: an
    /// unattributed transcript (Parakeet writes no speakers) whose sentences
    /// happen to open with a capitalized phrase. Reading one as a speaker would
    /// delete the phrase, so the whole file has to be read as unlabeled.
    #[test]
    fn keeps_capitalized_lead_ins_out_of_an_unlabeled_transcript() {
        // The proportions of a real machine transcript: a wall of prose, with
        // the occasional sentence that opens like a name.
        let lead_ins = [
            "Law of Large Numbers: as n grows the sample mean converges.",
            "Chapter 3: sampling distributions, and why they are not the data.",
            "Learning Objectives: describe the difference between the two.",
            "Note (important): this is the part people get wrong on the exam.",
            "Yeah, Marcus: that is exactly the case I had in mind.",
        ];
        let mut vtt = String::from("WEBVTT\n\n");
        let mut expected = Vec::new();
        for i in 0..60usize {
            let line = match i % 12 {
                0 => lead_ins[i / 12 % lead_ins.len()],
                _ => "So the estimator is consistent, which is what we want here.",
            };
            expected.push(line);
            let s = i as i64 * 3_000;
            vtt.push_str(&format!("{}\n{line}\n\n", timing(s, s + 3_000)));
        }

        let cues = parse(&vtt);
        assert_eq!(cues.len(), 60, "{:?}", &cues[..4]);
        assert!(cues.iter().all(|c| c.speaker.is_none()), "{:?}", &cues[..4]);
        // Every phrase must survive in the text, not vanish into a speaker name.
        for (cue, line) in cues.iter().zip(&expected) {
            assert_eq!(&cue.text, line);
        }
    }

    /// The same phrases in a file that *is* labeled sit behind a real name, so
    /// the first colon takes the speaker and the phrase stays in the text.
    #[test]
    fn a_labeled_track_still_attributes_and_keeps_the_phrase() {
        let vtt = "WEBVTT\n\n\
            00:00:00.000 --> 00:00:03.000\n\
            Esra Adiyeke: Law of Large Numbers: as n grows.\n\n\
            00:00:03.000 --> 00:00:06.000\n\
            Esra Adiyeke: Chapter 3: sampling distributions.\n\n\
            00:00:06.000 --> 00:00:09.000\n\
            Daniel Escalante: Is that on the exam?\n";
        let cues = parse(vtt);
        assert_eq!(cues[0].speaker.as_deref(), Some("Esra Adiyeke"), "{cues:?}");
        assert_eq!(cues[0].text, "Law of Large Numbers: as n grows.");
        assert_eq!(cues[1].text, "Chapter 3: sampling distributions.");
        // A student who speaks once is still a speaker in a labeled file.
        assert_eq!(cues[2].speaker.as_deref(), Some("Daniel Escalante"), "{cues:?}");
    }

    fn timing(start_ms: i64, end_ms: i64) -> String {
        let stamp = |ms: i64| {
            format!("{:02}:{:02}:{:02}.{:03}", ms / 3_600_000, ms % 3_600_000 / 60_000, ms % 60_000 / 1_000, ms % 1_000)
        };
        format!("{} --> {}", stamp(start_ms), stamp(end_ms))
    }

    /// The display-name shapes a university Zoom room actually produces.
    #[test]
    fn reads_real_display_name_shapes() {
        for (line, name, text) in [
            ("Shickel, Benjamin: The residual stream.", "Shickel, Benjamin", "The residual stream."),
            ("Esra Adiyeke (she/her): The mean.", "Esra Adiyeke (she/her)", "The mean."),
            ("Dr. Ozrazgat Baslanti: A t-test.", "Dr. Ozrazgat Baslanti", "A t-test."),
            ("Speaker 1: Unknown voice.", "Speaker 1", "Unknown voice."),
            ("Maria de la Cruz: A question.", "Maria de la Cruz", "A question."),
        ] {
            let cues = parse(&format!("WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n{line}\n"));
            assert_eq!(cues[0].speaker.as_deref(), Some(name), "{line:?}");
            assert_eq!(cues[0].text, text, "{line:?}");
        }
    }

    #[test]
    fn treats_untimed_text_as_one_cue() {
        let cues = parse("Just some pasted lecture notes.\nNo timestamps at all.");
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Just some pasted lecture notes. No timestamps at all.");
    }

    /// An untimed transcript still has turns in it. Collapsing the file into a
    /// single cue would silently discard every speaker in it.
    #[test]
    fn keeps_speakers_in_untimed_text() {
        let cues = parse(
            "Esra Adiyeke: The median is the honest summary.\n\n\
             Daniel Escalante: Is there a rule for that?\n\n\
             Esra Adiyeke: No hard threshold.",
        );
        assert_eq!(cues.len(), 3, "{cues:?}");
        assert_eq!(cues[0].speaker.as_deref(), Some("Esra Adiyeke"));
        assert_eq!(cues[1].speaker.as_deref(), Some("Daniel Escalante"));
        assert_eq!(cues[1].text, "Is there a rule for that?");
    }

    /// A half-downloaded caption file is a real input; it must not panic or
    /// spin, and the cues before the tear must survive.
    #[test]
    fn survives_malformed_and_truncated_input() {
        let _ = parse("");
        let _ = parse("WEBVTT\n\n00:00:00.000 --> \nDangling arrow\n");
        let _ = parse("00:00:00.000 --> 00:00:02.000\n<v Unclosed tag");
        let _ = parse("not:a:timestamp --> also:not\ntext\n");

        let torn = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\nFirst.\n\n\
                    00:00:02.000 --> 00:00:04.000\n";
        assert_eq!(parse(torn).len(), 1);
    }

    #[test]
    fn renders_markdown_with_anchors_and_a_speaker_list() {
        let md = to_markdown(
            &parse(ZOOM_VTT),
            &Meta {
                title: "AI in Health Design Studio I — 2026-08-24",
                date: "2026-08-24",
                source_name: "GMT20260824-210000_Recording.vtt",
            },
        );
        assert!(md.starts_with("# AI in Health Design Studio I — 2026-08-24"), "{md}");
        assert!(md.contains("Speakers: Benjamin Shickel, Daniel Escalante"), "{md}");
        assert!(md.contains("## 00:00"), "{md}");
        assert!(md.contains("**Daniel Escalante:** Is that the same as BERT?"), "{md}");
        assert!(md.contains("source: GMT20260824-210000_Recording.vtt"), "{md}");
    }

    /// A lecture-shaped file: many two-second cues, long same-speaker runs,
    /// real pauses, three speakers. This is the shape the merger exists for, so
    /// it is asserted on the property that matters — a caption grid collapsing
    /// into far fewer paragraphs without losing a speaker or a word.
    const LECTURE_VTT: &str = "WEBVTT\n\n\
        1\n00:00:03.120 --> 00:00:06.480\n\
        Esra Adiyeke: Okay, I think we're recording now.\n\n\
        2\n00:00:06.480 --> 00:00:10.220\n\
        Esra Adiyeke: Today is all about measures of central tendency,\n\n\
        3\n00:00:10.220 --> 00:00:13.900\n\
        Esra Adiyeke: and then we'll move into spread in the second half.\n\n\
        4\n00:00:14.100 --> 00:00:18.640\n\
        Esra Adiyeke: So the mean, the median, the mode.\n\n\
        5\n00:00:48.200 --> 00:00:52.740\n\
        Esra Adiyeke: Here's the clinical example: hospital length of stay.\n\n\
        6\n00:01:12.300 --> 00:01:15.640\n\
        Daniel Escalante: Is there a rule for how skewed it has to be?\n\n\
        7\n00:01:16.000 --> 00:01:20.480\n\
        Esra Adiyeke: Good question. There's no hard threshold.\n\n\
        8\n00:06:45.000 --> 00:06:49.480\n\
        Tezcan Ozrazgat Baslanti: The mode is mostly for categorical data.\n";

    #[test]
    fn collapses_a_lecture_caption_grid_into_prose() {
        let cues = parse(LECTURE_VTT);
        assert_eq!(cues.len(), 8, "{cues:?}");

        let merged = merge(&cues);
        assert!(
            merged.len() < cues.len(),
            "merging did nothing: {} cues → {} paragraphs",
            cues.len(),
            merged.len()
        );
        // Cues 1–4 run together; 5 is past the pause; 6, 7 and 8 each change
        // speaker or follow a long gap.
        assert_eq!(merged.len(), 5, "{merged:?}");
        assert!(
            merged[0].text.starts_with("Okay, I think we're recording now. Today is all about"),
            "{:?}",
            merged[0].text
        );

        let md = to_markdown(
            &cues,
            &Meta {
                title: "2026-08-20 — Lecture",
                date: "2026-08-20",
                source_name: "GMT20260820-114500_Recording.transcript.vtt",
            },
        );
        assert!(
            md.contains("Speakers: Esra Adiyeke, Daniel Escalante, Tezcan Ozrazgat Baslanti"),
            "{md}"
        );
        // Anchors track the recording's own clock, so a digest can cite a time
        // that scrubs to the right moment — the paragraph's own start, not the
        // five-minute boundary it happens to fall inside, which would send the
        // reader up to five minutes early.
        assert!(md.contains("## 00:00"), "{md}");
        assert!(md.contains("## 00:06"), "{md}");
        assert!(!md.contains("## 00:05"), "anchored to the bucket, not the speech: {md}");
        assert!(
            md.contains(
                "Recorded 2026-08-20 · 6m · source: GMT20260820-114500_Recording.transcript.vtt"
            ),
            "{md}"
        );
    }

    /// What the sorter is given has to be the subject matter — the file name
    /// carries no routing signal, which is the whole reason this exists. The
    /// source name here is a real Zoom one deliberately: a short one leaves the
    /// facts line under the length test and hides the bug this pins.
    #[test]
    fn describes_a_transcript_for_the_sorter() {
        let md = to_markdown(
            &parse(ZOOM_VTT),
            &Meta {
                title: "2026-08-24 — Lecture",
                date: "2026-08-24",
                source_name: "GMT20260824-210000_Recording.transcript.vtt",
            },
        );
        let described = describe(&md).expect("described");
        assert!(described.contains("lecture transcript"), "{described}");
        assert!(described.contains("Benjamin Shickel"), "{described}");
        assert!(described.contains("opens: \"**Benjamin Shickel:** Today we're"), "{described}");
        assert!(!described.contains("source:"), "the header is not the opening: {described}");
    }

    /// Everything else in an inbox is not a transcript, and must not be
    /// announced to the sorter as one.
    #[test]
    fn declines_to_describe_other_markdown() {
        assert_eq!(describe(""), None);
        assert_eq!(describe("Just notes, no heading."), None);
        assert_eq!(describe("# A note\n\nSome content about the reading."), None);
    }

    /// A lone `<` in speech is not markup, and treating it as such deletes
    /// everything up to the next `>` — a whole clause of a statistics lecture.
    #[test]
    fn keeps_comparisons_and_decodes_escapes() {
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:04.000\n\
                   <i>So if n &lt; 30 we use the t distribution, but if n > 30 the z applies.</i>\n";
        let cues = parse(vtt);
        assert_eq!(
            cues[0].text,
            "So if n < 30 we use the t distribution, but if n > 30 the z applies."
        );
        // Real markup still goes, and the ampersand escape decodes once.
        let vtt = "WEBVTT\n\n00:00:00.000 --> 00:00:02.000\n\
                   <c.colorE5E5E5>AT&amp;T</c> and <v Esra>Bayes</v>\n";
        assert_eq!(parse(vtt)[0].text, "AT&T and Bayes");
    }

    /// A speaker who never lands a full stop — routine for auto-captions —
    /// would otherwise take the whole lecture into one paragraph under one
    /// anchor, which M14 then resolves to a single span covering everything.
    #[test]
    fn breaks_a_long_unpunctuated_monologue_and_anchors_each_section() {
        let mut vtt = String::from("WEBVTT\n\n");
        for i in 0..400i64 {
            let s = i * 3_000;
            vtt.push_str(&format!(
                "{}\nEsra Adiyeke: and then we keep going without ever stopping properly\n\n",
                timing(s, s + 3_000)
            ));
        }
        let cues = parse(&vtt);
        let merged = merge(&cues);
        // Bounded despite never meeting a sentence boundary. Before the hard
        // ceiling this was one paragraph of twenty-five thousand characters.
        assert!(merged.len() > 1, "one speaker ran together: {} paragraphs", merged.len());
        for paragraph in &merged {
            assert!(
                paragraph.text.len() < 3 * MAX_PARAGRAPH_CHARS,
                "unbounded paragraph: {} chars",
                paragraph.text.len()
            );
        }

        let md = to_markdown(
            &cues,
            &Meta { title: "Long", date: "2026-08-20", source_name: "long.vtt" },
        );
        // Twenty minutes of speech is four five-minute sections, each anchored.
        for anchor in ["## 00:00", "## 00:05", "## 00:10", "## 00:15"] {
            assert!(md.contains(anchor), "missing {anchor}");
        }
    }

    /// Untimed turns are the only structure an untimed file has; the gap and
    /// length rules cannot see them, so they must not be merged away.
    #[test]
    fn keeps_untimed_turns_apart() {
        let cues = parse("First thing said.\n\nSecond thing said.\n\nThird thing said.");
        assert_eq!(merge(&cues).len(), 3, "{cues:?}");
        // And the same file with CRLF line endings, which has no "\n\n" in it.
        let cues = parse("First thing said.\r\n\r\nSecond thing said.\r\n\r\nThird thing said.");
        assert_eq!(cues.len(), 3, "{cues:?}");
        assert_eq!(cues[1].text, "Second thing said.");
    }

    /// Two tracks concatenated, or a player list handed back unsorted. A
    /// backwards jump makes the gap negative, so the pause rule never fires and
    /// the anchors come out non-monotonic.
    #[test]
    fn orders_cues_before_merging() {
        let vtt = "WEBVTT\n\n\
            00:10:00.000 --> 00:10:02.000\nLater.\n\n\
            00:00:00.000 --> 00:00:02.000\nEarlier.\n";
        let cues = parse(vtt);
        assert_eq!(cues[0].text, "Earlier.", "{cues:?}");

        let md = to_markdown(
            &cues,
            &Meta { title: "T", date: "2026-08-20", source_name: "t.vtt" },
        );
        let first = md.find("## 00:00").expect("first anchor");
        let second = md.find("## 00:10").expect("second anchor");
        assert!(first < second, "anchors run backwards: {md}");
    }

    #[test]
    fn formats_durations_and_anchors() {
        assert_eq!(human_duration(82 * 60 * 1_000), "1h 22m");
        assert_eq!(human_duration(45 * 60 * 1_000), "45m");
        assert_eq!(clock(65 * 60 * 1_000), "01:05");
    }
}
