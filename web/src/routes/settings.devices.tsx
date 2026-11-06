import { createFileRoute } from "@tanstack/react-router";

import { DevicesPage } from "@/features/auth/devices-page";

export const Route = createFileRoute("/settings/devices")({
  component: DevicesPage,
});
