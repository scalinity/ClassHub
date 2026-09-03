import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import { withDocumentCsp } from "@/lib/document";
import {
  formatGeneratedAt,
  readGuide,
  type GuideInfo,
} from "@/lib/guides";
import { openInDefaultApp, revealInFinder } from "@/lib/materials";
import { dragWindow } from "@/lib/window";
import { headerAction } from "@/lib/styles";

/**
 * SPEC §12: sandboxed in-app guide viewer. allow-scripts (without
 * allow-same-origin) runs the guide's inline interactive devices in an
 * opaque origin, isolated from the app: no IPC, no storage, no navigation.
 * The guide contract additionally forbids network and storage APIs.
 */
export function GuideViewer({
  classId,
  guide,
  onClose,
}: {
  classId: number;
  guide: GuideInfo;
  onClose: () => void;
}) {
  const {
    data: html,
    error,
    isPending,
  } = useQuery({
    queryKey: ["guideHtml", classId, guide.scope, guide.generatedAt],
    queryFn: () => readGuide(classId, guide.scope),
  });

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={`${guide.label} study guide`}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
      className="fixed inset-0 z-40 flex flex-col bg-background animate-in fade-in duration-150 motion-reduce:animate-none"
    >
      {/* pl clears the macOS traffic lights (overlay title bar). */}
      <header
        onMouseDown={dragWindow}
        className="flex h-12 shrink-0 items-center gap-2.5 border-b bg-card pl-24 pr-3"
      >
        <p className="pointer-events-none min-w-0 truncate font-mono text-[11px] tracking-[0.18em] text-(--accent)">
          STUDY GUIDE ·{" "}
          {guide.label.toUpperCase()}
        </p>
        {guide.stale && (
          <span
            title="Sources changed since this guide was generated"
            className="shrink-0 rounded bg-class-amber/12 px-1.5 py-0.5 font-mono text-[10px] tracking-[0.14em] text-class-amber"
          >
            STALE
          </span>
        )}
        <span className="ml-auto shrink-0 font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
          GENERATED {formatGeneratedAt(guide.generatedAt)}
        </span>
        <button
          type="button"
          onClick={() => void openInDefaultApp(classId, guide.relPath)}
          className={headerAction}
        >
          OPEN IN BROWSER
        </button>
        <button
          type="button"
          onClick={() => void revealInFinder(classId, guide.relPath)}
          className={headerAction}
        >
          SHOW IN FINDER
        </button>
        <button
          type="button"
          autoFocus
          aria-label="Close guide viewer"
          onClick={onClose}
          className="shrink-0 cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <div className="min-h-0 flex-1">
        {error ? (
          <p className="py-16 text-center font-mono text-xs text-destructive">
            COULD NOT LOAD GUIDE — {String(error)}
          </p>
        ) : isPending ? (
          <p className="py-16 text-center font-mono text-xs text-muted-foreground">
            LOADING GUIDE…
          </p>
        ) : (
          <iframe
            sandbox="allow-scripts"
            srcDoc={withDocumentCsp(html)}
            title={`${guide.label} study guide`}
            className="block h-full w-full border-0"
          />
        )}
      </div>
    </div>
  );
}
