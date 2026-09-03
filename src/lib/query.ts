import { QueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";

/** One cache for the whole app, so backend pushes can invalidate it. */
export const queryClient = new QueryClient();

interface HubChange {
  /** What changed backend-side (tools.rs, sorter.rs, deadlines.rs). */
  area:
    | "deadlines"
    | "grades"
    | "notes"
    | "proposals"
    | "files"
    | "deadlineProposals"
    | "units"
    | "announcements"
    | "canvasSyllabus";
}

// Backend writes (chat tools, drop-to-sort, deadline CRUD) change hub data;
// this push turns each one into a targeted refetch, so the UI reflects it
// without a manual refresh. Module-level listener — no useEffect per
// workspace rules.
void listen<HubChange>("hub-changed", ({ payload }) => {
  switch (payload.area) {
    case "deadlines":
      // The dashboard strip and class lists, plus the card's deadline line.
      void queryClient.invalidateQueries({ queryKey: ["deadlines"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "grades":
      // The workspace Grades section and the card's computed grade. The
      // ["classes"] refetch is deliberate — the card shows the grade — but it
      // is the expensive one: db::list_classes re-reads every inbox folder
      // from disk per class. Worth remembering if a chat turn recording
      // several grade items ever feels sluggish.
      void queryClient.invalidateQueries({ queryKey: ["grades"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "deadlineProposals":
      // The workspace's confirm cards — a syllabus scan or a Canvas sync
      // proposed something, or one was approved or skipped — and the card's
      // PROPOSED count, which reads the same queue.
      void queryClient.invalidateQueries({ queryKey: ["deadlineProposals"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "notes":
      void queryClient.invalidateQueries({ queryKey: ["notes"] });
      // A chat rewrite of a note the viewer or editor has cached.
      void queryClient.invalidateQueries({ queryKey: ["classFile"] });
      break;
    case "proposals":
      // The workspace inbox queue and the card badges.
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "units":
      // A Canvas sync or a syllabus scan changed the course's divisions, and
      // the sync also stamps when each class last read from Canvas.
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      void queryClient.invalidateQueries({ queryKey: ["canvasStatus"] });
      // The card and the workspace header name the current division, which
      // is resolved from those same rows.
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      // A rescan can rename a division, and its name is the label on its
      // guide and on the lectures that feed it.
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
      break;
    case "announcements":
      // A Canvas sync recorded or updated what the professor said; the
      // workspace's NOTICES section reads it.
      void queryClient.invalidateQueries({ queryKey: ["announcements"] });
      break;
    case "canvasSyllabus":
      // A Canvas sync mirrored the syllabus page; the scan picker offers it.
      void queryClient.invalidateQueries({ queryKey: ["canvasSyllabus"] });
      break;
    case "files":
      // An approved move changed the tree on disk.
      void queryClient.invalidateQueries({ queryKey: ["classTree"] });
      // Staleness is measured against that tree, and a refiled lecture also
      // changes which guide's sources it counts among and which transcript
      // its session document is keyed by — none of which the guides query
      // sees unless told. The card's stale count reads the same answer, and
      // a newly filed lecture emits `files` alone.
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      // The same scan points each declared division at its folder, so the
      // Structure list is reading a join the scan just rewrote.
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      // A move can be a lecture refiled into a different week, which moves
      // which division it feeds (SPEC §8.5).
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
      break;
  }
});
