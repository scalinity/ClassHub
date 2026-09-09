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
    | "canvasSyllabus"
    | "recordings"
    | "project"
    | "cards"
    | "practice"
    | "index";
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
      // Today's waiting lines count the proposed deadlines.
      void queryClient.invalidateQueries({ queryKey: ["today"] });
      // The project's items are deadlines (SPEC §8.6), and a brief's row is
      // labelled by its deadline's title.
      void queryClient.invalidateQueries({ queryKey: ["project"] });
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      break;
    case "project":
      // The `Project material` pick changed.
      void queryClient.invalidateQueries({ queryKey: ["project"] });
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
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
      void queryClient.invalidateQueries({ queryKey: ["today"] });
      break;
    case "notes":
      void queryClient.invalidateQueries({ queryKey: ["notes"] });
      // A chat rewrite of a note the viewer or editor has cached.
      void queryClient.invalidateQueries({ queryKey: ["classFile"] });
      // Whether a note carries `Against the room` is read off its text.
      void queryClient.invalidateQueries({ queryKey: ["noteReviews"] });
      break;
    case "proposals":
      // The workspace inbox queue and the card badges.
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      void queryClient.invalidateQueries({ queryKey: ["today"] });
      break;
    case "units":
      // A Canvas sync or a syllabus scan changed the course's divisions, and
      // the sync also stamps when each class last read from Canvas.
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      void queryClient.invalidateQueries({ queryKey: ["canvasStatus"] });
      // The weeks a lecture or a file can be filed into are read off the
      // same rows.
      void queryClient.invalidateQueries({ queryKey: ["lectureWeeks"] });
      // A Canvas card's week alternative is derived from these rows on every
      // read (SPEC §10 step 8), so a reading the rescan withdrew has to leave
      // the queue too, before a click sends it as the destination.
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      // The card and the workspace header name the current division, which
      // is resolved from those same rows.
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      // A rescan can rename a division, and its name is the label on its
      // guide and on the lectures that feed it.
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
      void queryClient.invalidateQueries({ queryKey: ["hints"] });
      break;
    case "announcements":
      // A Canvas sync recorded or updated what the professor said; the
      // workspace's NOTICES section reads it, and Today lists what is new.
      void queryClient.invalidateQueries({ queryKey: ["announcements"] });
      void queryClient.invalidateQueries({ queryKey: ["today"] });
      break;
    case "recordings":
      // A sync listed recordings behind the Zoom tool, or a capture filed
      // one; the Lectures section lists what still waits, as does Today.
      void queryClient.invalidateQueries({ queryKey: ["recordings"] });
      void queryClient.invalidateQueries({ queryKey: ["today"] });
      break;
    case "canvasSyllabus":
      // A Canvas sync mirrored the syllabus page; the scan picker offers it.
      void queryClient.invalidateQueries({ queryKey: ["canvasSyllabus"] });
      break;
    case "cards":
      // A card was answered: the class's listing follows. The dashboard's
      // ten hold still — the day's ten are the list as it was fetched, and
      // an answered card leaves the face on its own (TenCards.tsx).
      void queryClient.invalidateQueries({ queryKey: ["cards", "list"] });
      break;
    case "practice":
      // An exam's self-score landed; its row shows the score.
      void queryClient.invalidateQueries({ queryKey: ["practice"] });
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
      void queryClient.invalidateQueries({ queryKey: ["hints"] });
      // A Canvas card's week alternative lands beside a file already at its
      // destination (SPEC §10 step 8), so it is read off the disk as well as
      // the rows; a file that arrived there since is what the queue must see
      // before a click sends the unbumped path.
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      break;
    case "index":
      // A scan asked for from the workspace changed the file index — a file
      // added, changed or removed on disk, or a transcript deleted or moved
      // in Finder forgotten or refiled (SPEC §8.5). Staleness, the divisions'
      // counts, the lectures and the card's stale count read what the scan
      // rewrote; the tree is that scan's own result, so it is not run again.
      // The launch scan, whose tree reaches no one, emits `files` instead.
      void queryClient.invalidateQueries({ queryKey: ["guides"] });
      void queryClient.invalidateQueries({ queryKey: ["units"] });
      void queryClient.invalidateQueries({ queryKey: ["contributions"] });
      void queryClient.invalidateQueries({ queryKey: ["hints"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      // The same disk the alternative reads (above, under `files`).
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      break;
  }
});
