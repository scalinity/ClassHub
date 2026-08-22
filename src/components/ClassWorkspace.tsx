import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronLeft, FolderOpen, RefreshCw } from "lucide-react";

import { FileTree } from "@/components/FileTree";
import { GuideViewer } from "@/components/GuideViewer";
import { MasterGuideStrip } from "@/components/MasterGuide";
import type { ClassInfo } from "@/lib/classes";
import { listGuides, synthesizeModule } from "@/lib/guides";
import { useJobs } from "@/lib/jobs";
import { openInDefaultApp, scanClass } from "@/lib/materials";
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

  const [viewScope, setViewScope] = useState<string | null>(null);
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

      {viewedGuide && (
        <GuideViewer
          classId={info.id}
          guide={viewedGuide}
          onClose={() => setViewScope(null)}
        />
      )}
    </main>
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
