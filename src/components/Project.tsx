import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";

import { SectionHeading } from "@/components/SectionHeading";
import {
  deltaLabel,
  deltaTitle,
  formatGeneratedAt,
  projectStatus,
  setProjectMaterial,
  writeWorkbook,
  type GuideInfo,
  type ProjectStatus,
} from "@/lib/guides";
import type { TreeNode } from "@/lib/materials";
import { dueDayLabel, isOverdue } from "@/lib/schedule";
import {
  buttonChip,
  buttonIcon,
  buttonText,
  buttonTextMuted,
  chipMuted,
  errorLine,
  meta,
  pulseDot,
  readingText,
  row,
  statusLine,
} from "@/lib/styles";

/** The one query the section and the workspace's nav share. */
export function projectQuery(classId: number) {
  return {
    queryKey: ["project", classId] as const,
    queryFn: () => projectStatus(classId),
    placeholderData: (prev: ProjectStatus | null | undefined) => prev,
  };
}

function collectFiles(nodes: readonly TreeNode[], out: TreeNode[] = []): TreeNode[] {
  for (const node of nodes) {
    if (node.dir) collectFiles(node.children, out);
    else out.push(node);
  }
  return out;
}

/** One item of the material picker: a file path laid out as a menu row. */
const pickerItem =
  "block w-full cursor-pointer truncate rounded-sm px-2 py-1 text-left font-mono text-code text-muted-foreground transition-colors hover:bg-(--accent)/10 hover:text-(--accent-ink) focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60";

/**
 * SPEC §8.6 / §12 — the project: present while the class has a project
 * item. The next item and its date, the workbook's state — `Write the
 * workbook`, `Writing the workbook…`, `Refresh · 1 file changed`, `Read the
 * workbook` — and the `Project material` pick, a class file the workbook
 * reads as the guidelines beside every file named for the project and the
 * drafts under `Project/`.
 */
export function ProjectSection({
  classId,
  guide,
  active,
  tree,
  onView,
}: {
  classId: number;
  /** The workbook's row, once written. */
  guide: GuideInfo | undefined;
  /** A workbook job is queued or running. */
  active: boolean;
  tree: readonly TreeNode[] | undefined;
  onView: () => void;
}) {
  const { data: status } = useQuery(projectQuery(classId));
  const [error, setError] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [picking, setPicking] = useState(false);
  if (status === undefined || status === null) return null;

  const write = () => {
    setError(null);
    writeWorkbook(classId).catch((e) => setError(`The workbook didn't start: ${String(e)}`));
  };
  const pick = (relPath: string | null) => {
    setPicking(true);
    setError(null);
    setProjectMaterial(classId, relPath)
      .then(() => {
        setPicking(false);
        setPickerOpen(false);
      })
      .catch((e) => {
        setError(String(e));
        setPicking(false);
      });
  };

  const next = status.next;
  const count = `${status.items.length} ${status.items.length === 1 ? "item" : "items"}${
    status.weight > 0 ? ` · ${status.weight}% of the grade` : ""
  }`;

  return (
    <section id="project" className="mt-14 scroll-mt-20" aria-label="Project">
      <SectionHeading
        title="Project"
        count={count}
        actions={
          <>
            <button
              type="button"
              onClick={() => setPickerOpen((prev) => !prev)}
              aria-expanded={pickerOpen}
              className={`${buttonTextMuted} ${pickerOpen ? "bg-muted text-foreground" : ""}`}
            >
              Project material
            </button>
            {active ? (
              <span className={`px-2 ${statusLine}`}>
                <span aria-hidden className={pulseDot} />
                Writing the workbook…
              </span>
            ) : !guide ? (
              <button type="button" onClick={write} className={buttonText}>
                Write the workbook
              </button>
            ) : (
              <>
                {guide.stale ? (
                  <button
                    type="button"
                    title={deltaTitle(guide.diff)}
                    onClick={write}
                    className={`${buttonChip} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
                  >
                    Refresh · {deltaLabel(guide.diff)}
                  </button>
                ) : (
                  <button
                    type="button"
                    title="Rewrite the workbook"
                    aria-label="Rewrite the project workbook"
                    onClick={write}
                    className={buttonIcon}
                  >
                    <RefreshCw size={12} aria-hidden />
                  </button>
                )}
                <button type="button" onClick={onView} className={buttonText}>
                  Read the workbook
                </button>
              </>
            )}
          </>
        }
      >
        {error && <p className={errorLine}>{error}</p>}
      </SectionHeading>

      {pickerOpen && (
        <div className="mt-3 max-h-52 overflow-y-auto rounded-md p-1 ring-1 ring-border">
          <p className="px-2 py-1.5 text-body text-muted-foreground">
            Pick the file the workbook reads as the guidelines — beside every file named
            for the project and the drafts under Project/.
          </p>
          {status.material !== null && (
            <button
              type="button"
              onClick={() => pick(null)}
              disabled={picking}
              className="block w-full cursor-pointer rounded-sm px-2 py-1 text-left text-body font-medium text-(--accent-ink) transition-colors hover:bg-(--accent)/10 focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none disabled:opacity-60"
            >
              None — the named files alone
            </button>
          )}
          {tree === undefined ? (
            <p className="px-2 py-1.5 text-body text-muted-foreground">Files are still loading…</p>
          ) : (
            collectFiles(tree).map((file) => (
              <button
                key={file.relPath}
                type="button"
                onClick={() => pick(file.relPath)}
                disabled={picking}
                className={pickerItem}
              >
                {file.relPath}
              </button>
            ))
          )}
        </div>
      )}

      <div className="mt-3">
        {next ? (
          <div className={row}>
            <span className={`shrink-0 ${meta}`}>Next</span>
            <span className="min-w-0 flex-1 truncate text-title" title={next.notes ?? undefined}>
              {next.title}
            </span>
            {next.kind !== "other" && <span className={chipMuted}>{next.kind}</span>}
            {next.fromCanvas && (
              <span className="shrink-0 text-fine text-muted-foreground">from Canvas</span>
            )}
            <span
              className={
                "shrink-0 text-meta font-medium tabular-nums " +
                (isOverdue(next.dueAt) ? "text-destructive" : "text-(--accent-ink)")
              }
            >
              {dueDayLabel(next.dueAt)}
            </span>
          </div>
        ) : (
          <p className={`py-2 ${readingText} text-muted-foreground`}>
            Every item on the list is done or past.
          </p>
        )}
        <p className={`mt-2 ${meta}`}>
          {status.material !== null
            ? `Guidelines · ${status.material}`
            : "Guidelines · every file named for the project, and the drafts under Project/"}
          {guide && ` · workbook written ${formatGeneratedAt(guide.generatedAt)}`}
        </p>
      </div>
    </section>
  );
}
