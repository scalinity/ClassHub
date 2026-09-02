# M20 — Every format in the tree

**Read `SPEC.md` in full first**, then this file. §5 (`files.kind`), §7 (the extraction
pipeline) and §12 (the viewer) are what this milestone extends.

## Why

`scanner::kind_for` knows pptx, pdf, rmd, r, html, md and caption tracks; everything else is
`other`, and `extract::route` skips it. A `.docx` from Canvas has sat in Biostatistics since
Sep 1 with no extract, invisible to search. Design Studio's Week 4 (Sep 16) is Python, Jupyter,
pandas and GitHub, and Applied Generative AI is a deep-learning course: notebooks, scripts and
CSVs are about to arrive, and none of them would reach chat or a guide. Separately, a PDF opens
in Preview: the in-app viewer stops at text formats, so reading a deck beside the chat means
leaving the app.

## Phase 1 — Kinds

`kind_for` gains `docx`, `ipynb`, `py`, `csv`. SPEC §5's `kind` comment, `FileTree`'s icon map
and `VIEWABLE_KINDS` follow.

## Phase 2 — Zero-token extracts

- **docx** → `soffice --headless --convert-to html` into the extracts mirror, then the existing
  HTML stripper (the `Route::Html` code path) → `<name>.docx.md`. Same bounded subprocess, same
  profile dir, same sidecar rule as pptx. Figures are lost; for a docx that is acceptable, and
  SPEC §7 says so.
- **ipynb** → a local pass: markdown cells verbatim; code cells fenced with the kernel's
  language; `stream` and `text/plain` outputs included, capped per cell; image outputs replaced
  by `[Figure: image output]`. A pure function, with `cargo test` on a small fixture covering
  all four cell shapes.
- **py, csv** → the text route. CSV capped at a few hundred lines with a note of the total,
  since an extract exists to be searched, not to hold a dataset.

`.txt` keeps its caption route (the note in `extract.rs::route`).

## Phase 3 — PDFs in the app

Enable Tauri's asset protocol scoped to the AIBHS root; `frame-src` in both CSPs gains
`asset: http://asset.localhost` (img-src already carries it). `FileViewer` renders a `pdf` as
an iframe on `convertFileSrc(absolute path)` — WebKit's own PDF viewer, no library. A `pptx`
opens its converted twin from the extracts mirror when the sidecar matches, and falls back to
the default app when it does not. Scripts and CSVs open in the text viewer; a notebook opens its
extract.

## Acceptance

- The Biostatistics `.docx` has an extract after a rescan, and chat finds a phrase from it.
- A fixture notebook extracts to markdown with code fences and outputs; a script and a CSV
  extract locally.
- A slide PDF opens inline; the pptx beside it opens as its PDF twin; a PDF with a remote image
  loads nothing from the network.
- No extract job was spawned for any of the above: zero tokens.
- `cargo test` covers the notebook flattener and the CSV cap; `npx tsc --noEmit` passes.

## Watch for

- **The asset scope is the AIBHS root and nothing else.** Not home, not `/`.
- **soffice names docx output `<stem>.html`**, as it named pptx output after the stem; rename
  to `<name>.docx.html` before stripping, and keep the sidecar so an unchanged file is never
  converted twice.
- **Guides read extracts by path.** New kinds join `current_manifest` automatically because
  they are files; check that a unit or folder guide's source list names them.
