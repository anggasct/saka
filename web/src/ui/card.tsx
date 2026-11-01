import type { ComponentProps } from "react";

import { cn } from "./cn";

export function Card({ className, ...props }: ComponentProps<"section">) {
  return (
    <section
      className={cn(
        "rounded-lg border border-border bg-card text-card-foreground",
        className,
      )}
      {...props}
    />
  );
}
