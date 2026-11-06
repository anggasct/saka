import { label, record } from "@/features/delivery/text";
import { Section } from "@/features/delivery/section";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Disclosure } from "@/ui/disclosure";

import type { Project, ProjectRepo } from "./queries";
import { configPath, warningText } from "./text";

export function Scope({ globs }: { globs: readonly string[] }) {
  return (
    <ul className="flex flex-wrap gap-1">
      {globs.map((glob) => (
        <li key={glob}>
          <code className="rounded bg-muted px-1 text-xs break-all">
            {glob}
          </code>
        </li>
      ))}
    </ul>
  );
}

function sameScope(a: readonly string[], b: readonly string[]): boolean {
  return a.length === b.length && a.every((glob, i) => glob === b[i]);
}

function Binding({ repo }: { repo: ProjectRepo }) {
  const t = useT();
  const { effective, requested: asked } = repo.autonomy;
  return (
    <li className="space-y-2 rounded-lg border border-border p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium break-words">{repo.name}</span>
        {repo.is_default && <Badge>{t("settings.repos.default")}</Badge>}
        {repo.is_workspace && <Badge>{t("settings.repos.docsRepo")}</Badge>}
      </div>
      <dl className="grid gap-x-4 gap-y-1 sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.repos.repository")}
        </dt>
        <dd className="break-words">
          <a
            href={repo.url}
            target="_blank"
            rel="noopener noreferrer"
            className="text-primary underline underline-offset-4"
          >
            {repo.full_name}
          </a>
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.repos.integration")}
        </dt>
        <dd className="break-words">{repo.integration_id}</dd>
        <dt className="text-muted-foreground">
          {t("settings.repos.baseBranch")}
        </dt>
        <dd className="break-words">{repo.base_branch}</dd>
        <dt className="text-muted-foreground">{t("settings.repos.scope")}</dt>
        <dd>
          <Scope globs={repo.path_scope} />
        </dd>
        <dt className="text-muted-foreground">
          {t("settings.repos.autonomy")}
        </dt>
        <dd className="break-words">
          {label(t, "autonomy", effective)}
          {asked !== undefined && asked !== null && asked !== effective && (
            <span className="block text-muted-foreground">
              {t("settings.repos.autonomyClipped", {
                requested: label(t, "autonomy", asked),
                allowed: label(t, "autonomy", repo.autonomy.allowed),
              })}
            </span>
          )}
        </dd>
      </dl>
      {!sameScope(repo.path_scope, repo.effective_path_scope) && (
        <Disclosure summary={t("settings.repos.effectiveScope")}>
          <Scope globs={repo.effective_path_scope} />
        </Disclosure>
      )}
    </li>
  );
}

export function ReposSection({ project }: { project: Project }) {
  const t = useT();
  const { workspace } = project;
  const file = configPath(workspace.project_path);
  return (
    <Section title={t("settings.repos.title")}>
      <dl className="grid gap-x-4 gap-y-1 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">{t("settings.repos.docs")}</dt>
        <dd className="break-words">{workspace.full_name}</dd>
        <dt className="text-muted-foreground">{t("settings.repos.folder")}</dt>
        <dd className="break-words">
          <code>{workspace.project_path}</code>
        </dd>
        <dt className="text-muted-foreground">{t("settings.repos.branch")}</dt>
        <dd className="break-words">{workspace.base_branch}</dd>
        <dt className="text-muted-foreground">{t("settings.repos.file")}</dt>
        <dd className="break-words">
          <code>{file}</code>
        </dd>
      </dl>
      <p className="text-sm text-muted-foreground">
        {t("settings.repos.source", {
          path: file,
          branch: workspace.base_branch,
        })}
      </p>
      {project.warnings.length > 0 && (
        <ul className="space-y-1">
          {project.warnings.map((warning) => (
            <li
              key={`${warning.code}-${JSON.stringify(warning.params)}`}
              className="rounded-md bg-attention p-2 text-sm text-attention-foreground"
            >
              {warningText(t, warning.code, record(warning.params))}
            </li>
          ))}
        </ul>
      )}
      {project.repos.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          {t("settings.repos.empty")}
        </p>
      ) : (
        <ul className="space-y-2">
          {project.repos.map((repo) => (
            <Binding key={repo.repo_id} repo={repo} />
          ))}
        </ul>
      )}
    </Section>
  );
}
