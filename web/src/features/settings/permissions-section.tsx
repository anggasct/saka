import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";

import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Skeleton } from "@/ui/skeleton";

import { answerSourceText } from "./permission-text";
import { PermissionRuleDialog } from "./permission-rule-dialog";
import {
  invalidate,
  revokeRule,
  usePermissionHistory,
  usePermissionRules,
} from "./queries";
import type {
  AnsweredBy,
  OperatorRule,
  PermissionHistoryItem,
  PermissionRule,
} from "./queries";

const EFFECT_TONES: Record<string, BadgeTone> = {
  allow: "success",
  deny: "danger",
  ask: "progress",
};
const STATE_TONES: Record<string, BadgeTone> = {
  allowed: "success",
  denied: "danger",
  pending: "progress",
};
const SOURCES: readonly (AnsweredBy | "all")[] = ["all", "policy", "human"];

function Loading({ name }: { name: string }) {
  return (
    <div role="status" aria-busy="true" aria-label={name} className="space-y-2">
      <Skeleton className="h-10" />
      <Skeleton className="h-10" />
    </div>
  );
}

function OperatorRules({ rules }: { rules: readonly OperatorRule[] }) {
  const t = useT();
  return (
    <div className="space-y-2">
      <h3 className="text-base font-semibold">
        {t("settings.permissions.operator.title")}
      </h3>
      <p className="text-sm text-muted-foreground">
        {t("settings.permissions.operator.note")}
      </p>
      {rules.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.permissions.operator.empty")}
        </p>
      ) : (
        <ul className="divide-y divide-border">
          {rules.map((rule) => (
            <li key={rule.id} className="space-y-1 py-2">
              <p className="flex min-w-0 flex-wrap items-center gap-2">
                <Badge tone={EFFECT_TONES[rule.effect] ?? "neutral"}>
                  {label(t, "permission.effect", rule.effect)}
                </Badge>
                <code className="font-medium break-all">{rule.id}</code>
                <span className="text-sm break-all text-muted-foreground">
                  {rule.tools.join(", ")}
                </span>
              </p>
              {[...rule.commands, ...rule.paths].length > 0 && (
                <p className="text-sm break-all">
                  <code>{[...rule.commands, ...rule.paths].join("  ")}</code>
                </p>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function ruleTarget(rule: PermissionRule): string {
  return rule.command ?? rule.path ?? rule.tool;
}

function ProjectRule({ rule }: { rule: PermissionRule }) {
  const t = useT();
  const client = useQueryClient();
  return (
    <li className="space-y-1 py-2">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="flex min-w-0 flex-wrap items-center gap-2">
          <Badge tone={EFFECT_TONES[rule.effect] ?? "neutral"}>
            {label(t, "permission.effect", rule.effect)}
          </Badge>
          <Badge>{label(t, "permission.match", rule.match)}</Badge>
          <span className="text-sm text-muted-foreground">{rule.tool}</span>
          <code className="font-medium break-all">{ruleTarget(rule)}</code>
        </p>
        <ActionDialog
          trigger={t("settings.permissions.revoke", {
            rule: ruleTarget(rule),
          })}
          title={t("settings.permissions.revokeTitle")}
          description={t("settings.permissions.revokeBody", {
            rule: ruleTarget(rule),
          })}
          reason={{ label: t("dialog.reason"), required: true }}
          submit={t("settings.permissions.revokeSubmit")}
          onSubmit={async (reason, key) => {
            await revokeRule(rule.id, reason ?? "", key);
            await invalidate(client, QUERY_ROOTS.permissionRules);
          }}
        />
      </div>
      {rule.note !== null && rule.note !== undefined && (
        <p className="text-sm break-words">{rule.note}</p>
      )}
      <p className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
        <span>{rule.created_by}</span>
        <When iso={rule.created_at} />
      </p>
    </li>
  );
}

function HistoryRow({ item }: { item: PermissionHistoryItem }) {
  const t = useT();
  return (
    <li className="space-y-1 py-2">
      <p className="flex min-w-0 flex-wrap items-center gap-2">
        <Badge tone={STATE_TONES[item.state] ?? "neutral"}>
          {label(t, "permission.state", item.state)}
        </Badge>
        <code className="font-medium break-all">{item.summary}</code>
      </p>
      <p className="flex flex-wrap gap-x-2 text-xs text-muted-foreground">
        {item.answer_source !== null && item.answer_source !== undefined && (
          <span>{answerSourceText(t, item.answer_source)}</span>
        )}
        <When iso={item.requested_at} />
      </p>
    </li>
  );
}

function History({ projectId }: { projectId: string }) {
  const t = useT();
  const [source, setSource] = useState<AnsweredBy | "all">("all");
  const history = usePermissionHistory(projectId, source);
  const items = history.data?.pages.flatMap((p) => p.items) ?? [];
  return (
    <div className="space-y-2">
      <h3 className="text-base font-semibold">
        {t("settings.permissions.history.title")}
      </h3>
      <div
        role="group"
        aria-label={t("settings.permissions.history.filter")}
        className="flex flex-wrap gap-2"
      >
        {SOURCES.map((s) => (
          <Button
            key={s}
            variant={source === s ? "default" : "outline"}
            aria-pressed={source === s}
            onClick={() => {
              setSource(s);
            }}
          >
            {t(`settings.permissions.history.source.${s}`)}
          </Button>
        ))}
      </div>
      {history.error !== null ? (
        <PageError
          title="settings.permissions.history.error"
          error={history.error}
          onRetry={() => void history.refetch()}
        />
      ) : history.isPending ? (
        <Loading name={t("settings.permissions.history.loading")} />
      ) : items.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.permissions.history.empty")}
        </p>
      ) : (
        <ul className="divide-y divide-border">
          {items.map((item) => (
            <HistoryRow key={item.id} item={item} />
          ))}
        </ul>
      )}
      {history.hasNextPage && (
        <Button
          variant="outline"
          disabled={history.isFetchingNextPage}
          onClick={() => void history.fetchNextPage()}
        >
          {t("settings.permissions.history.more")}
        </Button>
      )}
    </div>
  );
}

export function PermissionsSection({ projectId }: { projectId: string }) {
  const t = useT();
  const rules = usePermissionRules(projectId);
  return (
    <Section title={t("settings.permissions.title")}>
      <p className="text-sm text-muted-foreground">
        {t("settings.permissions.intro")}
      </p>
      {rules.error !== null ? (
        <PageError
          title="settings.permissions.error"
          error={rules.error}
          onRetry={() => void rules.refetch()}
        />
      ) : rules.isPending ? (
        <Loading name={t("settings.permissions.loading")} />
      ) : (
        <>
          <OperatorRules rules={rules.data.operator_rules} />
          <div className="space-y-2">
            <h3 className="text-base font-semibold">
              {t("settings.permissions.project.title")}
            </h3>
            {rules.data.items.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("settings.permissions.project.empty")}
              </p>
            ) : (
              <ul className="divide-y divide-border">
                {rules.data.items.map((rule) => (
                  <ProjectRule key={rule.id} rule={rule} />
                ))}
              </ul>
            )}
            <PermissionRuleDialog projectId={projectId} />
          </div>
        </>
      )}
      <History projectId={projectId} />
    </Section>
  );
}
