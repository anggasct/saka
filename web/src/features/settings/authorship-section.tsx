import { useQueryClient } from "@tanstack/react-query";
import { useCallback, useId, useState } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { ActionDialog } from "@/features/delivery/action-dialog";
import { useFormat } from "@/features/delivery/format";
import { PageError, codeOf } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { RadioGroup } from "@/ui/radio-group";
import { Skeleton } from "@/ui/skeleton";

import { DevicePanel } from "./device-link";
import {
  deleteUserToken,
  invalidate,
  putAuthorship,
  removeUserLink,
  startAuthorization,
  useAuthorship,
  useUserLink,
} from "./queries";
import type { Authorization, Authorship, UserLink } from "./queries";
import { expiresSoon, settingsErrorText } from "./text";
import { UserTokenDialog } from "./user-token-dialog";
import { useKeyedRun } from "./use-keyed-run";

const MODES = ["bot", "user", "user_pat"] as const;

function errorSentence(t: ReturnType<typeof useT>, error: unknown): string {
  return settingsErrorText(
    t,
    codeOf(error),
    error instanceof ApiError ? error.params : {},
  );
}

function ModeForm({ authorship }: { authorship: Authorship }) {
  const t = useT();
  const client = useQueryClient();
  const action = useKeyedRun();
  const [mode, setMode] = useState(authorship.mode);
  const name = useId();
  const same = mode === authorship.mode;

  const save = async () => {
    await action.run(mode, async (key) => {
      await putAuthorship(authorship.project_id, mode, key);
      await invalidate(client, QUERY_ROOTS.authorship);
    });
  };

  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (!same && !action.pending) {
          void save();
        }
      }}
    >
      <RadioGroup
        legend={t("settings.author.mode")}
        name={name}
        value={mode}
        options={MODES.map((value) => ({
          value,
          label: t(`settings.author.modes.${value}`),
          description: t(`settings.author.modeHelp.${value}`),
        }))}
        onChange={setMode}
      />
      {action.error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {errorSentence(t, action.error)}
        </p>
      )}
      <Button type="submit" disabled={same || action.pending}>
        {t("settings.author.save")}
      </Button>
    </form>
  );
}

function AccountDetails({ link }: { link: UserLink }) {
  const t = useT();
  const format = useFormat();
  return (
    <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
      <dt className="text-muted-foreground">{t("settings.account.login")}</dt>
      <dd>
        <code>{link.login}</code>
        {link.name !== null && link.name !== undefined && (
          <span className="ml-2">{link.name}</span>
        )}
      </dd>
      <dt className="text-muted-foreground">{t("settings.account.state")}</dt>
      <dd className="flex flex-wrap gap-2">
        <Badge tone={link.state === "active" ? "success" : "attention"}>
          {label(t, "settings.linkState", link.state)}
        </Badge>
        {link.relink_cause !== null && link.relink_cause !== undefined && (
          <span>{label(t, "settings.relinkCause", link.relink_cause)}</span>
        )}
      </dd>
      <dt className="text-muted-foreground">
        {t("settings.account.refreshExpires")}
      </dt>
      <dd className="flex flex-wrap gap-2">
        <When iso={link.refresh_expires_at} />
        {expiresSoon(link.refresh_expires_at, format.now) && (
          <Badge tone="attention">{t("settings.expiresSoon")}</Badge>
        )}
      </dd>
    </dl>
  );
}

function Account({ integrationId }: { integrationId: string }) {
  const t = useT();
  const client = useQueryClient();
  const link = useUserLink(integrationId);
  const start = useKeyedRun();
  const [authorization, setAuthorization] = useState<Authorization | null>(
    null,
  );
  const [linked, setLinked] = useState<string | null>(null);

  const begin = async () => {
    setLinked(null);
    setAuthorization(null);
    await start.run(integrationId, async (key) => {
      setAuthorization(await startAuthorization(integrationId, key));
    });
  };
  const done = useCallback((login: string) => {
    setAuthorization(null);
    setLinked(login);
  }, []);

  const needsRelink = link.data?.state === "needs_relink";
  return (
    <li className="space-y-2 rounded-md border border-border p-3">
      <h4 className="font-medium break-all">
        {t("settings.account.integration", { id: integrationId })}
      </h4>
      {link.error !== null ? (
        <PageError
          title="settings.account.error"
          error={link.error}
          onRetry={() => void link.refetch()}
        />
      ) : link.isPending ? (
        <Skeleton className="h-10" />
      ) : link.data === null ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.account.none")}
        </p>
      ) : (
        <AccountDetails link={link.data} />
      )}
      <p role="status" className="text-sm">
        {linked === null ? "" : t("settings.link.linked", { login: linked })}
      </p>
      {authorization !== null && (
        <DevicePanel
          key={authorization.id}
          initial={authorization}
          onCompleted={done}
          onRetry={() => void begin()}
          retrying={start.pending}
        />
      )}
      {start.error !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {errorSentence(t, start.error)}
        </p>
      )}
      <div className="flex flex-wrap gap-2">
        {link.data !== undefined && (
          <Button
            variant="outline"
            disabled={start.pending}
            onClick={() => void begin()}
          >
            {link.data === null
              ? t("settings.account.link")
              : needsRelink
                ? t("settings.account.relink")
                : t("settings.account.linkAgain")}
          </Button>
        )}
        {link.data !== undefined && link.data !== null && (
          <ActionDialog
            trigger={t("settings.account.unlink")}
            title={t("settings.account.unlinkTitle", { id: integrationId })}
            description={t("settings.account.unlinkBody")}
            submit={t("settings.account.unlinkConfirm")}
            onSubmit={async (_reason, key) => {
              await removeUserLink(integrationId, key);
              await Promise.all([
                invalidate(client, QUERY_ROOTS.userLinks),
                invalidate(client, QUERY_ROOTS.authorship),
              ]);
            }}
          />
        )}
      </div>
    </li>
  );
}

