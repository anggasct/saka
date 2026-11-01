import { createFileRoute } from "@tanstack/react-router";

import { DeliveryPage } from "@/features/delivery/delivery-page";

export const Route = createFileRoute("/delivery")({
  component: DeliveryPage,
});
