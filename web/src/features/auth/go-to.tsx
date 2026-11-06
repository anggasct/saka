import { useEffect } from "react";

import { platform } from "@/platform";

export function GoTo({ path }: { path: string }) {
  useEffect(() => {
    platform.navigateTo(path);
  }, [path]);
  return null;
}
