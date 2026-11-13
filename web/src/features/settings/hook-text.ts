import { label, number, record, text } from "@/features/delivery/text";
import type { T } from "@/features/delivery/text";
import { CATALOGS } from "@/i18n/messages";
import type { MessageKey } from "@/i18n/messages";

import type { Hook, HookRun } from "./queries";

function known(key: string): key is MessageKey {
  return key in CATALOGS.en;
}

export function hookErrorText(t: T, error: NonNullable<HookRun["error"]>) {
  const key = `hook.error.${error.code}`;
  if (!known(key)) {
    return t("hook.error.unknown", { code: error.code });
  }
  const params = record(error.params);
  return t(key, {
    exit_code: text(params.exit_code),
    timeout_secs: number(params.timeout_secs),
    status: text(params.status),
    name: text(params.name),
    channel: text(params.channel),
  });
}

export function hookSummary(t: T, hook: Hook): string {
  return t("settings.hooks.summary", {
    event: label(t, "hook.event", hook.on),
    mode: label(t, "hook.mode", hook.mode),
    action: label(t, "hook.action", hook.action),
  });
}

export type Detail = { key: string; name: string; value: string };

export function hookDetails(t: T, hook: Hook): Detail[] {
  const { target } = hook;
  const rows: Detail[] = [
    {
      key: "command",
      name: t("settings.hooks.detail.command"),
      value: target.command ?? "",
    },
    {
      key: "cwd",
      name: t("settings.hooks.detail.cwd"),
      value: target.cwd ?? "",
    },
    {
      key: "url",
      name: t("settings.hooks.detail.url"),
      value: target.url ?? "",
    },
    {
      key: "secret",
      name: t("settings.hooks.detail.secret"),
      value: target.secret ?? "",
    },
    {
      key: "secrets",
      name: t("settings.hooks.detail.secrets"),
      value: target.secrets.join(", "),
    },
    {
      key: "channel",
      name: t("settings.hooks.detail.channel"),
      value: target.channel ?? "",
    },
    {
      key: "repos",
      name: t("settings.hooks.detail.repos"),
      value: hook.repos.join(", "),
    },
    {
      key: "branches",
      name: t("settings.hooks.detail.branches"),
      value: hook.branches.join(", "),
    },
    {
      key: "environments",
      name: t("settings.hooks.detail.environments"),
      value: hook.environments.join(", "),
    },
    {
      key: "codes",
      name: t("settings.hooks.detail.codes"),
      value: hook.codes.join(", "),
    },
  ];
  return rows.filter((row) => row.value !== "");
}
