import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { Section } from "@/features/delivery/section";
import { record } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";

import { ProblemDetails } from "./problem-details";
import { invalidate, reloadProject } from "./queries";
import type { Project } from "./queries";
import { configErrorText, settingsErrorText, shortSha } from "./text";
import { useKeyedRun } from "./use-keyed-run";

const SHORT = 12;

export function ConfigSection({ project }: { project: Project }) {
  const t = useT();
  const client = useQueryClient();
  const action = useKeyedRun();
  const [outcome, setOutcome] = useState<string | null>(null);
  const failure = action.error;
  const failed = failure instanceof ApiError ? failure : null;

  const reload = async () => {
    setOutcome(null);
    await action.run("reload", async (key) => {
      try {
        const result = await reloadProject(project.id, key);
        setOutcome(result.outcome);
      } finally {
        await invalidate(client, QUERY_ROOTS.projects);
      }
    });
  };

  const rejected = project.config_error;
  return (
    <Section title={t("settings.config.title")}>
      <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">{t("settings.config.commit")}</dt>
        <dd>
          <code>{project.config_commit.slice(0, SHORT)}</code>
        </dd>
        <dt className="text-muted-foreground">{t("settings.config.hash")}</dt>
        <dd>
          <code>{shortSha(project.config_sha256)}</code>
        </dd>
      </dl>
      {rejected !== null && rejected !== undefined && (
        <div
          role="group"
          aria-label={t("settings.config.rejected")}
          className="space-y-2 rounded-md bg-danger p-3 text-danger-foreground"
        >
          <p className="font-medium">{t("settings.config.rejected")}</p>
          <p className="text-sm">
            {configErrorText(t, rejected.code, record(rejected.params))}
          </p>
          <ProblemDetails
            code={rejected.code}
            params={record(rejected.params)}
          />
          <p className="flex flex-wrap gap-1 text-xs">
            <span>
              {t("settings.config.observed", {
                commit: rejected.commit.slice(0, SHORT),
              })}
            </span>
            <When iso={rejected.observed_at} />
          </p>
        </div>
      )}
      <div className="flex flex-wrap items-center gap-3">
        <Button
          variant="outline"
          disabled={action.pending}
          onClick={() => void reload()}
        >
          {action.pending
            ? t("settings.config.reloading")
            : t("settings.config.reload")}
        </Button>
      </div>
      <p role="status" className="text-sm">
        {outcome === null
          ? ""
          : t(
              outcome === "changed"
                ? "settings.config.outcome.changed"
                : "settings.config.outcome.unchanged",
            )}
      </p>
      {failure !== null && (
        <div role="alert" className="space-y-2 text-sm text-danger-foreground">
          <p>
            {settingsErrorText(
              t,
              failed?.code ?? "network_error",
              failed?.params ?? {},
            )}
          </p>
          {failed !== null && (
            <ProblemDetails code={failed.code} params={failed.params} />
          )}
        </div>
      )}
    </Section>
  );
}
