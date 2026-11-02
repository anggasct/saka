import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import { Skeleton } from "@/ui/skeleton";

import { codeOf } from "./page-error";
import { useWorkPacket } from "./queries";
import { Section } from "./section";
import { errorText, label, sha } from "./text";

function digest(value: string): string {
  return sha(value.replace(/^sha256:/, ""));
}

export function WorkPacketSection({ workItemId }: { workItemId: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  const packet = useWorkPacket(workItemId, open);
  const regionId = useId();
  return (
    <Section title={t("task.packet.title")}>
      <Button
        variant="outline"
        aria-expanded={open}
        aria-controls={regionId}
        onClick={() => {
          setOpen((value) => !value);
        }}
      >
        {open ? t("task.packet.hide") : t("task.packet.show")}
      </Button>
      <div id={regionId} hidden={!open} className="space-y-3">
        {open &&
          (packet.isPending ? (
            <Skeleton className="h-24" />
          ) : packet.isError ? (
            <p className="text-sm text-muted-foreground">
              {errorText(
                t,
                codeOf(packet.error),
                packet.error instanceof ApiError ? packet.error.params : {},
              )}
            </p>
          ) : (
            <>
              <p className="text-sm text-muted-foreground">
                {t("task.packet.summary", {
                  bytes: packet.data.bytes,
                  sha: digest(packet.data.sha256),
                })}
              </p>
              <ol className="space-y-1 text-sm">
                {packet.data.documents.map((doc) => (
                  <li key={doc.index} className="break-all">
                    {t("task.packet.document", {
                      index: doc.index,
                      kind: label(t, "packet.kind", doc.kind),
                      origin: doc.origin
                        ? label(t, "packet.origin", doc.origin)
                        : "none",
                      path: doc.path,
                      bytes: doc.bytes,
                    })}
                  </li>
                ))}
              </ol>
              <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
                {packet.data.text}
              </pre>
            </>
          ))}
      </div>
    </Section>
  );
}
