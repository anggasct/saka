import { useSyncExternalStore } from "react";

import { platform } from "@/platform";

export function useNarrow(): boolean {
  return useSyncExternalStore(
    platform.viewport.subscribe,
    platform.viewport.narrow,
  );
}
