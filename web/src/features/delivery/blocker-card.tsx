import { Link } from "@tanstack/react-router";

import { useT } from "@/i18n/use-t";

import { useFormat } from "./format";
import { PrimaryAction } from "./primary-action";
import type { Blocker, WorkItem } from "./queries";
import { Section } from "./section";
import { blockerText, label, record, sha, text } from "./text";
import { When } from "./when";

function list(value: unknown): string[] {
  return Array.isArray(value) ? value.map(text) : [];
}

export function BlockerCard({
  task,
  blocker,
  specPath,
}: {
  task: WorkItem;
  blocker: Blocker;
  specPath: string | undefined;
}) {
  const t = useT();
  const format = useFormat();
  const params = record(blocker.params);
  const checks = list(params.checks);
  const missing = list(params.missing);
  const command = text(params.command);
  const tail = text(params.log_tail);
  const log = text(params.log);
  const head = sha(params.head_sha);
  return (
    <Section title={t("task.blocker.title")}>
      <p className="break-words text-danger-foreground">
        {blockerText(t, blocker.code, params, format.relative)}
      </p>
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="font-medium">{t("task.blocker.code")}</dt>
        <dd>
          <code>{blocker.code}</code>
        </dd>
        {blocker.stage && (
          <>
            <dt className="font-medium">{t("task.blocker.stage")}</dt>
            <dd>{label(t, "stage", blocker.stage)}</dd>
          </>
        )}
        <dt className="font-medium">{t("task.blocker.opened")}</dt>
        <dd>
          <When iso={blocker.opened_at} />
        </dd>
        {head !== "" && (
          <>
            <dt className="font-medium">{t("task.head")}</dt>
            <dd>
              <code>{head}</code>
            </dd>
          </>
        )}
      </dl>
      {checks.length > 0 && (
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t("task.blocker.checks")}</h3>
          <ul className="list-disc pl-5">
            {checks.map((name) => (
              <li key={name} className="break-words">
                {name}
              </li>
            ))}
          </ul>
        </div>
      )}
      {missing.length > 0 && (
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t("task.blocker.missing")}</h3>
          <ul className="list-disc pl-5">
            {missing.map((name) => (
              <li key={name} className="break-words">
                {name}
              </li>
            ))}
          </ul>
        </div>
      )}
      {command !== "" && (
        <p className="text-sm break-words">
          {params.timed_out === true
            ? t("task.blocker.commandTimedOut", { command })
            : t("task.blocker.commandFailed", {
                command,
                exit_code: text(params.exit_code),
              })}
        </p>
      )}
      {tail !== "" && (
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t("task.blocker.logTail")}</h3>
          <pre className="rounded bg-muted p-2 text-xs break-words whitespace-pre-wrap">
            {tail}
          </pre>
        </div>
      )}
      {log !== "" && (
        <p className="text-sm break-all text-muted-foreground">
          {t("task.blocker.log", { path: log })}
        </p>
      )}
      {blocker.code === "hook_failed" && (
        <Link
          to="/settings/$projectId"
          params={{ projectId: task.project_id }}
          className="inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4"
        >
          {t("task.blocker.hookHistory")}
        </Link>
      )}
      <PrimaryAction task={task} specPath={specPath} />
    </Section>
  );
}