function TokenPanel({ authorship }: { authorship: Authorship }) {
  const t = useT();
  const client = useQueryClient();
  const format = useFormat();
  const token = authorship.user_token;
  return (
    <div className="space-y-2">
      <h3 className="font-medium">{t("settings.token.heading")}</h3>
      {token === null || token === undefined ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.token.none")}
        </p>
      ) : (
        <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="text-muted-foreground">
            {t("settings.account.login")}
          </dt>
          <dd>
            <code>{token.login}</code>
            <span className="ml-2 text-muted-foreground">
              {token.integration_id}
            </span>
          </dd>
          <dt className="text-muted-foreground">
            {t("settings.account.state")}
          </dt>
          <dd className="flex flex-wrap gap-2">
            <Badge tone={token.state === "active" ? "success" : "danger"}>
              {label(t, "settings.tokenState", token.state)}
            </Badge>
            {token.invalid_cause !== null &&
              token.invalid_cause !== undefined && (
                <span>
                  {label(t, "settings.tokenCause", token.invalid_cause)}
                </span>
              )}
          </dd>
          <dt className="text-muted-foreground">
            {t("settings.token.expires")}
          </dt>
          <dd className="flex flex-wrap gap-2">
            {token.expires_at === null || token.expires_at === undefined ? (
              <span>{t("settings.token.noExpiry")}</span>
            ) : (
              <>
                <When iso={token.expires_at} />
                {expiresSoon(token.expires_at, format.now) && (
                  <Badge tone="attention">{t("settings.expiresSoon")}</Badge>
                )}
              </>
            )}
          </dd>
          <dt className="text-muted-foreground">{t("settings.token.setAt")}</dt>
          <dd>
            <When iso={token.set_at} />
          </dd>
        </dl>
      )}
      <div className="flex flex-wrap gap-2">
        <UserTokenDialog
          projectId={authorship.project_id}
          replacing={token !== null && token !== undefined}
        />
        {token !== null && token !== undefined && (
          <ActionDialog
            trigger={t("settings.token.remove")}
            title={t("settings.token.removeTitle")}
            description={t("settings.token.removeBody")}
            submit={t("settings.token.removeConfirm")}
            onSubmit={async (_reason, key) => {
              await deleteUserToken(authorship.project_id, key);
              await invalidate(client, QUERY_ROOTS.authorship);
            }}
          />
        )}
      </div>
    </div>
  );
}

export function AuthorshipSection({ projectId }: { projectId: string }) {
  const t = useT();
  const authorship = useAuthorship(projectId);
  const data = authorship.data;
  return (
    <Section title={t("settings.author.title")}>
      {authorship.error !== null && data === undefined ? (
        <PageError
          title="settings.author.error"
          error={authorship.error}
          onRetry={() => void authorship.refetch()}
        />
      ) : data === undefined ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("settings.author.loading")}
          className="space-y-2"
        >
          <Skeleton className="h-10" />
          <Skeleton className="h-10" />
        </div>
      ) : (
        <>
          <ModeForm key={data.mode} authorship={data} />
          <p className="text-sm">
            {data.author === null || data.author === undefined
              ? t("settings.author.noAuthor")
              : t("settings.author.next", {
                  name: data.author.name,
                  email: data.author.email,
                  source: label(
                    t,
                    "settings.author.source",
                    data.author.source,
                  ),
                })}
          </p>
          <h3 className="font-medium">{t("settings.account.heading")}</h3>
          {data.integrations.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {t("settings.account.noIntegrations")}
            </p>
          ) : (
            <ul className="space-y-2">
              {data.integrations.map((integration) => (
                <Account
                  key={integration.integration_id}
                  integrationId={integration.integration_id}
                />
              ))}
            </ul>
          )}
          <TokenPanel authorship={data} />
        </>
      )}
    </Section>
  );
}
