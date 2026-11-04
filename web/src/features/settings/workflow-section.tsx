import { label } from "@/features/delivery/text";
import { Section } from "@/features/delivery/section";
import { useT } from "@/i18n/use-t";

import type { Project } from "./queries";

export function WorkflowSection({ project }: { project: Project }) {
  const t = useT();
  return (
    <Section title={t("settings.workflow.title")}>
      <dl className="grid gap-x-4 gap-y-2 text-sm sm:grid-cols-[max-content_1fr]">
        <dt className="text-muted-foreground">
          {t("settings.workflow.preset")}
        </dt>
        <dd>{label(t, "workflowPreset", project.workflow_preset)}</dd>
        <dt className="text-muted-foreground">
          {t("settings.workflow.autonomy")}
        </dt>
        <dd>{label(t, "autonomy", project.autonomy)}</dd>
      </dl>
      <p className="text-sm text-muted-foreground">
        {t("settings.workflow.single")}
      </p>
    </Section>
  );
}
