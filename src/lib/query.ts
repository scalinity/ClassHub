import { QueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";

/** One cache for the whole app, so backend pushes can invalidate it. */
export const queryClient = new QueryClient();

interface HubChange {
  /** What changed backend-side (tools.rs, sorter.rs, deadlines.rs). */
  area: "deadlines" | "grades" | "notes" | "proposals" | "files" | "syllabus";
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
      // Surfaces on the class cards (computed grade lands in M11).
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "syllabus":
      // The workspace's syllabus confirm cards.
      void queryClient.invalidateQueries({ queryKey: ["syllabusProposals"] });
      break;
    case "notes":
      void queryClient.invalidateQueries({ queryKey: ["notes"] });
      break;
    case "proposals":
      // The workspace inbox queue and the card badges.
      void queryClient.invalidateQueries({ queryKey: ["sortState"] });
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
      break;
    case "files":
      // An approved move changed the tree on disk; the rescan also refreshes
      // guide staleness through the tree-keyed guides query.
      void queryClient.invalidateQueries({ queryKey: ["classTree"] });
      break;
  }
});
