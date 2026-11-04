import type { ComponentProps, ReactNode } from "react";

import { cn } from "./cn";

export function Disclosure({
  summary,
  className,
  children,
  ...props
}: Omit<ComponentProps<"details">, "children"> & {
  summary: ReactNode;
  children: ReactNode;
}) {
  return (
    <details className={cn("group", className)} {...props}>
      <summary className="inline-flex min-h-11 cursor-pointer items-center text-sm font-medium focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring">
        {summary}
      </summary>
      {children}
    </details>
  );
}
