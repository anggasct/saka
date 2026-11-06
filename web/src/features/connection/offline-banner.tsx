import { useSyncExternalStore } from "react";

import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";

export function OfflineBanner() {
  const t = useT();
  const online = useSyncExternalStore(
    platform.connection.subscribe,
    platform.connection.online,
  );
  if (online) {
    return null;
  }
  return (
    <div role="status" className="bg-muted px-4 py-2 text-sm">
      {t("connection.offline")}
    </div>
  );
}
