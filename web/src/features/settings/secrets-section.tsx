import { useQueryClient } from "@tanstack/react-query";

import { QUERY_ROOTS } from "@/api/events";
import { Section } from "@/features/delivery/section";
import { When } from "@/features/delivery/when";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import { useT } from "@/i18n/use-t";
import { Skeleton } from "@/ui/skeleton";

import { deleteSecret, invalidate, useSecrets } from "./queries";
import { SecretDialog } from "./secret-dialog";

const ENVIRONMENT_FILE = ".saka/environment.toml";

export function SecretsSection({ projectId }: { projectId: string }) {
  const t = useT();
  const client = useQueryClient();
  const secrets = useSecrets(projectId);
  return (
    <Section title={t("settings.secrets.title")}>
      {secrets.error !== null ? (
        <PageError
          title="settings.secrets.error"
          error={secrets.error}
          onRetry={() => void secrets.refetch()}
        />
      ) : secrets.isPending ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("settings.secrets.loading")}
          className="space-y-2"
        >
          <Skeleton className="h-10" />
          <Skeleton className="h-10" />
        </div>
      ) : secrets.data.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.secrets.empty", { file: ENVIRONMENT_FILE })}
        </p>
      ) : (
        <ul className="divide-y divide-border">
          {secrets.data.map((secret) => (
            <li
              key={secret.name}
              className="flex flex-wrap items-center justify-between gap-2 py-2"
            >
              <div className="min-w-0">
                <code className="font-medium break-all">{secret.name}</code>
                <p className="flex gap-1 text-xs text-muted-foreground">
                  <span>{t("settings.secrets.updated")}</span>
                  <When iso={secret.updated_at} />
                </p>
              </div>
              <div className="flex flex-wrap gap-2">
                <SecretDialog
                  projectId={projectId}
                  lockedName={secret.name}
                  trigger={t("settings.secrets.replace", { name: secret.name })}
                />
                <ActionDialog
                  trigger={t("settings.secrets.remove", { name: secret.name })}
                  title={t("settings.secrets.removeTitle", {
                    name: secret.name,
                  })}
                  description={t("settings.secrets.removeBody", {
                    name: secret.name,
                  })}
                  submit={t("settings.secrets.removeConfirm")}
                  onSubmit={async (_reason, key) => {
                    await deleteSecret(projectId, secret.name, key);
                    await invalidate(client, QUERY_ROOTS.secrets);
                  }}
                />
              </div>
            </li>
          ))}
        </ul>
      )}
      <SecretDialog projectId={projectId} trigger={t("settings.secrets.add")} />
    </Section>
  );
}
