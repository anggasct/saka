import { useT } from "@/i18n/use-t";

import type { WorkItem } from "./queries";
import { Section } from "./section";
import { label, sha } from "./text";
import { When } from "./when";

export function ChangeSection({ task }: { task: WorkItem }) {
  const t = useT();
  const change = task.change_request;
  const docs = task.docs_change_request;
  if (!change) {
    return null;
  }
  return (
    <Section title={t("task.change.title")}>
      <dl className="grid grid-cols-1 gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="font-medium">{t("task.change.link")}</dt>
        <dd>
          <a
            className="inline-flex min-h-11 items-center font-medium text-primary underline underline-offset-4"
            href={change.url}
            rel="noreferrer"
            target="_blank"
          >
            {t("task.change", { number: change.number })}
          </a>
        </dd>
        <dt className="font-medium">{t("task.change.state")}</dt>
        <dd>{label(t, "change.state", change.state)}</dd>
        <dt className="font-medium">{t("task.head")}</dt>
        <dd>
          <code>{sha(change.head_sha)}</code>
        </dd>
        <dt className="font-medium">{t("task.change.base")}</dt>
        <dd className="break-all">
          <code>{change.base_branch}</code>
        </dd>
        {change.merged_at && (
          <>
            <dt className="font-medium">{t("task.change.mergedAt")}</dt>
            <dd>
              <When iso={change.merged_at} />
            </dd>
          </>
        )}
        {change.merge_commit_sha && (
          <>
            <dt className="font-medium">{t("task.change.mergeCommit")}</dt>
            <dd>
              <code>{sha(change.merge_commit_sha)}</code>
            </dd>
          </>
        )}
        {docs && (
          <>
            <dt className="font-medium">{t("task.change.docs")}</dt>
            <dd>
              <a
                className="inline-flex min-h-11 items-center font-medium text-primary underline underline-offset-4"
                href={docs.url}
                rel="noreferrer"
                target="_blank"
              >
                {t("task.change", { number: docs.number })}
              </a>
            </dd>
            <dt className="font-medium">{t("task.change.docsState")}</dt>
            <dd>
              {docs.skipped
                ? t("task.change.docsSkipped")
                : label(t, "change.state", docs.state)}
            </dd>
            {docs.merge_commit_sha && (
              <>
                <dt className="font-medium">
                  {t("task.change.docsMergeCommit")}
                </dt>
                <dd>
                  <code>{sha(docs.merge_commit_sha)}</code>
                </dd>
              </>
            )}
          </>
        )}
      </dl>
    </Section>
  );
}
