import { useQuery } from "@tanstack/react-query";
import { convertFileSrc } from "@tauri-apps/api/core";
import { X } from "lucide-react";

import { escapeHtml } from "@/lib/answer";
import { docShell, renderMarkdown, withDocumentCsp } from "@/lib/document";
import { anchorId } from "@/lib/hints";
import { derivePhase, useJobs } from "@/lib/jobs";
import { openInDefaultApp, readClassFile, revealInFinder } from "@/lib/materials";
import { dragWindow } from "@/lib/window";
import { buttonIcon, buttonTextMuted, meta } from "@/lib/styles";

export interface ViewedFile {
  relPath: string;
  name: string;
  kind: string; // scanner kind: html | md | rmd | r | py | csv | ipynb | pdf | pptx | ...
  /** Live document preview: poll the growing file and refresh in place. */
  live?: boolean;
  /** The job composing this file — gates the live view to fresh content. */
  jobId?: number;
  /**
   * Absolute path of a PDF to frame through the asset protocol — the file
   * itself, or a deck's converted twin. Nothing is read as text when set.
   */
  pdfPath?: string;
  /** Class-relative path read in place of `relPath`: a notebook's extract. */
  source?: string;
  /**
   * An `HH:MM` heading of a transcript to open at (SPEC §8.4): the document
   * register gives those headings ids, and the frame scrolls there once it
   * has loaded. A heading the transcript lacks opens it at the top.
   */
  anchor?: string;
}

/** Scrolls a loaded markdown frame to the anchor's heading, when it has one. */
function scrollToAnchor(el: HTMLIFrameElement, anchor: string | undefined) {
  if (anchor === undefined) return;
  const doc = el.contentDocument;
  const heading = doc?.getElementById(anchorId(anchor));
  heading?.scrollIntoView({ block: "start" });
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
  md: "Markdown",
  rmd: "R Markdown",
  r: "R script",
  py: "Python",
  csv: "CSV",
  ipynb: "Notebook extract",
  pdf: "PDF",
  pptx: "Slides as PDF",
};

/**
 * In-app reading view for class materials — the same reading room as the
 * guide viewer. Markdown and code render in ClassHub's document register;
 * HTML notebooks run their own embedded scripts but stay sandboxed from the
 * app (no same-origin). A PDF, or a deck's converted twin, is WebKit's own
 * PDF view on an `asset:` URL: a different origin by scheme, admitted by the
 * window CSP's `frame-src` and reaching only the AIBHS root (SPEC §13). Live
 * mode polls the file a synthesis job is writing and refreshes the frame in
 * place, preserving the scroll position.
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
  const textPath = file.source ?? file.relPath;
  const { data, error, isPending } = useQuery({
    queryKey: ["classFile", classId, textPath, file.live ?? false],
    queryFn: () => readClassFile(classId, textPath),
    enabled: file.pdfPath === undefined,
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
  // the final Edit removes it the run is in Verify (index 3) or finished.
  const stale =
    jobActive &&
    phase.index < 3 &&
    data !== undefined &&
    !data.includes(CONTINUE_MARKER);

  // The document and the sandbox it runs under are chosen together, because
  // each is only safe with the other: a class HTML notebook keeps its scripts
  // (allow-scripts, in an opaque origin, with the CSP injected into it closing
  // every egress), while a document the register renders — untrusted
  // transcript or note text through marked — runs no script at all (the
  // shell's CSP admits none and the sandbox grants none), so same-origin costs
  // nothing and lets the viewer scroll it to a flagged item's heading.
  const framed =
    data === undefined
      ? undefined
      : file.kind === "html"
        ? { srcDoc: withDocumentCsp(data), sandbox: "allow-scripts" }
        : file.kind === "md" || file.kind === "rmd" || file.kind === "ipynb"
          ? { srcDoc: docShell(renderMarkdown(data)), sandbox: "allow-same-origin" }
          : {
              srcDoc: docShell(`<pre class="sheet">${escapeHtml(data)}</pre>`),
              sandbox: "allow-same-origin",
            };
  const srcDoc = framed?.srcDoc;

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
        className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border/70 bg-surface pl-24 pr-3"
      >
        {file.live && (
          <span
            aria-hidden
            className={`size-1.5 shrink-0 rounded-full bg-(--accent) ${jobActive ? "animate-pulse motion-reduce:animate-none" : ""}`}
          />
        )}
        <p className={`pointer-events-none min-w-0 truncate ${meta}`}>
          {file.live
            ? `Live · ${
                jobActive
                  ? phase.detail
                  : liveJob !== undefined && liveJob.status !== "succeeded"
                    ? "run stopped"
                    : "complete"
              }`
            : `Material · ${KIND_LABELS[file.kind] ?? "File"}`}
        </p>
        <span className="pointer-events-none min-w-0 truncate text-[15px] font-medium">
          {file.name}
        </span>
        <span className="ml-auto" />
        <button
          type="button"
          onClick={() => void openInDefaultApp(classId, file.relPath)}
          className={buttonTextMuted}
        >
          Open in default app
        </button>
        <button
          type="button"
          onClick={() => void revealInFinder(classId, file.relPath)}
          className={buttonTextMuted}
        >
          Show in Finder
        </button>
        <button
          type="button"
          autoFocus
          aria-label="Close file viewer"
          onClick={onClose}
          className={buttonIcon}
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <div className="min-h-0 flex-1">
        {file.pdfPath !== undefined ? (
          // No sandbox attribute: a sandboxed frame has no plugins, and
          // WebKit's PDF view is one. The asset scheme keeps it cross-origin.
          <iframe
            src={convertFileSrc(file.pdfPath)}
            title={file.name}
            className="block h-full w-full border-0"
          />
        ) : file.live ? (
          srcDoc === undefined || stale ? (
            <p className="py-16 text-center text-body text-muted-foreground">
              Waiting for the first section
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
          <p className="py-16 text-center text-body text-destructive">
            Couldn't load the file: {String(error)}
          </p>
        ) : isPending || framed === undefined ? (
          <p className="py-16 text-center text-body text-muted-foreground">
            Loading…
          </p>
        ) : (
          <iframe
            sandbox={framed.sandbox}
            srcDoc={framed.srcDoc}
            title={file.name}
            onLoad={(e) => scrollToAnchor(e.currentTarget, file.anchor)}
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
