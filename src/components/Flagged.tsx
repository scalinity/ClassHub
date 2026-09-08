import { useQuery } from "@tanstack/react-query";
import { Flag } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import { hintKindLabel, listHints, type HintInfo } from "@/lib/hints";
import { chipAccent, chipMuted, meta, readingText } from "@/lib/styles";

/** The one query the section and the workspace's nav share — TanStack dedupes
 *  it, so the nav's link costs no second request. */
export function hintsQuery(classId: number) {
  return {
    queryKey: ["hints", classId] as const,
    queryFn: () => listHints(classId),
    placeholderData: (prev: HintInfo[] | undefined) => prev,
  };
}

/** `2026-09-03` → `Sep 3`, parsed by parts so the day never slips a zone. */
function sessionDay(iso: string): string {
  const [year, month, day] = iso.split("-").map(Number);
  if (!year || !month || !day) return iso;
  return new Date(year, month - 1, day).toLocaleDateString("en-US", {
    month: "short",
    day: "numeric",
  });
}

/**
 * SPEC §8.4 — what the professor flagged, across the class's distilled
 * sessions: emphasis, exam hints, corrections, where the room got stuck, what
 * was assigned, and what each session built on. One list, newest session
 * first, because two days before a quiz the question is "what did they say
 * mattered", and nothing else answers it. Each item's anchor opens the
 * transcript at the stretch it was said in. Absent until a session has been
 * distilled for it.
 */
export function FlaggedSection({
  classId,
  onOpenTranscript,
}: {
  classId: number;
  /** Open the transcript in the material viewer at an `HH:MM` heading. */
  onOpenTranscript: (relPath: string, anchor: string) => void;
}) {
  const { data: hints } = useQuery(hintsQuery(classId));
  if (hints === undefined || hints.length === 0) return null;

  // Grouped by session, in the order the backend already sorted them.
  const sessions: { relPath: string; date: string; title: string; unitName: string; items: HintInfo[] }[] = [];
  for (const hint of hints) {
    const last = sessions[sessions.length - 1];
    if (last && last.relPath === hint.relPath) last.items.push(hint);
    else
      sessions.push({
        relPath: hint.relPath,
        date: hint.date,
        title: hint.title,
        unitName: hint.unitName,
        items: [hint],
      });
  }
  const sessionCount = sessions.length;

  return (
    <section id="flagged" className="mt-14 scroll-mt-20" aria-label="Flagged">
      <SectionHeading
        title="Flagged"
        count={`${hints.length} ${hints.length === 1 ? "item" : "items"} from ${sessionCount} ${
          sessionCount === 1 ? "session" : "sessions"
        }`}
      />
      <p className={`mt-1 max-w-xl ${readingText} text-muted-foreground`}>
        What was said in the room that a slide never carried — the professor's
        own words about what matters, newest session first.
      </p>
      <div className="mt-3">
        {sessions.map((session) => (
          <div key={session.relPath} className="border-b border-border/70 py-3 last:border-b-0">
            <div className="flex flex-wrap items-baseline gap-x-2.5 gap-y-0.5">
              <span className="text-title">
                {session.title !== "" ? session.title : session.unitName}
              </span>
              <span className={meta}>
                {session.date !== "" ? sessionDay(session.date) : ""}
                {session.title !== "" ? ` · ${session.unitName}` : ""}
              </span>
            </div>
            <ul className="mt-2 space-y-2">
              {session.items.map((item) => (
                <li key={item.id} className="flex items-start gap-2.5">
                  <Flag
                    size={12}
                    aria-hidden
                    className="mt-1 shrink-0 text-muted-foreground/60"
                  />
                  <span className="min-w-0 flex-1">
                    <span
                      className={`${item.kind === "exam_hint" || item.kind === "correction" ? chipAccent : chipMuted} mr-2 align-[1px]`}
                    >
                      {hintKindLabel(item.kind)}
                    </span>
                    <span className="text-body">{item.text}</span>
                  </span>
                  {item.anchor !== null ? (
                    <button
                      type="button"
                      title="Open the transcript at this moment"
                      onClick={() => onOpenTranscript(session.relPath, item.anchor ?? "")}
                      className={`shrink-0 cursor-pointer rounded-sm font-mono text-fine tabular-nums text-(--accent-ink) hover:underline focus-visible:outline-2 focus-visible:outline-(--accent)`}
                    >
                      {item.anchor}
                    </button>
                  ) : (
                    <span className="shrink-0 font-mono text-fine text-muted-foreground/50">
                      untimed
                    </span>
                  )}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
    </section>
  );
}
