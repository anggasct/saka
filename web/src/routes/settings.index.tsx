import { createFileRoute } from "@tanstack/react-router";

import { SettingsIndexPage } from "@/features/settings/settings-index-page";

export const Route = createFileRoute("/settings/")({
  component: SettingsIndexPage,
});
