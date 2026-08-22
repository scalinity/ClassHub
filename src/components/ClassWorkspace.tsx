import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ChevronLeft,
  FileQuestion,
  FolderOpen,
  NotepadText,
  RefreshCw,
  type LucideIcon,
} from "lucide-react";

import { FileTree } from "@/components/FileTree";
import { FileViewer, type ViewedFile } from "@/components/FileViewer";
import { GuideViewer } from "@/components/GuideViewer";
import { MasterGuideStrip } from "@/components/MasterGuide";
import type { ClassInfo } from "@/lib/classes";
import {
  formatGeneratedAt,
  listGuides,
  MASTER_OUTPUT_PATH,
  synthesizeModule,
} from "@/lib/guides";
import { useJobs } from "@/lib/jobs";
import {
  listNotes,
  listPractice,
  openInDefaultApp,
  scanClass,
  type ManagedFile,
} from "@/lib/materials";
import { formatTimeRange, weekdayLabel } from "@/lib/schedule";

const ACCENTS: Record<string, string> = {
  blue: "var(--class-blue)",
  orange: "var(--class-orange)",
  green: "var(--class-green)",
  amber: "var(--class-amber)",
};

export function ClassWorkspace({
  info,
  onBack,
}: {
  info: ClassInfo;
  onBack: () => void;
}) {
  const {
    data: tree,
    error,
    refetch,
    isPending,
    isFetching,
    dataUpdatedAt,
  } = useQuery({
    queryKey: ["classTree", info.id],
    queryFn: () => scanClass(info.id),
  });

  // Guide state (M5/M6). Staleness is computed on demand backend-side; the
  // query re-runs when a scan lands (dataUpdatedAt) or a guide job settles
  // (settledGuideJobs) — the guides upsert commits before the status flips,
  // so a refetch triggered by the transition always sees the new row.
  const { jobs } = useJobs();
  const guideJobs = jobs.filter(
    (j) =>
      (j.kind === "module_guide" || j.kind === "master_guide") &&
      j.classId === info.id,
  );
  const activeScopes = new Set(
    guideJobs
      .filter((j) => j.status === "running" || j.status === "queued")
      .map((j) => j.scope ?? ""),
  );
  const settledGuideJobs = guideJobs.length - activeScopes.size;
  const { data: guides } = useQuery({
    queryKey: ["guides", info.id, settledGuideJobs, dataUpdatedAt],
    queryFn: () => listGuides(info.id),
    placeholderData: (prev) => prev,
  });
  const guideMap = new Map((guides ?? []).map((g) => [g.scope, g]));

  // Practice exams and notes (M8): both chat-written, both listed from disk.
  // The practice query re-runs when a practice job settles (finalize verifies
  // the file before the status flips, same ordering as guides); the notes
  // query is invalidated by the hub-changed push (lib/query.ts).
  const practiceJobs = jobs.filter(
    (j) => j.kind === "practice" && j.classId === info.id,
  );
  const activePractice = practiceJobs.filter(
    (j) => j.status === "running" || j.status === "queued",
  );
  const { data: practice } = useQuery({
    queryKey: ["practice", info.id, practiceJobs.length - activePractice.length],
    queryFn: () => listPractice(info.id),
    placeholderData: (prev) => prev,
  });
  const { data: notes } = useQuery({
    queryKey: ["notes", info.id],
    queryFn: () => listNotes(info.id),
  });

  const [viewScope, setViewScope] = useState<string | null>(null);
  const [viewFile, setViewFile] = useState<ViewedFile | null>(null);
  const [synthError, setSynthError] = useState<string | null>(null);
  const handleSynthesize = (scope: string) => {
    setSynthError(null);
    synthesizeModule(info.id, scope).catch((e) => setSynthError(String(e)));
  };
  const viewedGuide = viewScope ? (guideMap.get(viewScope) ?? null) : null;

  const style = {
    "--accent": ACCENTS[info.color] ?? "var(--class-blue)",
  } as CSSProperties;

  const meetingLine = [
    ...info.meetings.map(
      (m) =>
        `${weekdayLabel(m.weekday)} ${formatTimeRange(m.startTime, m.endTime)}`,
    ),
    info.room,
    `${info.credits} CR`,
  ].join(" · ");

  const scannedLabel =
    dataUpdatedAt > 0
      ? new Date(dataUpdatedAt)
          .toLocaleTimeString("en-US", { hour: "numeric", minute: "2-digit" })
          .toUpperCase()
      : null;

  return (
    <main
      style={style}
      className="mx-auto max-w-4xl px-8 pt-16 pb-20 animate-in fade-in duration-200"
    >
      <button
        type="button"
        onClick={onBack}
        className="flex cursor-pointer items-center gap-1 font-mono text-[11px] tracking-[0.18em] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
      >
        <ChevronLeft size={12} aria-hidden />
        DASHBOARD
      </button>

      <header className="relative mt-6 pl-5">
        <span
          aria-hidden
          className="absolute inset-y-1 left-0 w-[3px] rounded-full bg-(--accent)"
        />
        <p className="font-mono text-[11px] tracking-[0.18em] text-(--accent)">
          {meetingLine.toUpperCase()}
        </p>
        <h1 className="mt-2 text-[28px] font-semibold leading-tight tracking-tight">
          {info.displayName}
        </h1>
        <p className="mt-1 text-[13px] text-muted-foreground">
          {info.instructors}
        </p>
      </header>

      {tree !== undefined && tree.length > 0 && (
        <MasterGuideStrip
          classId={info.id}
          guide={guideMap.get("master")}
          onView={() => setViewScope("master")}
          onWatchLive={(jobId) =>
            setViewFile({
              relPath: MASTER_OUTPUT_PATH,
              name: "Semester Master",
              kind: "html",
              live: true,
              jobId,
            })
          }
        />
      )}

      <section className="mt-12">
        <div className="flex items-baseline justify-between border-b pb-3">
          <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
            MATERIALS
          </h2>
          <div className="flex items-baseline gap-4">
            {scannedLabel && (
              <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                SCANNED {scannedLabel}
              </span>
            )}
            <button
              type="button"
              onClick={() => refetch()}
              disabled={isFetching}
              className="flex cursor-pointer items-center gap-1.5 font-mono text-[11px] tracking-[0.18em] text-muted-foreground transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent) disabled:pointer-events-none"
            >
              <RefreshCw
                size={11}
                aria-hidden
                className={isFetching ? "animate-spin" : undefined}
              />
              RESCAN
            </button>
          </div>
        </div>

        {synthError && (
          <p className="mt-3 font-mono text-[11px] text-destructive">
            SYNTHESIS NOT STARTED — {synthError}
          </p>
        )}

        <div className="mt-4">
          {error ? (
            <p className="py-10 text-center font-mono text-xs text-destructive">
              SCAN FAILED — {String(error)}
            </p>
          ) : isPending || tree === undefined ? (
            <p className="py-10 text-center font-mono text-xs text-muted-foreground">
              SCANNING…
            </p>
          ) : tree.length === 0 ? (
            <EmptyMaterials classId={info.id} />
          ) : (
            <FileTree
              classId={info.id}
              nodes={tree}
              onEntryMissing={() => refetch()}
              onViewFile={(node) =>
                setViewFile({
                  relPath: node.relPath,
                  name: node.name,
                  kind: node.kind ?? "other",
                })
              }
              guideControls={{
                guides: guideMap,
                activeScopes,
                onSynthesize: handleSynthesize,
                onView: setViewScope,
              }}
            />
          )}
        </div>
      </section>

      {(activePractice.length > 0 || (practice?.length ?? 0) > 0) && (
        <section className="mt-12">
          <div className="flex items-baseline justify-between border-b pb-3">
            <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
              PRACTICE EXAMS
            </h2>
            <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
              {(practice?.length ?? 0) === 1
                ? "1 EXAM"
                : `${practice?.length ?? 0} EXAMS`}
            </span>
          </div>
          <div className="mt-3 space-y-1">
            {activePractice.map((job) => (
              <div
                key={job.id}
                className="flex h-8 items-center gap-2 rounded-md px-2 font-mono text-[10px] tracking-[0.14em] text-(--accent)"
              >
                <span
                  aria-hidden
                  className="size-1.5 shrink-0 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
                />
                {job.status === "running" ? "GENERATING" : "QUEUED"}
                {` — ${job.scope && job.scope !== "master" ? job.scope.toUpperCase() : "SEMESTER"}`}
                <span className="font-normal text-muted-foreground/70">
                  · LIVE IN THE JOB CENTER
                </span>
              </div>
            ))}
            {(practice ?? []).map((exam) => (
              <ManagedRow
                key={exam.relPath}
                icon={FileQuestion}
                file={exam}
                strippedExt=".html"
                stamp={formatGeneratedAt(exam.modifiedAt)}
                onView={(name) =>
                  setViewFile({ relPath: exam.relPath, name, kind: "html" })
                }
              />
            ))}
          </div>
        </section>
      )}

      {(notes?.length ?? 0) > 0 && (
        <section className="mt-12">
          <div className="flex items-baseline justify-between border-b pb-3">
            <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
              NOTES
            </h2>
            <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
              {(notes?.length ?? 0) === 1 ? "1 NOTE" : `${notes?.length} NOTES`}
            </span>
          </div>
          <div className="mt-3 space-y-1">
            {(notes ?? []).map((note) => (
              <ManagedRow
                key={note.relPath}
                icon={NotepadText}
                file={note}
                strippedExt=".md"
                stamp={formatGeneratedAt(note.modifiedAt)}
                onView={(name) =>
                  setViewFile({
                    relPath: note.relPath,
                    name,
                    kind: note.name.toLowerCase().endsWith(".md") ? "md" : "other",
                  })
                }
              />
            ))}
          </div>
        </section>
      )}

      {viewedGuide && (
        <GuideViewer
          classId={info.id}
          guide={viewedGuide}
          onClose={() => setViewScope(null)}
        />
      )}
      {viewFile && (
        <FileViewer
          classId={info.id}
          file={viewFile}
          onClose={() => setViewFile(null)}
        />
      )}
    </main>
  );
}

