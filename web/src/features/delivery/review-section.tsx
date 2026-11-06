import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import type { BadgeTone } from "@/ui/badge";
import { Disclosure } from "@/ui/disclosure";

import type { WorkItemDetail } from "./queries";
import { Section } from "./section";
import { label, sha } from "./text";
import { When } from "./when";

type Review = WorkItemDetail["reviews"][number];

function tone(outcome: string): BadgeTone {
  switch (outcome) {
    case "approved":
      return "success";
    case "changes_requested":
      return "danger";
    default:
      return "attention";
  }
}

function where(path: string, line: number | null | undefined): string {
  return line ? [path, line].join(":") : path;
}

function Verdict({ review }: { review: Review }) {
  const t = useT();
  const posted = review.posted ?? null;
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Badge tone={tone(review.outcome)}>
          {label(t, "task.review.outcome", review.outcome)}
        </Badge>
        {review.basis === "none" && (
          <Badge tone="attention">{t("external.noSpec")}</Badge>
        )}
        <span className="text-sm text-muted-foreground">
          {t("task.review.at", { sha: sha(review.head_sha) })}
        </span>
        {!review.current && <Badge>{t("task.review.notCurrent")}</Badge>}
        <When iso={review.created_at} />
      </div>
      <p className="break-words">{review.summary}</p>
      {review.outcome === "spec_unclear" ? (
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t("task.review.questions")}</h3>
          <ul className="list-disc pl-5">
            {review.questions.map((question) => (
              <li key={question} className="break-words">
                {question}
              </li>
            ))}
          </ul>
        </div>
      ) : (
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t("task.review.findings")}</h3>
          {review.findings.length === 0 ? (
            <p className="text-muted-foreground">
              {t("task.review.noFindings")}
            </p>
          ) : (
            <ul className="space-y-2">
              {review.findings.map((finding, index) => (
                <li
                  key={`${finding.path ?? ""}:${finding.line ?? 0}:${index}`}
                  className="space-y-1"
                >
                  <p className="flex flex-wrap items-center gap-2">
                    <Badge
                      tone={
                        finding.severity === "blocker" ||
                        finding.severity === "major"
                          ? "danger"
                          : "neutral"
                      }
                    >
                      {label(t, "task.review.severity", finding.severity)}
                    </Badge>
                    {finding.path && (
                      <code className="break-all">
                        {where(finding.path, finding.line)}
                      </code>
                    )}
                  </p>
                  <p className="break-words">{finding.body}</p>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
      {posted && (
        <p className="flex flex-wrap items-center gap-x-3 text-sm text-muted-foreground">
          <span>{t("task.review.posted", { event: posted.event })}</span>
          {posted.url && (
            <a
              className="inline-flex min-h-11 items-center text-primary underline underline-offset-4"
              href={posted.url}
              rel="noopener noreferrer"
              target="_blank"
            >
              {t("task.review.view")}
            </a>
          )}
        </p>
      )}
    </div>
  );
}

export function ReviewSection({ task }: { task: WorkItemDetail }) {
  const t = useT();
  const [latest, ...earlier] = task.reviews;
  if (latest === undefined) {
    return null;
  }
  return (
    <Section title={t("task.review.title")}>
      <Verdict review={latest} />
      {earlier.length > 0 && (
        <Disclosure
          summary={t("task.review.earlier", { count: earlier.length })}
        >
          <ul className="space-y-1 pt-2 text-sm">
            {earlier.map((review) => (
              <li key={review.id} className="flex flex-wrap items-center gap-2">
                <code>{sha(review.head_sha)}</code>
                <Badge tone={tone(review.outcome)}>
                  {label(t, "task.review.outcome", review.outcome)}
                </Badge>
                <When iso={review.created_at} />
              </li>
            ))}
          </ul>
        </Disclosure>
      )}
    </Section>
  );
}
