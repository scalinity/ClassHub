import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";
import { marked } from "marked";

import { derivePhase } from "@/components/MasterGuide";
import { useJobs } from "@/lib/jobs";
import { openInDefaultApp, readClassFile, revealInFinder } from "@/lib/materials";
import { dragWindow } from "@/lib/window";

const headerAction =
  "shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)";

export interface ViewedFile {
  relPath: string;
  name: string;
  kind: string; // scanner kind: html | md | rmd | r | ...
  /** Live document preview: poll the growing file and refresh in place. */
  live?: boolean;
  /** The job composing this file — gates the live view to fresh content. */
  jobId?: number;
}

/**
 * The chunked-writing contract (prompt templates + RESUME_PROMPT) keeps this
 * literal marker in the document from the first Write until the final Edit.
 * Its presence is the proof that the file on disk is the run's own output
 * rather than the previous generation awaiting overwrite.
 */
const CONTINUE_MARKER = "<!-- CONTINUE -->";

const KIND_LABELS: Record<string, string> = {
  html: "HTML",
  md: "MARKDOWN",
  rmd: "R MARKDOWN",
  r: "R SCRIPT",
};

/**
 * In-app reading view for class materials — the same reading room as the
 * guide viewer. Markdown and code render in ClassHub's document register;
 * HTML notebooks run their own embedded scripts but stay sandboxed from the
 * app (no same-origin). Live mode polls the file a synthesis job is writing
 * and refreshes the frame in place, preserving the scroll position.
 */
export function FileViewer({
  classId,
  file,
  onClose,
}: {
  classId: number;
  file: ViewedFile;
  onClose: () => void;
}) {
  const { data, error, isPending } = useQuery({
    queryKey: ["classFile", classId, file.relPath, file.live ?? false],
    queryFn: () => readClassFile(classId, file.relPath),
    refetchInterval: file.live ? 3000 : false,
    retry: false,
  });

  const { jobs, output } = useJobs();
  const liveJob =
    file.jobId === undefined ? undefined : jobs.find((j) => j.id === file.jobId);
  const jobActive =
    liveJob !== undefined &&
    (liveJob.status === "running" || liveJob.status === "queued");
  const phase = derivePhase(output.get(file.jobId ?? -1) ?? []);
  // Until the run's first Write lands, the file on disk is still the previous
  // generation. Fresh content carries CONTINUE_MARKER mid-composition; once
  // the final Edit removes it the run is in VERIFY (index 3) or finished.
  const stale =
    jobActive &&
    phase.index < 3 &&
    data !== undefined &&
    !data.includes(CONTINUE_MARKER);

  const srcDoc =
    data === undefined
      ? undefined
      : file.kind === "html"
        ? data
        : file.kind === "md" || file.kind === "rmd"
          ? docShell(renderMarkdown(data))
          : docShell(`<pre class="sheet">${escapeHtml(data)}</pre>`);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`${file.name} viewer`}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
      className="fixed inset-0 z-40 flex flex-col bg-background animate-in fade-in duration-150 motion-reduce:animate-none"
    >
      <header
        onMouseDown={dragWindow}
        className="flex h-12 shrink-0 items-center gap-2.5 border-b bg-card pl-24 pr-3"
      >
        {file.live && (
          <span
            aria-hidden
            className={`size-1.5 shrink-0 rounded-full bg-(--accent) ${jobActive ? "animate-pulse motion-reduce:animate-none" : ""}`}
          />
        )}
        <p className="pointer-events-none min-w-0 truncate font-mono text-[11px] tracking-[0.18em] text-(--accent)">
          {file.live
            ? `LIVE · ${
                jobActive
                  ? phase.detail
                  : liveJob !== undefined && liveJob.status !== "succeeded"
                    ? "RUN STOPPED"
                    : "COMPLETE"
              }`
            : `MATERIAL · ${KIND_LABELS[file.kind] ?? "FILE"}`}
        </p>
        <span className="pointer-events-none min-w-0 truncate text-[12px] text-muted-foreground">
          {file.name}
        </span>
        <span className="ml-auto" />
        <button
          type="button"
          onClick={() => void openInDefaultApp(classId, file.relPath)}
          className={headerAction}
        >
          OPEN IN DEFAULT APP
        </button>
        <button
          type="button"
          onClick={() => void revealInFinder(classId, file.relPath)}
          className={headerAction}
        >
          SHOW IN FINDER
        </button>
        <button
          type="button"
          autoFocus
          aria-label="Close file viewer"
          onClick={onClose}
          className="shrink-0 cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <div className="min-h-0 flex-1">
        {file.live ? (
          srcDoc === undefined || stale ? (
            <p className="py-16 text-center font-mono text-xs text-muted-foreground">
              WAITING FOR THE FIRST SECTION
              {jobActive ? ` · ${phase.detail}` : "…"}
            </p>
          ) : (
            // allow-same-origin (still no scripts) lets the in-place rewrite
            // preserve the reading position as new sections append below.
            <iframe
              sandbox="allow-same-origin"
              title={file.name}
              ref={(el) => syncLiveFrame(el, srcDoc)}
              className="block h-full w-full border-0"
            />
          )
        ) : error ? (
          <p className="py-16 text-center font-mono text-xs text-destructive">
            COULD NOT LOAD FILE — {String(error)}
          </p>
        ) : isPending || srcDoc === undefined ? (
          <p className="py-16 text-center font-mono text-xs text-muted-foreground">
            LOADING…
          </p>
        ) : (
          // Class HTML notebooks need their own embedded scripts to unpack;
          // allow-scripts without same-origin keeps them isolated from the app.
          <iframe
            sandbox={file.kind === "html" ? "allow-scripts" : ""}
            srcDoc={srcDoc}
            title={file.name}
            className="block h-full w-full border-0"
          />
        )}
      </div>
    </div>
  );
}

