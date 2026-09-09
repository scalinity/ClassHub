import { useState, type CSSProperties } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  BookOpen,
  ChevronLeft,
  CirclePlay,
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
import { FlaggedSection, hintsQuery } from "@/components/Flagged";
import { GradesSection } from "@/components/Grades";
import { GuideViewer } from "@/components/GuideViewer";
import { InboxQueue, inboxShown } from "@/components/InboxQueue";
import { MasterGuideStrip } from "@/components/MasterGuide";
import { NoteEditor, type EditedNote } from "@/components/NoteEditor";
import { announcementsQuery, NoticesSection } from "@/components/Notices";
import { ProjectSection, projectQuery } from "@/components/Project";
import { SectionHeading } from "@/components/SectionHeading";
import { StructureSection } from "@/components/Structure";
import { listUnits } from "@/lib/canvas";
import { CLASS_ACCENTS, classesQuery, type ClassInfo } from "@/lib/classes";
import { getDeadlineProposals, listDeadlines } from "@/lib/deadlines";
import { listGrades } from "@/lib/grades";
import {
  deltaTitle,
  formatGeneratedAt,
  generatePractice,
  listGuides,
  listNoteReviews,
  listPrereads,
  MASTER_OUTPUT_PATH,
  PROJECT_SCOPE,
  reviewNote,
  SESSION_SCOPE_PREFIX,
  synthesizeModule,
  synthesizeUnit,
  writePresentationKit,
  writePreread,
} from "@/lib/guides";
import { useJobs } from "@/lib/jobs";
import {
  clearFindProgress,
  collectTranscripts,
  dateFromFileName,
  digestLecture,
  findRecordings,
  formatDuration,
  lectureWeeks,
  listLectureContributions,
  listRecordings,
  recordingPlayUrl,
  useFindProgress,
  type LectureFormOpen,
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
import { formatClock, formatDueDate, formatMeetingDay, formatTimeRange, todayIso, weekdayLabel } from "@/lib/schedule";
import { getSortState, useDragState } from "@/lib/sorter";
import {
  buttonChip,
  buttonText,
  buttonTextMuted,
  chipAmber,
  errorLine,
  meta,
  pulseDot,
  readingText,
  row,
  statusLine,
} from "@/lib/styles";

/** A row's title as its one target: a lecture, a note. */
const rowTitle =
  "min-w-0 flex-1 cursor-pointer truncate rounded-sm text-left text-title transition-colors hover:text-(--accent-ink) focus-visible:outline-2 focus-visible:outline-(--accent)";

/** Smooth unless the reader asked for less motion. No observer, no effect: a click. */
function jumpTo(id: string) {
  const reduce = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  document
    .getElementById(id)
    ?.scrollIntoView({ behavior: reduce ? "auto" : "smooth", block: "start" });
}

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
    .filter((g) => g.family === "session")
    .sort((a, b) => b.relPath.localeCompare(a.relPath));
  // The small documents' jobs (SPEC §8.6), each keyed by the scope its row
  // will carry — a brief by its deadline, a kit by its paper, a pre-read by
  // its division, the workbook by the class — and the review by its note.
  const activeScopesOf = (kind: string) =>
    new Set(
      jobs
        .filter(
          (j) =>
            j.kind === kind &&
            j.classId === info.id &&
            (j.status === "running" || j.status === "queued"),
        )
        .map((j) => j.scope ?? ""),
    );
  const activeBriefScopes = activeScopesOf("assignment_brief");
  const activeKitScopes = activeScopesOf("presentation_kit");
  const activePrereadScopes = activeScopesOf("pre_read");
  const activeReviewPaths = activeScopesOf("notes_review");
  const workbookActive = activeScopesOf("project_workbook").has(PROJECT_SCOPE);
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
  // folder from its row (SPEC §10) — the form's own read, keyed on today, so
  // the two usually share a cache entry (the form keys on the date it shows,
  // which a dated file name can move), and invalidated with the divisions.
  const { data: weeks } = useQuery({
    queryKey: ["lectureWeeks", info.id, todayIso()],
    queryFn: () => lectureWeeks(info.id, todayIso()),
    placeholderData: (prev) => prev,
  });
  // Which files have a card waiting in the queue, read from the query the
  // queue itself renders, so a row's PROPOSED follows a dismissal there.
  const { data: sortState } = useQuery({
    queryKey: ["sortState", info.id],
    queryFn: () => getSortState(info.id),
    placeholderData: (prev) => prev,
  });
  const pendingSources = new Set(
    (sortState?.proposals ?? []).map((p) => p.sourceRelPath),
  );

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
  // Open with nothing, or pre-filled from a recording the sync found whose
  // week is the form's to pick (SPEC §7.1).
  const [addingLecture, setAddingLecture] = useState<LectureFormOpen | null>(null);
  const [findError, setFindError] = useState<string | null>(null);
  // A waiting recording's form that could not open: its own line, so it
  // never reads as a digest's failure.
  const [recordingError, setRecordingError] = useState<string | null>(null);
  // Recordings found behind the Zoom tool that still wait for a capture.
  const { data: recordings } = useQuery({
    queryKey: ["recordings", info.id],
    queryFn: () => listRecordings(info.id),
    placeholderData: (prev) => prev,
  });
  const finding = useFindProgress(info.id);
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
      setMaterialsError(`The guide didn't start: ${String(e)}`),
    );
  };
  const handlePractice = (scope: string, focus: string | null) => {
    setMaterialsError(null);
    generatePractice(info.id, scope, focus).catch((e) =>
      setMaterialsError(`The practice exam didn't start: ${String(e)}`),
    );
  };
  const handleKit = (relPath: string) => {
    setMaterialsError(null);
    writePresentationKit(info.id, relPath).catch((e) =>
      setMaterialsError(`The presentation kit didn't start: ${String(e)}`),
    );
  };
  // The coming weeks a pre-read can be written for, and the ones written
  // (SPEC §8.6); a landed session document removes its week's row.
  const { data: prereads } = useQuery({
    queryKey: ["prereads", info.id, todayIso()],
    queryFn: () => listPrereads(info.id),
    placeholderData: (prev) => prev,
  });
  const [prereadError, setPrereadError] = useState<string | null>(null);
  // The notes dated for a distilled session, and whether each has been read
  // against the room.
  const { data: noteReviews } = useQuery({
    queryKey: ["noteReviews", info.id],
    queryFn: () => listNoteReviews(info.id),
    placeholderData: (prev) => prev,
  });
  const reviewFor = new Map((noteReviews ?? []).map((t) => [t.relPath, t]));
  const [noteError, setNoteError] = useState<string | null>(null);
  const { data: project } = useQuery(projectQuery(info.id));
  // What the professor flagged (SPEC §8.4): the section and its nav link.
  // Which sessions have been read for it is the contribution row's stamp,
  // never the row count — a session the professor flagged nothing in has an
  // empty ledger, and only one distilled before the ledger existed says so.
  const { data: hints } = useQuery(hintsQuery(info.id));
  const hintsReadFor = new Map(
    (contributions ?? []).map((c) => [c.relPath, c.hintsRead]),
  );
  const openTranscriptAt = (relPath: string, anchor: string) =>
    setViewFile({
      relPath,
      name: (relPath.split("/").pop() ?? relPath).replace(/\.md$/i, ""),
      kind: "md",
      anchor,
    });
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
          setMaterialsError(`Couldn't open ${node.name}: ${String(e)}`),
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

  const metaLine = [
    ...info.meetings.map(
      (m) =>
        `${weekdayLabel(m.weekday)} ${formatTimeRange(m.startTime, m.endTime)}`,
    ),
    info.room,
    `${info.credits} ${info.credits === 1 ? "credit" : "credits"}`,
    info.instructors,
  ].join(" · ");

  const scannedLabel =
    dataUpdatedAt > 0 ? formatClock(new Date(dataUpdatedAt)) : null;

  // The nav lists a section only while it has content (SPEC §12): each link
  // reads the query its section renders — TanStack dedupes them, so none
  // costs a second request — and an empty section is reached by scrolling.
  const { data: announcements } = useQuery(announcementsQuery(info.id));
  const { data: units } = useQuery({
    queryKey: ["units", info.id],
    queryFn: () => listUnits(info.id),
  });
  const { data: allDeadlines } = useQuery({
    queryKey: ["deadlines"],
    queryFn: listDeadlines,
  });
  const { data: deadlineQueue } = useQuery({
    queryKey: ["deadlineProposals", info.id],
    queryFn: () => getDeadlineProposals(info.id),
    placeholderData: (prev) => prev,
  });
  const { data: grades } = useQuery({
    queryKey: ["grades", info.id],
    queryFn: () => listGrades(info.id),
  });
  const hasMaterials = tree !== undefined && tree.length > 0;
  const hasDeadlines =
    (allDeadlines ?? []).some((d) => d.classId === info.id && d.status === "open") ||
    (deadlineQueue?.proposals.length ?? 0) > 0;
  const hasLectures =
    sessions.length + activeDigests.length + pendingTranscripts.length > 0 ||
    (recordings?.length ?? 0) > 0 ||
    (prereads?.length ?? 0) > 0;
  const hasPractice = activePractice.length > 0 || (practice?.length ?? 0) > 0;
  const links = [
    hasMaterials && { id: "master", label: "Semester master" },
    inboxShown(sortState, jobs, info.id, drag.notice) && {
      id: "inbox",
      label: "Inbox",
    },
    (announcements?.length ?? 0) > 0 && { id: "notices", label: "Notices" },
    (units?.length ?? 0) > 0 && { id: "structure", label: "Structure" },
    hasDeadlines && { id: "deadlines", label: "Deadlines" },
    project !== undefined && project !== null && { id: "project", label: "Project" },
    (grades?.categories.length ?? 0) > 0 && { id: "grades", label: "Grades" },
    hasMaterials && { id: "materials", label: "Materials" },
    hasLectures && { id: "lectures", label: "Lectures" },
    (hints?.length ?? 0) > 0 && { id: "flagged", label: "Flagged" },
    hasPractice && { id: "practice-exams", label: "Practice exams" },
    (notes?.length ?? 0) > 0 && { id: "notes", label: "Notes" },
  ].filter((l): l is { id: string; label: string } => l !== false);

  return (
    <main
      style={style}
      className="animate-in fade-in duration-200 motion-reduce:animate-none"
    >
      {/* The band bleeds to the window edges; the fixed drag strip (36px)
          overlays its top, so the first control sits below it. */}
      <header className="bg-(--wash)">
        <div className="mx-auto max-w-4xl px-8 pt-12">
          <button type="button" onClick={onBack} className={`${buttonText} -ml-2`}>
            <ChevronLeft size={14} aria-hidden />
            Dashboard
          </button>
          <h1 className="mt-4 text-display">{info.displayName}</h1>
          {info.currentUnit && (
            <p className="mt-1.5 text-headline font-medium text-(--accent-ink)">
              {info.currentUnit.name}
            </p>
          )}
          <p className={`mt-3 ${meta}`}>{metaLine}</p>
        </div>
      </header>

      {/* A direct child of main, not of the header: a sticky element only
          sticks within its parent's box. pt-9 is the band's last-row spacing
          when unstuck and the traffic-light clearance when stuck; z-[5] keeps
          it under the z-10 drag strip so dragging still works over that 36px.
          Sections carry scroll-mt-20: stuck, the nav is pt-9 + one body line +
          pb-3, about 68px, and 80 leaves air under it; if the link row ever
          wraps at the 920px minimum, raise the offset with it. */}
      <nav aria-label="Sections" className="sticky top-0 z-[5] bg-(--wash) pt-9">
        <div className="mx-auto flex max-w-4xl flex-wrap gap-x-4 gap-y-1 px-8 pb-3">
          {links.map((link) => (
            <button
              key={link.id}
              type="button"
              onClick={() => jumpTo(link.id)}
              className="cursor-pointer rounded-sm text-body font-medium text-(--accent-ink) transition-colors hover:text-foreground focus-visible:outline-2 focus-visible:outline-(--accent)"
            >
              {link.label}
            </button>
          ))}
        </div>
      </nav>

      <div className="mx-auto max-w-4xl px-8 pb-20">
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
            onPractice: (scope, focus) => generatePractice(info.id, scope, focus),
            onView: setViewScope,
          }}
        />

        <DeadlinesSection
          classId={info.id}
          tree={tree}
          briefs={{
            guides: guideMap,
            activeScopes: activeBriefScopes,
            onView: setViewScope,
          }}
        />

        <ProjectSection
          classId={info.id}
          guide={guideMap.get(PROJECT_SCOPE)}
          active={workbookActive}
          tree={tree}
          onView={() => setViewScope(PROJECT_SCOPE)}
        />

        <GradesSection classId={info.id} />

        <section id="materials" className="mt-14 scroll-mt-20">
          <SectionHeading
            title="Materials"
            count={scannedLabel ? `scanned ${scannedLabel}` : undefined}
            actions={
              <button
                type="button"
                onClick={() => refetch()}
                disabled={isFetching}
                className={buttonTextMuted}
              >
                <RefreshCw
                  size={12}
                  aria-hidden
                  className={isFetching ? "animate-spin" : undefined}
                />
                Rescan
              </button>
            }
          >
            {materialsError && <p className={errorLine}>{materialsError}</p>}
          </SectionHeading>

          <div className="mt-4">
            {error ? (
              <p className={`${errorLine} py-8 text-center`}>
                The scan failed: {String(error)}
              </p>
            ) : isPending || tree === undefined ? (
              <p className="py-10 text-center text-body text-muted-foreground">
                Scanning…
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
                pendingSources={pendingSources}
                guideControls={{
                  guides: guideMap,
                  activeScopes,
                  activePracticeScopes,
                  activeKitScopes,
                  onSynthesize: handleSynthesize,
                  onPractice: handlePractice,
                  onKit: handleKit,
                  onView: setViewScope,
                }}
              />
            )}
          </div>
        </section>

        <section id="lectures" className="mt-14 scroll-mt-20" aria-label="Lectures">
          <SectionHeading
            title="Lectures"
            count={
              sessions.length === 0
                ? undefined
                : sessions.length === 1
                  ? "1 session"
                  : `${sessions.length} sessions`
            }
            actions={
              <>
                {finding && !finding.done ? (
                  <span className={statusLine}>
                    <span aria-hidden className={pulseDot} />
                    {finding.stage}
                  </span>
                ) : (
                  <button
                    type="button"
                    title="Read the course's Zoom recordings through Canvas and capture the new ones"
                    onClick={() => {
                      setFindError(null);
                      clearFindProgress(info.id);
                      findRecordings(info.id).catch((e) => setFindError(String(e)));
                    }}
                    className={buttonTextMuted}
                  >
                    Find recordings
                  </button>
                )}
                <button
                  type="button"
                  onClick={() => setAddingLecture({})}
                  className={buttonText}
                >
                  Add lecture
                </button>
              </>
            }
          >
            {digestError && (
              <p className={errorLine}>No session document: {digestError}</p>
            )}
            {findError && <p className={errorLine}>Not started: {findError}</p>}
            {recordingError && (
              <p className={errorLine}>The form could not open: {recordingError}</p>
            )}
            {prereadError && <p className={errorLine}>No pre-read: {prereadError}</p>}
            {finding?.done && (
              <p className={finding.error ? errorLine : `mt-3 ${meta}`}>
                {finding.error ? `Recordings not found: ${finding.error}` : finding.summary}
              </p>
            )}
          </SectionHeading>
          <div className="mt-3">
            {sessions.length === 0 &&
              activeDigests.length === 0 &&
              pendingTranscripts.length === 0 &&
              (recordings?.length ?? 0) === 0 &&
              (prereads?.length ?? 0) === 0 && (
                <p className={`max-w-xl py-2 ${readingText} text-muted-foreground`}>
                  Nothing recorded yet. Find the course's Zoom recordings, or add a
                  Zoom link or a recording, and ClassHub transcribes it, files it
                  under the week it belongs to, and writes both a summary of the
                  session and the note that week's study guide is built from.
                </p>
              )}
            {(recordings ?? []).map((recording) => (
              <div key={recording.id} className={row}>
                <CirclePlay size={14} aria-hidden className="shrink-0 text-muted-foreground" />
                <span className="min-w-0 flex-1 truncate text-title">
                  {formatDueDate(recording.recordedAt)}
                  <span className={`ml-2 ${meta}`}>{formatDuration(recording.durationMinutes)}</span>
                </span>
                <span
                  className="hidden min-w-0 shrink truncate text-fine text-muted-foreground sm:block sm:max-w-[22rem]"
                  title={recording.note ?? recording.title}
                >
                  {recording.status === "failed"
                    ? `Not captured — ${recording.note ?? "no reason recorded"}`
                    : (recording.note ?? "Found on Zoom, waiting to be captured")}
                </span>
                <button
                  type="button"
                  title="Open the Add lecture form with this recording filled in"
                  onClick={() => {
                    setRecordingError(null);
                    recordingPlayUrl(recording.id)
                      .then((source) => {
                        if (source === null) {
                          setRecordingError("this recording lists no playable file");
                          return;
                        }
                        setAddingLecture({
                          source,
                          date: recording.recordedAt.slice(0, 10),
                          recordingId: recording.id,
                        });
                      })
                      .catch((e) => setRecordingError(String(e)));
                  }}
                  className={buttonText}
                >
                  Add lecture
                </button>
              </div>
            ))}
            {(prereads ?? []).map((preread) => {
              // The page before a coming lecture (SPEC §8.6): offered while
              // the week's folder holds material and no transcript, read
              // once written, and gone once the session document lands.
              const writing = activePrereadScopes.has(preread.scope);
              const start = () => {
                setPrereadError(null);
                writePreread(info.id, preread.unitId).catch((e) => setPrereadError(String(e)));
              };
              return (
                <div key={preread.scope} className={row}>
                  <BookOpen size={14} aria-hidden className="shrink-0 text-muted-foreground" />
                  {preread.relPath !== null ? (
                    <button
                      type="button"
                      title="Read the page before class"
                      onClick={() => setViewScope(preread.scope)}
                      className={rowTitle}
                    >
                      Before class · {formatMeetingDay(preread.meetsOn)}
                    </button>
                  ) : (
                    <span className="min-w-0 flex-1 truncate text-title">
                      Before class · {formatMeetingDay(preread.meetsOn)}
                    </span>
                  )}
                  <FeedsUnit unitName={preread.unitName} />
                  {writing ? (
                    <span className={`shrink-0 px-2 ${statusLine}`}>
                      <span aria-hidden className={pulseDot} />
                      Writing the pre-read…
                    </span>
                  ) : preread.relPath === null ? (
                    <button
                      type="button"
                      title={`One page from the ${preread.files === 1 ? "file" : `${preread.files} files`} filed for the week — what to know walking in`}
                      onClick={start}
                      className={buttonText}
                    >
                      Write the pre-read
                    </button>
                  ) : (
                    <>
                      {preread.stale && preread.candidate && (
                        <button
                          type="button"
                          title="The week's folder changed since"
                          onClick={start}
                          className={`${buttonChip} bg-class-amber/12 text-class-amber hover:bg-class-amber/20`}
                        >
                          Rewrite
                        </button>
                      )}
                      <button
                        type="button"
                        onClick={() => setViewScope(preread.scope)}
                        className={buttonText}
                      >
                        Read
                      </button>
                    </>
                  )}
                  <span className={`shrink-0 ${meta}`}>
                    {preread.generatedAt !== null
                      ? formatGeneratedAt(preread.generatedAt)
                      : `${preread.files} ${preread.files === 1 ? "file" : "files"} posted`}
                  </span>
                </div>
              );
            })}
            {pendingTranscripts.map((transcript) => (
              <div key={transcript.relPath} className={row}>
                <Mic size={14} aria-hidden className="shrink-0 text-muted-foreground" />
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
                  className={rowTitle}
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
                  className={`${buttonText}`}
                >
                  Distill
                </button>
              </div>
            ))}
            {activeDigests.map((job) => (
              <div
                key={job.id}
                className="flex min-h-11 items-center gap-2.5 border-b border-border/70 px-1 last:border-b-0"
              >
                <span className={statusLine}>
                  <span aria-hidden className={pulseDot} />
                  {job.status === "running" ? "Distilling" : "Queued"}
                </span>
                <span className="min-w-0 truncate text-meta text-muted-foreground">
                  · {job.scope?.split("/").pop() ?? "lecture"}
                </span>
              </div>
            ))}
            {sessions.map((session) => {
              const transcript = session.scope.slice(SESSION_SCOPE_PREFIX.length);
              // A session read before the ledger existed carries no read
              // stamp; only a lecture mapped to a division is read for one,
              // so an unmapped one is not asked to.
              const unread = hintsReadFor.get(transcript) === false;
              return (
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
                    <>
                      {unread && (
                        <span className="hidden shrink-0 text-fine text-muted-foreground/70 sm:block">
                          Not yet read for what was flagged
                        </span>
                      )}
                      <FeedsUnit unitName={unitFor.get(transcript)} />
                    </>
                  }
                  // Digesting removes the transcript from the pending list, so a
                  // session whose transcript has since changed had no way back;
                  // one never read for the ledger takes the same way.
                  action={
                    session.stale || unread
                      ? {
                          label: "Distill again",
                          onSelect: () => {
                            setDigestError(null);
                            digestLecture(
                              info.id,
                              transcript,
                              dateFromFileName(transcript.split("/").pop() ?? "") ??
                                todayIso(),
                            ).catch((e) => setDigestError(String(e)));
                          },
                        }
                      : undefined
                  }
                  onView={() => setViewScope(session.scope)}
                />
              );
            })}
          </div>
        </section>

        <FlaggedSection classId={info.id} onOpenTranscript={openTranscriptAt} />

        {hasPractice && (
          <section id="practice-exams" className="mt-14 scroll-mt-20">
            <SectionHeading
              title="Practice exams"
              count={
                (practice?.length ?? 0) === 1
                  ? "1 exam"
                  : `${practice?.length ?? 0} exams`
              }
            />
            <div className="mt-3">
              {activePractice.map((job) => (
                <div
                  key={job.id}
                  className="flex min-h-11 items-center gap-2.5 border-b border-border/70 px-1 last:border-b-0"
                >
                  <span className={statusLine}>
                    <span aria-hidden className={pulseDot} />
                    {job.status === "running" ? "Writing" : "Queued"}
                    {` · ${job.scope && job.scope !== "master" ? (job.scopeLabel ?? job.scope) : "semester"}`}
                  </span>
                  <span className="text-meta text-muted-foreground">· live in Jobs</span>
                </div>
              ))}
              {(practice ?? []).map((exam) => (
                <ManagedRow
                  key={exam.relPath}
                  icon={FileQuestion}
                  file={exam}
                  strippedExt=".html"
                  stamp={formatGeneratedAt(exam.modifiedAt)}
                  // An exam's row records what it was built from (SPEC §8.3);
                  // one written before rows existed carries no freshness.
                  badge={
                    exam.stale === true && exam.diff !== null ? (
                      <span title={deltaTitle(exam.diff)} className={chipAmber}>
                        stale
                      </span>
                    ) : undefined
                  }
                  onView={(name) =>
                    setViewFile({ relPath: exam.relPath, name, kind: "html" })
                  }
                />
              ))}
            </div>
          </section>
        )}

        <section id="notes" className="mt-14 scroll-mt-20" aria-label="Notes">
          <SectionHeading
            title="Notes"
            count={
              (notes?.length ?? 0) === 0
                ? undefined
                : notes?.length === 1
                  ? "1 note"
                  : `${notes?.length} notes`
            }
            actions={
              <button
                type="button"
                onClick={() => setEditingNote({ title: null, relPath: null })}
                className={buttonText}
              >
                New note
              </button>
            }
          >
            {noteError && <p className={errorLine}>Not read against the room: {noteError}</p>}
          </SectionHeading>
          <div className="mt-3">
            {notes !== undefined && notes.length === 0 && (
              <p className={`max-w-xl py-2 ${readingText} text-muted-foreground`}>
                No notes yet — start one, or ask the chat to draft one from the
                material. They live as Markdown in the class's Notes folder.
              </p>
            )}
            {(notes ?? []).map((note) => {
              // A note dated for a distilled session (SPEC §8.6): read
              // against the room once, the section appended one Undo away.
              const target = reviewFor.get(note.relPath);
              const reviewing = activeReviewPaths.has(note.relPath);
              return (
                <ManagedRow
                  key={note.relPath}
                  icon={NotepadText}
                  file={note}
                  strippedExt=".md"
                  stamp={formatGeneratedAt(note.modifiedAt)}
                  badge={
                    reviewing ? (
                      <span className={`shrink-0 px-2 ${statusLine}`}>
                        <span aria-hidden className={pulseDot} />
                        Reading against the room…
                      </span>
                    ) : target?.reviewed ? (
                      <span
                        title={`Read against the ${target.date} session`}
                        className="hidden shrink-0 text-fine text-muted-foreground sm:block"
                      >
                        against the room
                      </span>
                    ) : undefined
                  }
                  action={
                    target !== undefined && !target.reviewed && !reviewing
                      ? {
                          label: "Against the room",
                          onSelect: () => {
                            setNoteError(null);
                            reviewNote(info.id, note.relPath).catch((e) =>
                              setNoteError(String(e)),
                            );
                          },
                        }
                      : undefined
                  }
                  onView={(name) =>
                    setEditingNote({ title: name, relPath: note.relPath })
                  }
                />
              );
            })}
          </div>
        </section>
      </div>

      {drag.active && (
        <div
          aria-hidden
          className="pointer-events-none fixed inset-0 z-40 bg-background/70 p-4 backdrop-blur-[2px] animate-in fade-in duration-150 motion-reduce:animate-none"
        >
          <div className="flex h-full flex-col items-center justify-center gap-2 rounded-2xl border-2 border-dashed border-(--accent)">
            <p className="text-headline text-(--accent-ink)">Drop to sort</p>
            <p className="text-body text-muted-foreground">
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
          initial={addingLecture}
          onClose={() => setAddingLecture(null)}
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
  /** An optional second affordance, shown on hover the way Distill is. */
  action?: { label: string; onSelect: () => void };
  onView: (name: string) => void;
}) {
  const name = file.name.toLowerCase().endsWith(strippedExt)
    ? file.name.slice(0, -strippedExt.length)
    : file.name;
  return (
    <div className={row}>
      <Icon size={14} aria-hidden className="shrink-0 text-muted-foreground" />
      <button
        type="button"
        title={`View ${name}`}
        onClick={() => onView(name)}
        className={rowTitle}
      >
        {name}
      </button>
      {badge}
      {action && (
        <button
          type="button"
          onClick={action.onSelect}
          className={`${buttonText}`}
        >
          {action.label}
        </button>
      )}
      <span className={`shrink-0 ${meta}`}>{stamp}</span>
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
      className={`hidden shrink-0 truncate text-fine sm:block sm:max-w-[16rem] ${
        unitName ? "text-muted-foreground" : "text-muted-foreground/60"
      }`}
    >
      {unitName ?? "no division"}
    </span>
  );
}

function EmptyMaterials({ classId }: { classId: number }) {
  return (
    <div className="rounded-xl border border-dashed border-border px-8 py-14 text-center">
      <FolderOpen size={22} aria-hidden className="mx-auto text-muted-foreground/60" />
      <p className="mt-4 text-[17px] font-semibold">No material yet</p>
      <p className="mx-auto mt-1 max-w-sm text-body text-muted-foreground">
        Drop files anywhere in this window to sort them in — or add them to the
        class folder in Finder and rescan.
      </p>
      <button
        type="button"
        onClick={() => void openInDefaultApp(classId, "")}
        className={`${buttonTextMuted} mt-6 ring-1 ring-border`}
      >
        Open folder in Finder
      </button>
    </div>
  );
}
