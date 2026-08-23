import { useState } from "react";
import { QueryClientProvider, useQuery } from "@tanstack/react-query";
import { Settings2 } from "lucide-react";

import { ChatSidebar } from "@/components/ChatSidebar";
import { ClassCard } from "@/components/ClassCard";
import { ClassWorkspace } from "@/components/ClassWorkspace";
import { DeadlineStrip } from "@/components/DeadlineStrip";
import { AuthWarning, JobCenter } from "@/components/JobCenter";
import { SettingsScreen } from "@/components/Settings";
import { WeekSchedule } from "@/components/WeekSchedule";
import { listClasses, type ClassInfo } from "@/lib/classes";
import { queryClient } from "@/lib/query";
import { setDropTarget } from "@/lib/sorter";
import { dragWindow } from "@/lib/window";

function Dashboard({
  onOpen,
  onSettings,
}: {
  onOpen: (cls: ClassInfo) => void;
  onSettings: () => void;
}) {
  const { data: classes, error } = useQuery({
    queryKey: ["classes"],
    queryFn: listClasses,
  });

  const dateLabel = new Date()
    .toLocaleDateString("en-US", {
      weekday: "short",
      month: "short",
      day: "numeric",
    })
    .toUpperCase()
    .replace(",", " ·");

  return (
    <main className="mx-auto max-w-4xl px-8 pt-16 pb-20">
      <header>
        <p className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
          FALL 2026 · AI IN BIOMEDICAL &amp; HEALTH SCIENCES
        </p>
        <div className="mt-2 flex items-baseline justify-between">
          <h1 className="text-[28px] font-semibold tracking-tight">
            ClassHub
          </h1>
          <div className="flex items-center gap-2.5">
            <p className="font-mono text-xs text-muted-foreground">
              {dateLabel}
            </p>
            <button
              type="button"
              aria-label="Open settings"
              title="Settings"
              onClick={onSettings}
              className="cursor-pointer rounded p-1.5 text-muted-foreground transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring"
            >
              <Settings2 size={14} aria-hidden />
            </button>
          </div>
        </div>
      </header>

      {error ? (
        <p className="mt-16 text-center font-mono text-xs text-destructive">
          FAILED TO LOAD CLASSES — {String(error)}
        </p>
      ) : (
        <>
          {classes !== undefined && <WeekSchedule classes={classes} />}
          <DeadlineStrip />
          <section className="mt-10" aria-label="Classes">
            <div className="border-b pb-3">
              <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
                CLASSES
              </h2>
            </div>
            <div className="mt-5 grid gap-5 md:grid-cols-2">
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

type View = "dashboard" | "settings" | ClassInfo;

export default function App() {
  const [view, setViewState] = useState<View>("dashboard");
  // Each view is its own page; carrying scroll depth between them opens the
  // next one mid-scroll.
  const setView = (next: View) => {
    window.scrollTo(0, 0);
    setViewState(next);
  };
  // Native file drops land in the open class's inbox (SPEC §10). A module
  // variable read only by the drag-drop listener — idempotent to set here.
  setDropTarget(typeof view === "object" ? view.id : null);

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
          <ClassWorkspace info={view} onBack={() => setView("dashboard")} />
        )}
        <JobCenter />
        <ChatSidebar />
        <AuthWarning />
      </div>
    </QueryClientProvider>
  );
}
