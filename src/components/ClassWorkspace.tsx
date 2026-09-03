import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  ChevronLeft,
  FileQuestion,
  FolderOpen,
  Mic,
  NotepadText,
  RefreshCw,
  type LucideIcon,
} from "lucide-react";

import { AddLecture } from "@/components/AddLecture";
import { DeadlinesSection } from "@/components/Deadlines";
import { FileTree } from "@/components/FileTree";
import { FileViewer, type ViewedFile } from "@/components/FileViewer";
import { GradesSection } from "@/components/Grades";
import { GuideViewer } from "@/components/GuideViewer";
import { InboxQueue } from "@/components/InboxQueue";
import { MasterGuideStrip } from "@/components/MasterGuide";
import { NoteEditor, type EditedNote } from "@/components/NoteEditor";
import { NoticesSection } from "@/components/Notices";
import { StructureSection } from "@/components/Structure";
import {
  CLASS_ACCENTS,
  classesQuery,
  currentUnitLabel,
  type ClassInfo,
} from "@/lib/classes";
import {
  formatGeneratedAt,
  generatePractice,
  listGuides,
  MASTER_OUTPUT_PATH,
  SESSION_SCOPE_PREFIX,
  synthesizeModule,
  synthesizeUnit,
} from "@/lib/guides";
import { useJobs } from "@/lib/jobs";
import {
  collectTranscripts,
  dateFromFileName,
  digestLecture,
  lectureWeeks,
  listLectureContributions,
} from "@/lib/lectures";
import {
  listNotes,
  listPractice,
  openInDefaultApp,
  pdfViewPath,
  scanClass,
  type ManagedFile,
  type TreeNode,
} from "@/lib/materials";
import { formatTimeRange, todayIso, weekdayLabel } from "@/lib/schedule";
import { useDragState } from "@/lib/sorter";

