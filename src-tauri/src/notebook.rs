//! SPEC §7 step 3: a Jupyter notebook flattened to markdown locally, zero
//! tokens. Markdown cells are kept verbatim, code cells are fenced with the
//! kernel's language, and what a cell printed follows it — `stream` and
//! `text/plain` outputs, capped per cell, with an image output replaced by the
//! bracketed figure marker the PDF extracts use. The raw `.ipynb` is JSON that
//! carries every image as base64, which is why chat reads the extract instead.

use anyhow::{Context, Result};
use serde_json::Value;

/// Output lines kept per cell: enough to show what a cell printed, not enough
/// to hold a dataset that scrolled by.
const MAX_OUTPUT_LINES: usize = 40;
/// A byte cap beneath the line cap, for the one-line output that is a whole
/// DataFrame repr.
const MAX_OUTPUT_BYTES: usize = 4 * 1024;

const IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/svg+xml"];

pub fn flatten(json: &str) -> Result<String> {
    let notebook: Value = serde_json::from_str(json).context("parsing notebook JSON")?;
    let cells = notebook
        .get("cells")
        .and_then(Value::as_array)
        .context("no `cells` array — not an nbformat 4 notebook")?;
    let language = kernel_language(&notebook);

    let mut out = String::new();
    for cell in cells {
        let source = text_of(cell.get("source"));
        let source = source.trim_end();
        match cell.get("cell_type").and_then(Value::as_str).unwrap_or("") {
            "markdown" | "raw" => push_block(&mut out, source),
            "code" => {
                if !source.is_empty() {
                    push_block(&mut out, &format!("```{language}\n{source}\n```"));
                }
                let outputs = cell.get("outputs").and_then(Value::as_array);
                for output in outputs.into_iter().flatten() {
                    if let Some(block) = render_output(output) {
                        push_block(&mut out, &block);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// `language_info.name` is what the kernel reports about itself; the
/// kernelspec's `language` is the fallback for a notebook saved without it.
fn kernel_language(notebook: &Value) -> String {
    let metadata = notebook.get("metadata");
    let from = |path: &[&str]| {
        let mut node = metadata?;
        for key in path {
            node = node.get(key)?;
        }
        node.as_str().map(|s| s.trim().to_lowercase())
    };
    from(&["language_info", "name"])
        .or_else(|| from(&["kernelspec", "language"]))
        .unwrap_or_default()
}

/// nbformat stores multi-line text either as one string or as a list of
/// line fragments; both shapes occur in files written by real tools.
fn text_of(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn render_output(output: &Value) -> Option<String> {
    let text = match output.get("output_type").and_then(Value::as_str)? {
        "stream" => text_of(output.get("text")),
        "execute_result" | "display_data" => {
            let data = output.get("data")?;
            // A figure comes with a `text/plain` placeholder (`<Figure size
            // 640x480 with 1 Axes>`), so the image is checked first.
            if IMAGE_TYPES.iter().any(|mime| data.get(mime).is_some()) {
                return Some("[Figure: image output]".to_string());
            }
            text_of(data.get("text/plain"))
        }
        "error" => format!(
            "{}: {}",
            text_of(output.get("ename")),
            text_of(output.get("evalue"))
        ),
        _ => return None,
    };
    let text = cap_output(text.trim_end());
    if text.is_empty() {
        return None;
    }
    Some(format!("```output\n{text}\n```"))
}

fn cap_output(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept = String::new();
    let mut shown = 0;
    for line in &lines {
        if shown == MAX_OUTPUT_LINES {
            break;
        }
        let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
        if line.len() > room {
            let end = (0..=room).rev().find(|&i| line.is_char_boundary(i)).unwrap_or(0);
            kept.push_str(&line[..end]);
            kept.push('…');
            shown += 1;
            break;
        }
        kept.push_str(line);
        kept.push('\n');
        shown += 1;
    }
    let dropped = lines.len() - shown;
    let mut kept = kept.trim_end().to_string();
    if dropped > 0 {
        kept.push_str(&format!("\n… ({dropped} more lines)"));
    }
    kept
}

fn push_block(out: &mut String, block: &str) {
    if block.is_empty() {
        return;
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::{cap_output, flatten, MAX_OUTPUT_LINES};

    const FIXTURE: &str = include_str!("../tests/notebook.ipynb");

    #[test]
    fn keeps_markdown_verbatim_and_fences_code_with_the_kernel_language() {
        let md = flatten(FIXTURE).expect("flatten");
        assert!(md.contains("# Week 4 — pandas warm-up\n\nLoad the cohort"), "{md}");
        assert!(
            md.contains("```python\nimport pandas as pd\ndf = pd.read_csv('cohort.csv')"),
            "{md}"
        );
        // Source as one string and as a list of fragments both flatten.
        assert!(md.contains("```python\ndf['age'].describe()\n```"), "{md}");
        assert!(md.contains("raw cell text"), "{md}");
    }

    #[test]
    fn keeps_stream_and_text_outputs_and_drops_the_html_twin() {
        let md = flatten(FIXTURE).expect("flatten");
        assert!(md.contains("```output\nrows: 120\ncolumns: 7\n```"), "{md}");
        assert!(md.contains("mean      54.216667"), "{md}");
        assert!(!md.contains("dataframe"), "text/html leaked: {md}");
    }

    #[test]
    fn replaces_an_image_output_with_the_figure_marker() {
        let md = flatten(FIXTURE).expect("flatten");
        assert!(md.contains("[Figure: image output]"), "{md}");
        assert!(!md.contains("iVBORw0KGgo"), "base64 leaked: {md}");
        assert!(!md.contains("<Figure size"), "the placeholder stood in for the marker: {md}");
    }

    #[test]
    fn reports_an_error_output_by_name_and_message_without_the_traceback() {
        let md = flatten(FIXTURE).expect("flatten");
        assert!(md.contains("ZeroDivisionError: division by zero"), "{md}");
        assert!(!md.contains("Traceback"), "{md}");
    }

    #[test]
    fn an_unrun_cell_has_code_and_no_output_block() {
        let md = flatten(FIXTURE).expect("flatten");
        assert!(md.contains("```python\n# not run yet\n```"), "{md}");
        // The stream, the describe() result and the error; the figure is a
        // marker line rather than an output block.
        assert_eq!(md.matches("```output").count(), 3, "{md}");
    }

    #[test]
    fn refuses_json_that_is_not_a_notebook() {
        assert!(flatten("{\"worksheets\": []}").is_err());
        assert!(flatten("not json").is_err());
    }

    #[test]
    fn falls_back_to_the_kernelspec_language_and_then_to_a_bare_fence() {
        let spec_only = r#"{"cells":[{"cell_type":"code","source":"x <- 1","outputs":[]}],
            "metadata":{"kernelspec":{"language":"R"}}}"#;
        assert!(flatten(spec_only).unwrap().contains("```r\nx <- 1\n```"));
        let bare = r#"{"cells":[{"cell_type":"code","source":"x","outputs":[]}]}"#;
        assert!(flatten(bare).unwrap().contains("```\nx\n```"));
    }

    /// The cap is what keeps a cell that printed a dataset from becoming the
    /// dataset: search needs what a cell showed, not every row it scrolled.
    #[test]
    fn caps_a_long_output_by_lines_and_names_the_remainder() {
        let text: String = (1..=100).map(|i| format!("row {i}\n")).collect();
        let capped = cap_output(text.trim_end());
        assert_eq!(capped.lines().count(), MAX_OUTPUT_LINES + 1);
        assert!(capped.starts_with("row 1\n"), "{capped}");
        assert!(capped.contains(&format!("row {MAX_OUTPUT_LINES}\n")), "{capped}");
        assert!(capped.ends_with("… (60 more lines)"), "{capped}");
    }

    #[test]
    fn caps_a_single_huge_line_at_a_char_boundary() {
        let text = "é".repeat(5_000);
        let capped = cap_output(&text);
        assert!(capped.len() < 4_200, "{}", capped.len());
        assert!(capped.ends_with('…'), "{capped}");
    }
}