/** A row in an app-managed listing (practice exams, notes) — FileRow's shape. */
function ManagedRow({
  icon: Icon,
  file,
  strippedExt,
  stamp,
  onView,
}: {
  icon: LucideIcon;
  file: ManagedFile;
  strippedExt: string;
  stamp: string;
  onView: (name: string) => void;
}) {
  const name = file.name.toLowerCase().endsWith(strippedExt)
    ? file.name.slice(0, -strippedExt.length)
    : file.name;
  return (
    <div className="group flex h-8 items-center gap-2 rounded-md px-2 transition-colors hover:bg-muted/60">
      <Icon size={14} aria-hidden className="shrink-0 text-muted-foreground/80" />
      <button
        type="button"
        title={`View ${name}`}
        onClick={() => onView(name)}
        className="min-w-0 flex-1 cursor-pointer truncate text-left text-[13px] transition-colors hover:text-(--accent) focus-visible:outline-2 focus-visible:outline-(--accent)"
      >
        {name}
      </button>
      <span className="shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
        {stamp}
      </span>
    </div>
  );
}

function EmptyMaterials({ classId }: { classId: number }) {
  return (
    <div className="rounded-xl border border-dashed px-8 py-14 text-center">
      <FolderOpen
        size={22}
        aria-hidden
        className="mx-auto text-muted-foreground/50"
      />
      <p className="mt-4 text-[13px] font-medium">No material yet</p>
      <p className="mx-auto mt-1 max-w-sm text-[13px] text-muted-foreground">
        Files added to this class folder in Finder appear here after a rescan.
      </p>
      <button
        type="button"
        onClick={() => void openInDefaultApp(classId, "")}
        className="mt-6 cursor-pointer rounded-md border px-3.5 py-1.5 text-[12px] font-medium transition-colors hover:bg-muted focus-visible:outline-2 focus-visible:outline-(--accent)"
      >
        Open folder in Finder
      </button>
    </div>
  );
}