export function ClassWorkspace({
  info: snapshot,
  onBack,
}: {
  info: ClassInfo;
  onBack: () => void;
}) {
  // The card the dashboard handed over is a snapshot from navigation time. A
  // syllabus scan run from here rewrites the divisions, and the `units` hub
  // change refetches ["classes"], so everything below reads the live card —
  // one source for the header and the Structure marker alike — with the
  // snapshot standing in until the first fetch lands.
  const { data: classes } = useQuery(classesQuery());
  const info = classes?.find((c) => c.id === snapshot.id) ?? snapshot;

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

  // Guide state (M5/M6). Staleness is computed on demand backend-side. The
  // key stays stable: the jobs store invalidates ["guides"] when a synthesis
  // job settles, and the upsert commits before the status flips, so the
  // refetch always sees the new row.
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
  const { data: guides } = useQuery({
    queryKey: ["guides", info.id],
    queryFn: () => listGuides(info.id),
    placeholderData: (prev) => prev,
  });
  const guideMap = new Map((guides ?? []).map((g) => [g.scope, g]));
  // Session documents share the guides table but are per-lecture, so they get
  // their own listing. Rel paths open with the session date, which is the order
  // they belong in — newest first.
  const sessions = (guides ?? [])
    .filter((g) => g.session)
    .sort((a, b) => b.relPath.localeCompare(a.relPath));
  const activeDigests = jobs.filter(
    (j) =>
      j.kind === "lecture_digest" &&
      j.classId === info.id &&
      (j.status === "running" || j.status === "queued"),
  );
  // Transcripts with no session document yet — the state a transcript lands in
  // when the sorter filed it, since approving a move never touches ingestion.
  const digestedPaths = new Set(
    sessions.map((s) => s.scope.slice(SESSION_SCOPE_PREFIX.length)),
  );
  const activeDigestPaths = new Set(activeDigests.map((j) => j.scope ?? ""));
  const pendingTranscripts = collectTranscripts(tree).filter(
    (t) => !digestedPaths.has(t.relPath) && !activeDigestPaths.has(t.relPath),
  );
  // Which division each lecture feeds (SPEC §8.5). Refetched on the same edges
  // as the tree: filing a lecture writes the map, and approving a move rewrites
  // it, both of which push a `files` hub change.
  const { data: contributions } = useQuery({
    queryKey: ["contributions", info.id],
    queryFn: () => listLectureContributions(info.id),
    placeholderData: (prev) => prev,
  });
  const unitFor = new Map(
    (contributions ?? []).map((c) => [c.relPath, c.unitName]),
  );
  // The weeks the course declares, for a file named for one to offer its week
  // folder from its row (SPEC §10) — the form's own read, keyed on today so
  // the two share a cache entry, and invalidated with the divisions.
  const { data: weeks } = useQuery({
    queryKey: ["lectureWeeks", info.id, todayIso()],
    queryFn: () => lectureWeeks(info.id, todayIso()),
    placeholderData: (prev) => prev,
  });

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
  const activePracticeScopes = new Set(
    activePractice.map((j) => j.scope ?? ""),
  );
  const { data: practice } = useQuery({
    queryKey: ["practice", info.id],
    queryFn: () => listPractice(info.id),
    placeholderData: (prev) => prev,
  });
  const { data: notes } = useQuery({
    queryKey: ["notes", info.id],
    queryFn: () => listNotes(info.id),
  });

  const drag = useDragState();
  const [viewScope, setViewScope] = useState<string | null>(null);
  const [viewFile, setViewFile] = useState<ViewedFile | null>(null);
  const [editingNote, setEditingNote] = useState<EditedNote | null>(null);
  const [addingLecture, setAddingLecture] = useState(false);
  // Something the Materials tree asked for and the backend turned down — a
  // folder guide, a practice exam, or a file to frame — as the line to show
  // under that heading.
  const [materialsError, setMaterialsError] = useState<string | null>(null);
  // Its own, because the only render site for materialsError is the Materials
  // header — a failed distillation surfaced there, above the fold, under a
  // heading about synthesis.
  const [digestError, setDigestError] = useState<string | null>(null);
  const handleSynthesize = (scope: string) => {
    setMaterialsError(null);
    synthesizeModule(info.id, scope).catch((e) =>
      setMaterialsError(`SYNTHESIS NOT STARTED — ${String(e)}`),
    );
  };
  const handlePractice = (scope: string) => {
    setMaterialsError(null);
    generatePractice(info.id, scope).catch((e) =>
      setMaterialsError(`PRACTICE EXAM NOT STARTED — ${String(e)}`),
    );
  };
  /**
   * A Materials row asked to be read in-app (SPEC §12). A PDF is framed
   * from its own path and a deck from its converted twin; a deck the pipeline
   * has not converted yet opens in its default app instead, and a lookup that
   * failed outright says so under the heading. A notebook opens its extract —
   * the flattened form chat reads — while the index holds one made from the
   * file as it is now, and itself until then; everything else opens as its
   * own text.
   */
  const openMaterial = (node: TreeNode) => {
    const kind = node.kind ?? "other";
    const file = { relPath: node.relPath, name: node.name, kind };
    const inDefaultApp = () =>
      openInDefaultApp(info.id, node.relPath).catch(() => refetch());
    if (kind === "pdf" || kind === "pptx") {
      setMaterialsError(null);
      pdfViewPath(info.id, node.relPath)
        .then((pdfPath) =>
          pdfPath === null
            ? inDefaultApp()
            : setViewFile({ ...file, pdfPath }),
        )
        .catch((e) =>
          setMaterialsError(`COULD NOT OPEN ${node.name} — ${String(e)}`),
        );
      return;
    }
    if (kind === "ipynb") {
      if (node.extractRelPath === undefined) void inDefaultApp();
      else setViewFile({ ...file, source: node.extractRelPath });
      return;
    }
    setViewFile(file);
  };
  const viewedGuide = viewScope ? (guideMap.get(viewScope) ?? null) : null;

  const style = {
    "--accent": CLASS_ACCENTS[info.color] ?? "var(--class-blue)",
  } as CSSProperties;

  const meetingLine = [
    ...info.meetings.map(
      (m) =>
        `${weekdayLabel(m.weekday)} ${formatTimeRange(m.startTime, m.endTime)}`,
    ),
    ...(info.currentUnit ? [currentUnitLabel(info.currentUnit.name)] : []),
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
          practiceActive={activePracticeScopes.has("master")}
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

      <InboxQueue classId={info.id} tree={tree} />

      <NoticesSection classId={info.id} />

      <StructureSection
        classId={info.id}
        currentUnitId={info.currentUnit?.id ?? null}
        controls={{
          guides: guideMap,
          activeScopes,
          activePracticeScopes,
          onSynthesize: (unitId) => synthesizeUnit(info.id, unitId),
          onPractice: (scope) => generatePractice(info.id, scope),
          onView: setViewScope,
        }}
      />

      <DeadlinesSection classId={info.id} tree={tree} />

      <GradesSection classId={info.id} />

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

        {materialsError && (
          <p className="mt-3 font-mono text-[11px] text-destructive">
            {materialsError}
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
              onViewFile={openMaterial}
              weekSlots={weeks?.slots ?? []}
              guideControls={{
                guides: guideMap,
                activeScopes,
                activePracticeScopes,
                onSynthesize: handleSynthesize,
                onPractice: handlePractice,
                onView: setViewScope,
              }}
            />
          )}
        </div>
      </section>

      <section className="mt-12" aria-label="Lectures">
        <div className="flex items-baseline justify-between border-b pb-3">
          <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
            LECTURES
          </h2>
          <div className="flex items-baseline gap-4">
            <button
              type="button"
              onClick={() => setAddingLecture(true)}
              className="shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-(--accent) transition-colors hover:bg-(--accent)/12 focus-visible:outline-2 focus-visible:outline-(--accent)"
            >
              ADD LECTURE
            </button>
            {sessions.length > 0 && (
              <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                {sessions.length === 1
                  ? "1 SESSION"
                  : `${sessions.length} SESSIONS`}
              </span>
            )}
          </div>
        </div>
        {digestError && (
          <p className="mt-3 font-mono text-[11px] text-destructive">
            NO SESSION DOCUMENT — {digestError}
          </p>
        )}
        <div className="mt-3 space-y-1">
          {sessions.length === 0 &&
            activeDigests.length === 0 &&
            pendingTranscripts.length === 0 && (
              <p className="max-w-xl py-2 text-[13px] leading-relaxed text-muted-foreground">
                Nothing recorded yet. Add a Zoom link or a recording and
                ClassHub transcribes it, files it under the week it belongs to,
                and writes both a summary of the session and the note that
                week's study guide is built from.
              </p>
            )}
          {pendingTranscripts.map((transcript) => (
            <div
              key={transcript.relPath}
              className="group flex h-8 items-center gap-2 rounded-md px-2 transition-colors hover:bg-muted/60"
            >
              <Mic
                size={14}
                aria-hidden
                className="shrink-0 text-muted-foreground/80"
              />
              <button
                type="button"
                title={`View ${transcript.name}`}
                onClick={() =>
                  setViewFile({
                    relPath: transcript.relPath,
                    name: transcript.name.replace(/\.md$/i, ""),
                    kind: "md",
                  })
                }
                className="min-w-0 flex-1 cursor-pointer truncate text-left text-[13px] transition-colors hover:text-(--accent) focus-visible:outline-2 focus-visible:outline-(--accent)"
              >
                {transcript.name.replace(/\.md$/i, "")}
              </button>
              <FeedsUnit unitName={unitFor.get(transcript.relPath)} />
              <button
                type="button"
                onClick={() => {
                  setDigestError(null);
                  digestLecture(
                    info.id,
                    transcript.relPath,
                    dateFromFileName(transcript.name) ?? todayIso(),
                  ).catch((e) => setDigestError(String(e)));
                }}
                className="shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-(--accent) opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-(--accent)"
              >
                DISTILL
              </button>
            </div>
          ))}
          {activeDigests.map((job) => (
            <div
              key={job.id}
              className="flex h-8 items-center gap-2 rounded-md px-2 font-mono text-[10px] tracking-[0.14em] text-(--accent)"
            >
              <span
                aria-hidden
                className="size-1.5 shrink-0 rounded-full bg-(--accent) animate-pulse motion-reduce:animate-none"
              />
              {job.status === "running" ? "DISTILLING" : "QUEUED"}
              <span className="min-w-0 truncate font-normal text-muted-foreground/70">
                · {job.scope?.split("/").pop() ?? "LECTURE"}
              </span>
            </div>
          ))}
          {sessions.map((session) => (
            <ManagedRow
              key={session.scope}
              icon={Mic}
              file={{
                name: session.relPath.split("/").pop() ?? session.relPath,
                relPath: session.relPath,
                modifiedAt: session.generatedAt,
              }}
              strippedExt=".html"
              stamp={formatGeneratedAt(session.generatedAt)}
              badge={
                <FeedsUnit
                  unitName={unitFor.get(
                    session.scope.slice(SESSION_SCOPE_PREFIX.length),
                  )}
                />
              }
              // Digesting removes the transcript from the pending list, so a
              // session whose transcript has since changed had no way back.
              action={
                session.stale
                  ? {
                      label: "REDISTILL",
                      onSelect: () => {
                        const relPath = session.scope.slice(
                          SESSION_SCOPE_PREFIX.length,
                        );
                        setDigestError(null);
                        digestLecture(
                          info.id,
                          relPath,
                          dateFromFileName(relPath.split("/").pop() ?? "") ??
                            todayIso(),
                        ).catch((e) => setDigestError(String(e)));
                      },
                    }
                  : undefined
              }
              onView={() => setViewScope(session.scope)}
            />
          ))}
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
                {` — ${job.scope && job.scope !== "master" ? (job.scopeLabel ?? job.scope).toUpperCase() : "SEMESTER"}`}
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

      <section className="mt-12" aria-label="Notes">
        <div className="flex items-baseline justify-between border-b pb-3">
          <h2 className="font-mono text-[11px] tracking-[0.18em] text-muted-foreground">
            NOTES
          </h2>
          <div className="flex items-baseline gap-4">
            <button
              type="button"
              onClick={() => setEditingNote({ title: null, relPath: null })}
              className="shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-(--accent) transition-colors hover:bg-(--accent)/12 focus-visible:outline-2 focus-visible:outline-(--accent)"
            >
              NEW NOTE
            </button>
            {(notes?.length ?? 0) > 0 && (
              <span className="font-mono text-[10px] tracking-[0.14em] text-muted-foreground/70">
                {notes?.length === 1 ? "1 NOTE" : `${notes?.length} NOTES`}
              </span>
            )}
          </div>
        </div>
        <div className="mt-3 space-y-1">
          {notes !== undefined && notes.length === 0 && (
            <p className="py-2 text-[13px] text-muted-foreground">
              No notes yet — start one, or ask the chat to draft one from the
              material. They live as Markdown in the class's Notes folder.
            </p>
          )}
          {(notes ?? []).map((note) => (
            <ManagedRow
              key={note.relPath}
              icon={NotepadText}
              file={note}
              strippedExt=".md"
              stamp={formatGeneratedAt(note.modifiedAt)}
              onView={(name) =>
                setEditingNote({ title: name, relPath: note.relPath })
              }
            />
          ))}
        </div>
      </section>

      {drag.active && (
        <div
          aria-hidden
          className="pointer-events-none fixed inset-0 z-40 bg-background/70 p-4 backdrop-blur-[2px] animate-in fade-in duration-150"
        >
          <div className="flex h-full flex-col items-center justify-center gap-2 rounded-2xl border-2 border-dashed border-(--accent)">
            <p className="font-mono text-[13px] font-bold tracking-[0.22em] text-(--accent)">
              DROP TO SORT
            </p>
            <p className="text-[13px] text-muted-foreground">
              Copies land in the inbox — originals stay put, and nothing moves
              without your approval.
            </p>
          </div>
        </div>
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
      {editingNote && (
        <NoteEditor
          classId={info.id}
          note={editingNote}
          onClose={() => setEditingNote(null)}
        />
      )}
      {addingLecture && (
        <AddLecture
          classId={info.id}
          onClose={() => setAddingLecture(false)}
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
  badge,
  action,
  onView,
}: {
  icon: LucideIcon;
  file: ManagedFile;
  strippedExt: string;
  stamp: string;
  /** A quiet standing label, e.g. the division a lecture feeds. */
  badge?: React.ReactNode;
  /** An optional second affordance, shown on hover the way DISTILL is. */
  action?: { label: string; onSelect: () => void };
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
      {badge}
      {action && (
        <button
          type="button"
          onClick={action.onSelect}
          className="shrink-0 cursor-pointer rounded px-1.5 py-1 font-mono text-[10px] tracking-[0.14em] text-(--accent) opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100 focus-visible:outline-2 focus-visible:outline-(--accent)"
        >
          {action.label}
        </button>
      )}
      <span className="shrink-0 font-mono text-[10px] tracking-[0.1em] text-muted-foreground">
        {stamp}
      </span>
    </div>
  );
}

/**
 * SPEC §8.5 — which of the course's divisions this lecture feeds.
 *
 * Standing rather than on hover: it is the one thing about a filed lecture that
 * is a decision rather than a fact, and the correction for a wrong one is to
 * refile the transcript, which nobody thinks to do without seeing it.
 */
function FeedsUnit({ unitName }: { unitName: string | undefined }) {
  return (
    <span
      title={
        unitName
          ? `Feeds the ${unitName} guide`
          : "Not mapped to any of this course's divisions — refile it under a week to change that"
      }
      className={`hidden shrink-0 truncate font-mono text-[10px] tracking-[0.14em] sm:block sm:max-w-[16rem] ${
        unitName ? "text-muted-foreground/70" : "text-muted-foreground/50"
      }`}
    >
      {unitName ? unitName.toUpperCase() : "NO DIVISION"}
    </span>
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
        Drop files anywhere in this window to sort them in — or add them to the
        class folder in Finder and rescan.
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
