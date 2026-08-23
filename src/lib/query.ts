import { QueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";

/** One cache for the whole app, so backend pushes can invalidate it. */
export const queryClient = new QueryClient();

interface HubChange {
  /** What changed backend-side (tools.rs and sorter.rs emit_hub_change). */
  area: "deadlines" | "grades" | "notes" | "proposals" | "files";
}

// Backend writes (chat tools, drop-to-sort) change hub data; this push turns
// each one into a targeted refetch, so the UI reflects it without a manual
// refresh. Module-level listener — no useEffect per workspace rules.
void listen<HubChange>("hub-changed", ({ payload }) => {
  switch (payload.area) {
    case "deadlines":
    case "grades":
      // Both surface on the class cards (nearest deadline now; grade in M11).
      void queryClient.invalidateQueries({ queryKey: ["classes"] });
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
