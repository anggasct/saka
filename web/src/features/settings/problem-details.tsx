import { Link } from "@tanstack/react-router";

import { label } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";

import { blockedRepos, issuesOf, issueText } from "./text";

export function ProblemDetails({
  code,
  params,
}: {
  code: string;
  params: Record<string, unknown>;
}) {
  const t = useT();
  if (code === "project_config_invalid") {
    const issues = issuesOf(params);
    return issues.length === 0 ? null : (
      <ul className="list-disc space-y-1 pl-5 text-sm">
        {issues.map((issue) => (
          <li key={`${issue.line}-${issue.code}-${issue.field}`}>
            {issueText(t, issue)}
          </li>
        ))}
      </ul>
    );
  }
  if (code === "project_repo_in_use") {
    const repos = blockedRepos(params);
    return (
      <ul className="space-y-2 text-sm">
        {repos.map((repo) => (
          <li key={repo.name}>
            <span className="font-medium">{repo.name}</span>
            <ul className="list-disc pl-5">
              {repo.tasks.map((task) => (
                <li key={task.id} className="flex flex-wrap gap-x-2">
                  <Link
                    to="/delivery/tasks/$workItemId"
                    params={{ workItemId: task.id }}
                    className="text-primary underline underline-offset-4"
                  >
                    {task.key}
                  </Link>
                  <span>{label(t, "task.state", task.state)}</span>
                </li>
              ))}
            </ul>
          </li>
        ))}
      </ul>
    );
  }
  return null;
}
