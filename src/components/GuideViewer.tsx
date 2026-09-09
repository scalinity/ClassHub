import { useCallback, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { X } from "lucide-react";

import { withDocumentCsp } from "@/lib/document";
import {
  deltaLabel,
  deltaTitle,
  FAMILY_LABELS,
  formatGeneratedAt,
  readGuide,
  type GuideInfo,
} from "@/lib/guides";
import { openInDefaultApp, revealInFinder } from "@/lib/materials";
import { setExamFrame, type ExamScore } from "@/lib/practice";
import { dragWindow } from "@/lib/window";
import { buttonIcon, buttonTextMuted, chipAmber, meta } from "@/lib/styles";

/**
 * SPEC §12: sandboxed in-app guide viewer. allow-scripts (without
 * allow-same-origin) runs the guide's inline interactive devices in an
 * opaque origin, isolated from the app: no IPC, no storage, no navigation.
 * The guide contract additionally forbids network and storage APIs.
 *
 * A practice exam's frame is the one the self-score listener matches
 * (SPEC §8.3): registered through the frame's ref while it is on screen,
 * for the exam this viewer opened, and the score it records is said in the
 * header.
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
  const [scored, setScored] = useState<ExamScore | null>(null);
  const [refused, setRefused] = useState<string | null>(null);
  const exam = guide.family === "practice";
  // The frame the self-score listener matches the panel's message against,
  // registered while the frame is on screen and cleared when it leaves. A
  // stable callback, so a render — the score landing — does not clear and
  // re-register it.
  const scope = guide.scope;
  const registerFrame = useCallback(
    (el: HTMLIFrameElement) => {
      setExamFrame({
        frame: el,
        classId,
        scope,
        onScored: (score) => {
          setRefused(null);
          setScored(score);
        },
        onRefused: setRefused,
      });
      return () => setExamFrame(null);
    },
    [classId, scope],
  );

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
        <p className={`pointer-events-none shrink-0 ${meta}`}>{FAMILY_LABELS[guide.family]}</p>
        <p className="pointer-events-none min-w-0 truncate text-[15px] font-medium">
          {guide.label}
        </p>
        {guide.stale && (
          <span title={deltaTitle(guide.diff)} className={chipAmber}>
            stale · {deltaLabel(guide.diff)}
          </span>
        )}
        {scored !== null && (
          <span
            title={
              scored.weakTopics.length > 0
                ? `Missed: ${scored.weakTopics.join(", ")} — the next exam of this scope focuses on them`
                : "Full marks — nothing for the next exam to revisit"
            }
            className={`pointer-events-none shrink-0 ${meta} text-(--accent-ink)`}
          >
            Scored {scored.correct} of {scored.total} · recorded
          </span>
        )}
        {refused !== null && (
          <span className="pointer-events-none min-w-0 truncate text-fine text-destructive">
            Score not recorded: {refused}
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
            ref={exam ? registerFrame : undefined}
            className="block h-full w-full border-0"
          />
        )}
      </div>
    </div>
  );
}
