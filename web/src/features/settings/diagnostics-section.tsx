import { useState } from "react";

import { useFormat } from "@/features/delivery/format";
import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { record } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Disclosure } from "@/ui/disclosure";
import { Skeleton } from "@/ui/skeleton";

import { diagnosticText, fixText } from "./diagnostic-text";
import { useDiagnostics } from "./queries";
import type { DiagnosticCheck } from "./queries";

const GROUPS = ["fail", "warn", "pass", "skip"] as const;
type Group = (typeof GROUPS)[number];

const TONES: Record<Group, BadgeTone> = {
  fail: "danger",
  warn: "attention",
  pass: "success",
  skip: "neutral",
};

function isGroup(status: string): status is Group {
  return (GROUPS as readonly string[]).includes(status);
}

function Row({ check }: { check: DiagnosticCheck }) {
  const t = useT();
  const format = useFormat();
  const fix = check.fix;
  const fixParams = record(fix?.params);
  const fixSentence =
    fix === null || fix === undefined
      ? null
      : fixText(t, format.relative, fix.code, fixParams);
  const command = fixParams.command;
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={isGroup(check.status) ? TONES[check.status] : "neutral"}>
          {isGroup(check.status)
            ? t(`settings.diagnostics.status.${check.status}`)
            : check.status}
        </Badge>
        <code className="text-xs break-all">{check.id}</code>
        <span className="text-sm text-muted-foreground break-all">
          {check.subject.label}
        </span>
      </div>
      <p className="text-sm">
        {diagnosticText(t, format.relative, check.code, record(check.params))}
      </p>
      {fixSentence !== null && (
        <p className="text-sm text-muted-foreground">
          {t("settings.diagnostics.fix")} {fixSentence}
          {typeof command === "string" && (
            <>
              {" "}
              <code className="rounded bg-muted px-1 break-all">{command}</code>
            </>
          )}
        </p>
      )}
    </li>
  );
}

function Report({ projectId }: { projectId: string }) {
  const t = useT();
  const report = useDiagnostics(projectId);
  const data = report.data;
  return (
    <div className="space-y-3">
      {report.isFetching && (
        <div role="status" aria-busy="true" className="space-y-2">
          <p className="text-sm">{t("settings.diagnostics.running")}</p>
          {data === undefined && (
            <>
              <Skeleton className="h-10" />
              <Skeleton className="h-10" />
            </>
          )}
        </div>
      )}
      {report.error !== null && !report.isFetching && (
        <PageError
          title="settings.diagnostics.error"
          error={report.error}
          onRetry={() => void report.refetch()}
        />
      )}
      {data !== undefined && (
        <>
          <p className="text-sm font-medium">
            {t("settings.diagnostics.summary", {
              fail: data.summary.fail,
              warn: data.summary.warn,
              pass: data.summary.pass,
              skip: data.summary.skip,
            })}
          </p>
          {GROUPS.map((group) => {
            const rows = data.checks.filter((c) => c.status === group);
            return rows.length === 0 ? null : (
              <Disclosure
                key={group}
                open={group === "fail" || group === "warn" ? true : undefined}
                summary={t(`settings.diagnostics.group.${group}`, {
                  count: rows.length,
                })}
              >
                <ul className="divide-y divide-border">
                  {rows.map((check) => (
                    <Row
                      key={`${check.id}-${check.subject.id}`}
                      check={check}
                    />
                  ))}
                </ul>
              </Disclosure>
            );
          })}
        </>
      )}
      <Button
        variant="outline"
        disabled={report.isFetching}
        onClick={() => void report.refetch()}
      >
        {t("settings.diagnostics.again")}
      </Button>
    </div>
  );
}

export function DiagnosticsSection({ projectId }: { projectId: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  return (
    <Section title={t("settings.diagnostics.title")}>
      <p className="text-sm text-muted-foreground">
        {t("settings.diagnostics.intro")}
      </p>
      <Disclosure
        summary={t("settings.diagnostics.open")}
        onToggle={(event) => {
          setOpen(event.currentTarget.open);
        }}
      >
        {open && <Report projectId={projectId} />}
      </Disclosure>
    </Section>
  );
}
