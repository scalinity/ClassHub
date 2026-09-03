import { useQuery } from "@tanstack/react-query";
import { convertFileSrc } from "@tauri-apps/api/core";
import { X } from "lucide-react";

import { escapeHtml } from "@/lib/answer";
import { docShell, renderMarkdown, withDocumentCsp } from "@/lib/document";
import { derivePhase, useJobs } from "@/lib/jobs";
import { openInDefaultApp, readClassFile, revealInFinder } from "@/lib/materials";
import { dragWindow } from "@/lib/window";
import { headerAction } from "@/lib/styles";

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
  py: "PYTHON",
  csv: "CSV",
  ipynb: "NOTEBOOK EXTRACT",
  pdf: "PDF",
  pptx: "SLIDES AS PDF",
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
        ? withDocumentCsp(data)
        : file.kind === "md" || file.kind === "rmd" || file.kind === "ipynb"
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

