import { QueryClient } from "@tanstack/react-query";
import { listen } from "@tauri-apps/api/event";

/** One cache for the whole app, so backend pushes can invalidate it. */
export const queryClient = new QueryClient();

interface HubChange {
  /** What a chat write tool changed (tools.rs emit_hub_change). */
  area: "deadlines" | "grades" | "notes" | "proposals";
}

// Chat write tools change hub data backend-side; this push turns each write
// into a targeted refetch, so the UI reflects it without a manual refresh.
// Module-level listener — no useEffect per workspace rules.
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
      break; // the confirm queue UI lands in M9
  }
});
