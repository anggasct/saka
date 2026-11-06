import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import { fieldParams } from "@/features/delivery/action-dialog";
import { label } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { Button } from "@/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
  DialogTrigger,
} from "@/ui/dialog";
import { Input } from "@/ui/input";

import { addRule, invalidate, previewRule } from "./queries";
import type { NewRule, RulePreview } from "./queries";
import { settingsErrorText } from "./text";
import { useKeyedRun } from "./use-keyed-run";

const COMMAND_MAX = 2000;
const NOTE_MAX = 500;
const SELECT =
  "block min-h-11 w-full rounded-md border border-border bg-background px-2 text-sm";

type Effect = NewRule["effect"];
type Match = NewRule["match"];
const EFFECTS: readonly Effect[] = ["allow", "ask", "deny"];
const MATCHES: readonly Match[] = ["exact", "glob"];

function pick<T extends string>(
  options: readonly T[],
  value: string,
  fallback: T,
): T {
  return options.find((o) => o === value) ?? fallback;
}

function errorMessage(t: ReturnType<typeof useT>, error: unknown): string {
  if (!(error instanceof ApiError)) {
    return settingsErrorText(t, "network_error");
  }
  return settingsErrorText(t, error.code, fieldParams(error.params));
}

function Preview({ preview }: { preview: RulePreview }) {
  const t = useT();
  return (
    <div role="status" className="space-y-1 text-sm">
      <p>
        {t("settings.permissions.preview.summary", {
          changed: preview.changed.length,
          scanned: preview.scanned,
        })}
      </p>
      {preview.changed.length > 0 && (
        <ul className="space-y-1">
          {preview.changed.map((change) => (
            <li
              key={change.permission_request_id}
              className="rounded border border-border p-2"
            >
              <code className="break-all">{change.command ?? change.tool}</code>
              <span className="block text-muted-foreground">
                {t("settings.permissions.preview.change", {
                  before: label(t, "permission.effect", change.before),
                  after: label(t, "permission.effect", change.after),
                })}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function RuleForm({
  projectId,
  onClose,
}: {
  projectId: string;
  onClose: () => void;
}) {
  const t = useT();
  const client = useQueryClient();
  const save = useKeyedRun();
  const [effect, setEffect] = useState<Effect>("allow");
  const [tool, setTool] = useState("Bash");
  const [match, setMatch] = useState<Match>("exact");
  const [command, setCommand] = useState("");
  const [path, setPath] = useState("");
  const [note, setNote] = useState("");
  const [previewed, setPreviewed] = useState<{
    fingerprint: string;
    preview: RulePreview;
  } | null>(null);
  const [checking, setChecking] = useState(false);
  const [previewError, setPreviewError] = useState<unknown>(null);
  const ids = { effect: useId(), tool: useId(), match: useId() };
  const commandId = useId();
  const pathId = useId();
  const noteId = useId();
  const toolHintId = useId();
  const commandHintId = useId();

  const bash = tool.trim() === "Bash";
  const rule: NewRule = {
    effect,
    tool: tool.trim(),
    match,
    command: bash ? command.trim() : null,
    path: !bash && path.trim() !== "" ? path.trim() : null,
    note: note.trim() === "" ? null : note.trim(),
  };
  const fingerprint = JSON.stringify(rule);
  const complete = rule.tool !== "" && (!bash || rule.command !== "");
  const current = previewed?.fingerprint === fingerprint ? previewed : null;

  const check = async () => {
    setChecking(true);
    setPreviewError(null);
    try {
      const preview = await previewRule(projectId, rule);
      setPreviewed({ fingerprint, preview });
    } catch (e) {
      setPreviewed(null);
      setPreviewError(e);
    } finally {
      setChecking(false);
    }
  };

  const submit = async () => {
    const done = await save.run(fingerprint, async (key) => {
      await addRule(projectId, rule, key);
      await invalidate(client, QUERY_ROOTS.permissionRules);
    });
    if (done) {
      onClose();
    }
  };

  const shown = previewError ?? save.error;
  return (
    <form
      className="space-y-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (current !== null && !save.pending) {
          void submit();
        }
      }}
    >
      <div className="space-y-1">
        <label htmlFor={ids.effect} className="text-sm font-medium">
          {t("settings.permissions.form.effect")}
        </label>
        <select
          id={ids.effect}
          className={SELECT}
          value={effect}
          onChange={(event) => {
            setEffect(pick(EFFECTS, event.target.value, "allow"));
          }}
        >
          {EFFECTS.map((e) => (
            <option key={e} value={e}>
              {label(t, "permission.effect", e)}
            </option>
          ))}
        </select>
      </div>
      <div className="space-y-1">
        <label htmlFor={ids.tool} className="text-sm font-medium">
          {t("settings.permissions.form.tool")}
        </label>
        <Input
          id={ids.tool}
          value={tool}
          maxLength={100}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={toolHintId}
          onChange={(event) => {
            setTool(event.target.value);
          }}
        />
        <p id={toolHintId} className="text-xs text-muted-foreground">
          {t("settings.permissions.form.toolHint")}
        </p>
      </div>
      <div className="space-y-1">
        <label htmlFor={ids.match} className="text-sm font-medium">
          {t("settings.permissions.form.match")}
        </label>
        <select
          id={ids.match}
          className={SELECT}
          value={match}
          onChange={(event) => {
            setMatch(pick(MATCHES, event.target.value, "exact"));
          }}
        >
          {MATCHES.map((m) => (
            <option key={m} value={m}>
              {label(t, "permission.match", m)}
            </option>
          ))}
        </select>
      </div>
      {bash ? (
        <div className="space-y-1">
          <label htmlFor={commandId} className="text-sm font-medium">
            {t("settings.permissions.form.command")}
          </label>
          <Input
            id={commandId}
            value={command}
            maxLength={COMMAND_MAX}
            autoComplete="off"
            spellCheck={false}
            aria-describedby={commandHintId}
            onChange={(event) => {
              setCommand(event.target.value);
            }}
          />
          <p id={commandHintId} className="text-xs text-muted-foreground">
            {t("settings.permissions.form.commandHint")}
          </p>
        </div>
      ) : (
        <div className="space-y-1">
          <label htmlFor={pathId} className="text-sm font-medium">
            {t("settings.permissions.form.path")}
          </label>
          <Input
            id={pathId}
            value={path}
            autoComplete="off"
            spellCheck={false}
            onChange={(event) => {
              setPath(event.target.value);
            }}
          />
          <p className="text-xs text-muted-foreground">
            {t("settings.permissions.form.pathHint")}
          </p>
        </div>
      )}
      <div className="space-y-1">
        <label htmlFor={noteId} className="text-sm font-medium">
          {t("settings.permissions.form.note")}
        </label>
        <Input
          id={noteId}
          value={note}
          maxLength={NOTE_MAX}
          autoComplete="off"
          onChange={(event) => {
            setNote(event.target.value);
          }}
        />
      </div>
      {current !== null ? (
        <Preview preview={current.preview} />
      ) : (
        <p className="text-sm text-muted-foreground">
          {t("settings.permissions.form.previewFirst")}
        </p>
      )}
      {shown !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {errorMessage(t, shown)}
        </p>
      )}
      <div className="flex flex-wrap justify-end gap-2">
        <DialogClose asChild>
          <Button type="button" variant="outline">
            {t("dialog.cancel")}
          </Button>
        </DialogClose>
        <Button
          type="button"
          variant="outline"
          disabled={!complete || checking}
          onClick={() => void check()}
        >
          {t("settings.permissions.form.preview")}
        </Button>
        <Button type="submit" disabled={current === null || save.pending}>
          {t("settings.permissions.form.save")}
        </Button>
      </div>
    </form>
  );
}

export function PermissionRuleDialog({ projectId }: { projectId: string }) {
  const t = useT();
  const [open, setOpen] = useState(false);
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="outline">{t("settings.permissions.add")}</Button>
      </DialogTrigger>
      <DialogContent>
        <DialogTitle>{t("settings.permissions.form.title")}</DialogTitle>
        <DialogDescription>
          {t("settings.permissions.form.description")}
        </DialogDescription>
        <RuleForm
          projectId={projectId}
          onClose={() => {
            setOpen(false);
          }}
        />
      </DialogContent>
    </Dialog>
  );
}
