import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import { withDocumentCsp } from "@/lib/document";
import {
  deltaLabel,
  deltaTitle,
  formatGeneratedAt,
  readGuide,
  type GuideInfo,
} from "@/lib/guides";
import { openInDefaultApp, revealInFinder } from "@/lib/materials";
import { dragWindow } from "@/lib/window";
import { buttonIcon, buttonTextMuted, chipAmber, meta } from "@/lib/styles";

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
        className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border/70 bg-surface pl-24 pr-3"
      >
        <p className={`pointer-events-none shrink-0 ${meta}`}>Study guide</p>
        <p className="pointer-events-none min-w-0 truncate text-[15px] font-medium">
          {guide.label}
        </p>
        {guide.stale && (
          <span title={deltaTitle(guide.diff)} className={chipAmber}>
            stale · {deltaLabel(guide.diff)}
          </span>
        )}
        <span className={`ml-auto shrink-0 ${meta}`}>
          written {formatGeneratedAt(guide.generatedAt)}
        </span>
        <button
          type="button"
          onClick={() => void openInDefaultApp(classId, guide.relPath)}
          className={buttonTextMuted}
        >
          Open in browser
        </button>
        <button
          type="button"
          onClick={() => void revealInFinder(classId, guide.relPath)}
          className={buttonTextMuted}
        >
          Show in Finder
        </button>
        <button
          type="button"
          autoFocus
          aria-label="Close guide viewer"
          onClick={onClose}
          className={buttonIcon}
        >
          <X size={14} aria-hidden />
        </button>
      </header>

      <div className="min-h-0 flex-1">
        {error ? (
          <p className="py-16 text-center text-body text-destructive">
            Couldn't load the guide: {String(error)}
          </p>
        ) : isPending ? (
          <p className="py-16 text-center text-body text-muted-foreground">
            Loading the guide…
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
