import { useState } from "react";
import { QueryClientProvider, useQuery } from "@tanstack/react-query";
import { Settings2 } from "lucide-react";

import { ChatSidebar } from "@/components/ChatSidebar";
import { ClassCard } from "@/components/ClassCard";
import { ClassWorkspace } from "@/components/ClassWorkspace";
import { DeadlineStrip } from "@/components/DeadlineStrip";
import { AuthWarning, JobCenter } from "@/components/JobCenter";
import { NoticeToast } from "@/components/NoticeToast";
import { SectionHeading } from "@/components/SectionHeading";
import { SettingsScreen } from "@/components/Settings";
import { TenCards } from "@/components/TenCards";
import { Today } from "@/components/Today";
import { WeekSchedule } from "@/components/WeekSchedule";
import {
  daysSinceSync,
  getCanvasStatus,
  SYNC_STALE_DAYS,
  syncAgeLabel,
  useCanvasSync,
} from "@/lib/canvas";
import { classesQuery, type ClassInfo } from "@/lib/classes";
import { queryClient } from "@/lib/query";
import { setDropTarget } from "@/lib/sorter";
import { buttonIconNeutral, errorLine, meta } from "@/lib/styles";
import { dragWindow } from "@/lib/window";

function Dashboard({
  onOpen,
  onSettings,
}: {
  /** Opens a workspace, and a document in it when Today names one. */
  onOpen: (cls: ClassInfo, scope?: string) => void;
  onSettings: () => void;
}) {
  const { data: classes, error } = useQuery(classesQuery());

  // The day is the dashboard's headline: the question every week starts with
  // is where each course is today (SPEC §8.5).
  const dateLabel = new Date().toLocaleDateString("en-US", {
    weekday: "long",
    month: "long",
    day: "numeric",
  });

  return (
    <main className="mx-auto max-w-4xl px-8 pt-14 pb-20">
      <header className="flex items-end justify-between gap-6">
        <div>
          <p className="text-title font-semibold text-muted-foreground">ClassHub</p>
          <h1 className="mt-3 text-display">{dateLabel}</h1>
        </div>
        <div className="flex flex-col items-end gap-1 pb-1">
          <div className="flex items-center gap-1.5">
            <p className={meta}>Fall 2026 · AI in Biomedical &amp; Health Sciences</p>
            <button
              type="button"
              aria-label="Open settings"
              title="Settings"
              onClick={onSettings}
              className={buttonIconNeutral}
            >
              <Settings2 size={14} aria-hidden />
            </button>
          </div>
          <CanvasLine onSettings={onSettings} />
        </div>
      </header>

      {error ? (
        <p className={`${errorLine} mt-16 text-center`}>
          Couldn't load the classes: {String(error)}
        </p>
      ) : (
        <>
          {/* The day's docket under its date (SPEC §12), then the cards. */}
          <Today onOpen={onOpen} />
          <TenCards />
          {classes !== undefined && <WeekSchedule classes={classes} />}
          <DeadlineStrip />
          <section className="mt-12" aria-label="Classes">
            <SectionHeading title="Classes" />
            <div className="mt-5 grid gap-4 md:grid-cols-2">
              {(classes ?? []).map((cls) => (
                <ClassCard key={cls.id} info={cls} onOpen={onOpen} />
              ))}
            </div>
          </section>
        </>
      )}
    </main>
  );
}

/**
 * SPEC §12 — when Canvas data last came across, on the dashboard where it is
 * read every day: `Canvas synced 6 days ago`, destructive past a week. On
 * 2026-09-02 the answer was six days and nothing on screen said so. Opens
 * Settings, where the sync lives.
 */
function CanvasLine({ onSettings }: { onSettings: () => void }) {
  const { data: status } = useQuery({
    queryKey: ["canvasStatus"],
    queryFn: getCanvasStatus,
  });
  const progress = useCanvasSync();
  if (status === undefined) return null;
  const running = progress !== null && !progress.done;
  const seconds = status.lastSyncedAt;
  const stale =
    !running && seconds !== null && daysSinceSync(seconds) >= SYNC_STALE_DAYS;
  return (
    <button
      type="button"
      onClick={onSettings}
      title={running ? "A Canvas sync is running" : "Sync from Settings"}
      className={`cursor-pointer rounded-sm text-meta tabular-nums transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring ${
        stale ? "text-destructive" : "text-muted-foreground"
      }`}
    >
      Canvas {syncAgeLabel(seconds, running)}
    </button>
  );
}

type View = "dashboard" | "settings" | ClassInfo;

export default function App() {
  const [view, setViewState] = useState<View>("dashboard");
  // A document Today asked the workspace to open with — a pre-read's scope.
  const [openScope, setOpenScope] = useState<string | null>(null);
  // Each view is its own page; carrying scroll depth between them opens the
  // next one mid-scroll.
  // Every view change goes through here, so the drop target rides along with
  // it rather than being written during render (SPEC §13: event handlers and
  // derived state). Native file drops land in the open class's inbox
  // (SPEC §10); sorter.ts already carries the dashboard default for the
  // initial mount.
  const setView = (next: View, scope?: string) => {
    window.scrollTo(0, 0);
    setDropTarget(typeof next === "object" ? next.id : null);
    setOpenScope(scope ?? null);
    setViewState(next);
  };

  return (
    <QueryClientProvider client={queryClient}>
      <div className="min-h-screen">
        {/* Drag strip clearing the macOS traffic lights (overlay title bar). */}
        <div onMouseDown={dragWindow} className="fixed inset-x-0 top-0 z-10 h-9" />
        {view === "dashboard" ? (
          <Dashboard
            onOpen={setView}
            onSettings={() => setView("settings")}
          />
        ) : view === "settings" ? (
          <SettingsScreen onBack={() => setView("dashboard")} />
        ) : (
          <ClassWorkspace
            key={`${view.id}-${openScope ?? ""}`}
            info={view}
            initialScope={openScope}
            onBack={() => setView("dashboard")}
          />
        )}
        <JobCenter />
        <NoticeToast />
        <ChatSidebar />
        <AuthWarning />
      </div>
    </QueryClientProvider>
  );
}
