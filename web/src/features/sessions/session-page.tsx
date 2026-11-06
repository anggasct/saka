import { Link } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import { DecisionSection } from "@/features/decisions/decision-section";
import { PageError } from "@/features/delivery/page-error";
import { useTaskPermissions } from "@/features/delivery/queries";
import { Section } from "@/features/delivery/section";
import { label } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Card } from "@/ui/card";
import { Skeleton } from "@/ui/skeleton";

import { PermissionCard } from "./permission-card";
import type { Session } from "./queries";
import { useSession } from "./queries";
import { SessionControls } from "./session-controls";
import { SessionSummary } from "./session-summary";
import { sessionTone } from "./text";
import { Timeline } from "./timeline";

const PAGE = "mx-auto w-full max-w-4xl space-y-4 p-4";
const LINK =
  "inline-flex min-h-11 items-center text-sm font-medium text-primary underline underline-offset-4";

function Environment({ session }: { session: Session }) {
  const t = useT();
  const env = session.environment;
  if (env == null) {
    return null;
  }
  return (
    <Section title={t("task.env.title")}>
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="font-medium">{t("task.env.state")}</dt>
        <dd>{label(t, "env.state", env.setup_state)}</dd>
        <dt className="font-medium">{t("sessions.branch")}</dt>
        <dd className="break-all">{env.branch ?? t("sessions.noBranch")}</dd>
        <dt className="font-medium">{t("task.env.ports")}</dt>
        <dd>
          {env.ports != null
            ? t("task.env.portRange", {
                start: String(env.ports.start),
                end: String(env.ports.start + env.ports.count - 1),
              })
            : t("task.env.noPorts")}
        </dd>
        <dt className="font-medium">{t("task.env.toolchains")}</dt>
        <dd className="break-words">
          {env.toolchains.length > 0
            ? env.toolchains.map((c) => `${c.tool} ${c.version}`).join(", ")
            : t("task.env.noToolchains")}
        </dd>
      </dl>
    </Section>
  );
}

function OwnerLink({ session }: { session: Session }) {
  const t = useT();
  const owner = session.owner;
  if (owner.kind === "stage_run") {
    return (
      <Link
        to="/delivery/tasks/$workItemId"
        params={{ workItemId: owner.work_item_id }}
        className={LINK}
      >
        {t("sessions.openTask")}
      </Link>
    );
  }
  return (
    <Link
      to="/docs/$projectId/drafts/$draftId"
      params={{ projectId: session.project_id, draftId: owner.spec_draft_id }}
      className={LINK}
    >
      {t("sessions.openDraft")}
    </Link>
  );
}

function Loaded({ session }: { session: Session }) {
  const t = useT();
  const owner = session.owner;
  const permissions = useTaskPermissions(
    owner.kind === "stage_run" ? owner.work_item_id : undefined,
  );
  const waiting = permissions.data ?? [];
  const run =
    owner.kind === "stage_run"
      ? ({ kind: "work_item", id: owner.work_item_id } as const)
      : ({ kind: "spec_draft", id: owner.spec_draft_id } as const);
  return (
    <div className={PAGE}>
      <Card className="space-y-2 p-4">
        <Link to="/sessions" className={LINK}>
          {t("sessions.back")}
        </Link>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h1 className="text-2xl font-semibold break-words">
            {session.title}
          </h1>
          <Badge tone={sessionTone(session.state)}>
            {label(t, "session.state", session.state)}
          </Badge>
        </div>
        <p className="text-sm text-muted-foreground">
          {t("sessions.of", {
            project: session.project_name,
            role: label(t, "session.role", session.role),
          })}
          {session.started_at != null && (
            <>
              {" · "}
              <When iso={session.started_at} />
            </>
          )}
        </p>
        <OwnerLink session={session} />
      </Card>
      {waiting.length > 0 && (
        <Section title={t("sessions.waiting")}>
          <ul className="space-y-2">
            {waiting.map((request) => (
              <li key={request.id}>
                <PermissionCard
                  request={request}
                  project={session.project_name}
                />
              </li>
            ))}
          </ul>
        </Section>
      )}
      <DecisionSection run={run} />
      <Section title={t("sessions.controls")}>
        <SessionSummary session={session} />
        <SessionControls session={session} subject={session.title} />
      </Section>
      <Environment session={session} />
      <Section title={t("sessions.timeline")}>
        <Timeline
          sessionId={session.id}
          project={session.project_name}
          permissions={[]}
        />
      </Section>
    </div>
  );
}

export function SessionPage({ sessionId }: { sessionId: string }) {
  const t = useT();
  const session = useSession(sessionId);
  if (session.isPending) {
    return (
      <div
        aria-busy="true"
        aria-label={t("sessions.loadingOne")}
        role="status"
        className={PAGE}
      >
        <Skeleton className="h-24" />
        <Skeleton className="h-48" />
      </div>
    );
  }
  if (session.isError) {
    if (session.error instanceof ApiError && session.error.status === 404) {
      return (
        <div className={PAGE}>
          <Card className="space-y-3 p-4">
            <h1 className="text-xl font-semibold">{t("sessions.notFound")}</h1>
            <Link to="/sessions" className={LINK}>
              {t("sessions.back")}
            </Link>
          </Card>
        </div>
      );
    }
    return (
      <div className={PAGE}>
        <PageError
          title="sessions.error.titleOne"
          error={session.error}
          onRetry={() => {
            void session.refetch();
          }}
        />
      </div>
    );
  }
  return <Loaded session={session.data} />;
}
