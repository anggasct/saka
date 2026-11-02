import { useT } from "@/i18n/use-t";

import type { WorkItem } from "./queries";
import { Section } from "./section";
import { label } from "./text";
import { When } from "./when";

export function SessionSection({ task }: { task: WorkItem }) {
  const t = useT();
  const session = task.session;
  return (
    <Section title={t("task.session.title")}>
      {!session ? (
        <p className="text-sm text-muted-foreground">
          {t("task.session.none")}
        </p>
      ) : (
        <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
          <dt className="font-medium">{t("task.session.state")}</dt>
          <dd>{label(t, "session.state", session.state)}</dd>
          <dt className="font-medium">{t("task.session.cli")}</dt>
          <dd>{session.cli}</dd>
          {session.started_at && (
            <>
              <dt className="font-medium">{t("task.session.started")}</dt>
              <dd>
                <When iso={session.started_at} />
              </dd>
            </>
          )}
          {session.ended_at && (
            <>
              <dt className="font-medium">{t("task.session.ended")}</dt>
              <dd>
                <When iso={session.ended_at} />
              </dd>
            </>
          )}
        </dl>
      )}
    </Section>
  );
}
