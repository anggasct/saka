import { useId } from "react";
import type { ReactNode } from "react";

import { Card } from "@/ui/card";

export function Section({
  title,
  children,
}: {
  title: string;
  children: ReactNode;
}) {
  const id = useId();
  return (
    <Card aria-labelledby={id} className="space-y-3 p-4">
      <h2 id={id} className="text-lg font-semibold break-words">
        {title}
      </h2>
      {children}
    </Card>
  );
}