/** Rewrites the live frame only when content changed, keeping scroll depth. */
function syncLiveFrame(el: HTMLIFrameElement | null, html: string) {
  if (!el || el.dataset.len === String(html.length)) return;
  const doc = el.contentDocument;
  if (!doc) return;
  const top = doc.scrollingElement?.scrollTop ?? 0;
  doc.open();
  doc.write(html);
  doc.close();
  el.dataset.len = String(html.length);
  if (doc.scrollingElement) doc.scrollingElement.scrollTop = top;
}

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** Front matter renders as a quiet mono block, the body through marked. */
export function renderMarkdown(src: string): string {
  let front = "";
  let body = src;
  if (src.startsWith("---\n")) {
    const end = src.indexOf("\n---\n", 4);
    if (end !== -1) {
      front = `<pre class="frontmatter">${escapeHtml(src.slice(4, end))}</pre>`;
      body = src.slice(end + 5);
    }
  }
  return front + (marked.parse(body, { async: false }) as string);
}

/**
 * ClassHub's document register for raw materials: same paper/ink/type system
 * as the generated guides, but neutral apparatus — raw sources are unbranded;
 * only synthesized documents carry the class accent. The note editor's live
 * preview uses it too, so a note previews exactly as it will read.
 */
export function docShell(body: string): string {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data:">
<!-- The sandbox already blocks scripts; the CSP closes the residual it
     doesn't cover — network loads (remote images, meta refresh) from note
     or material content rendered through this shell. -->

<style>
:root {
  --paper: #fcfcfd; --ink: #1c1f24; --muted: #697079;
  --hairline: #e3e5e9; --shade: rgba(28, 31, 36, 0.05);
}
@media (prefers-color-scheme: dark) {
  :root {
    --paper: #191b1f; --ink: #e6e8eb; --muted: #8f959d;
    --hairline: rgba(255,255,255,0.12); --shade: rgba(255,255,255,0.05);
  }
}
* { box-sizing: border-box; }
body {
  margin: 0; background: var(--paper); color: var(--ink);
  font: 15px/1.6 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
}
main { max-width: 72ch; margin: 0 auto; padding: 3rem 2rem 5rem; overflow-wrap: break-word; }
h1, h2, h3, h4 { font-family: ui-serif, "New York", Georgia, "Times New Roman", serif; line-height: 1.25; font-weight: 600; }
h1 { font-size: 28px; margin: 0 0 0.6em; }
h2 { font-size: 21px; margin: 2em 0 0.5em; padding-top: 1em; border-top: 1px solid var(--hairline); }
h3 { font-size: 17px; margin: 1.6em 0 0.4em; }
pre, code { font-family: ui-monospace, "SF Mono", Menlo, monospace; }
pre {
  font-size: 13px; line-height: 1.55; background: var(--shade);
  border: 1px solid var(--hairline); border-radius: 8px;
  padding: 0.8rem 0.95rem; overflow-x: auto;
}
code { font-size: 13px; }
p code, li code { background: var(--shade); border-radius: 4px; padding: 0.1em 0.35em; font-size: 12.5px; }
pre code { background: none; padding: 0; }
pre.sheet, pre.frontmatter { white-space: pre-wrap; }
pre.frontmatter { font-size: 11px; color: var(--muted); margin-bottom: 2rem; }
blockquote { margin: 1em 0; padding-left: 1em; border-left: 2px solid var(--hairline); color: var(--muted); }
table { border-collapse: collapse; margin: 1em 0; }
th, td { border: 1px solid var(--hairline); padding: 0.45em 0.8em; text-align: left; }
th { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 10px; letter-spacing: 0.12em; text-transform: uppercase; }
img { max-width: 100%; }
hr { border: 0; border-top: 1px solid var(--hairline); margin: 2em 0; }
a { color: inherit; }
@media print { :root { --paper: #fff; --ink: #000; } }
</style>
</head>
<body><main>${body}</main></body>
</html>`;
}
