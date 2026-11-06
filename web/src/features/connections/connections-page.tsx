import { useEffect, useId, useState } from "react";

import { useT } from "@/i18n/use-t";
import type { MessageKey } from "@/i18n/messages";
import { platform } from "@/platform";
import type { SavedConnection } from "@/platform";
import { ConnectionError } from "@/platform/connection-error";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Input } from "@/ui/input";

export type ConnectionsMode = "setup" | "manage" | "reauth";

const TITLES: Record<ConnectionsMode, MessageKey> = {
  setup: "connections.setup.title",
  manage: "connections.title",
  reauth: "connections.reauth.title",
};

const INTROS: Record<ConnectionsMode, MessageKey> = {
  setup: "connections.setup.intro",
  manage: "connections.intro",
  reauth: "connections.reauth.intro",
};

const ERRORS: Record<ConnectionError["code"], MessageKey> = {
  invalid_url: "connections.error.invalid_url",
  insecure_url: "connections.error.insecure_url",
  invalid_name: "connections.error.invalid_name",
  pairing_code_invalid: "connections.error.pairing_code_invalid",
  rate_limited: "connections.error.rate_limited",
  pairing_unavailable: "connections.error.pairing_unavailable",
  server_unreachable: "connections.error.server_unreachable",
  server_refused: "connections.error.server_refused",
  keychain_unavailable: "connections.error.keychain_unavailable",
  not_found: "connections.error.not_found",
  store_failed: "connections.error.store_failed",
  unknown: "connections.error.unknown",
};

function failureOf(error: unknown): ConnectionError["code"] {
  return error instanceof ConnectionError ? error.code : "unknown";
}

export function ConnectionsPage({ mode }: { mode: ConnectionsMode }) {
  const t = useT();
  const shell = platform.connections();
  const nameId = useId();
  const urlId = useId();
  const urlHintId = useId();
  const codeId = useId();
  const [items, setItems] = useState<SavedConnection[] | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [code, setCode] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<ConnectionError["code"] | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);

  useEffect(() => {
    if (shell === null) {
      return;
    }
    void shell.list().then((list) => {
      setItems(list);
      const active = list.find((c) => c.active);
      if (mode === "reauth" && active !== undefined) {
        setName(active.name);
        setUrl(active.url);
      }
    });
  }, [shell, mode]);

  if (shell === null) {
    return null;
  }

  const reload = async () => {
    setItems(await shell.list());
  };

  const act = async (work: () => Promise<void>) => {
    setFailure(null);
    try {
      await work();
    } catch (error) {
      setFailure(failureOf(error));
    }
  };

  const submit = async () => {
    setPending(true);
    await act(async () => {
      const saved = await shell.add({
        name: name.trim(),
        url: url.trim(),
        pairingCode: code.trim(),
      });
      setCode("");
      if (saved.active) {
        await shell.select(saved.id);
      } else {
        await reload();
      }
    });
    setPending(false);
  };

  const ready = name.trim() !== "" && url.trim() !== "" && code.trim() !== "";
  return (
    <div className="mx-auto w-full max-w-xl space-y-4 p-4">
      <h1 className="text-2xl font-semibold">{t(TITLES[mode])}</h1>
      <p className="text-sm text-muted-foreground">{t(INTROS[mode])}</p>
      {failure !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {t(ERRORS[failure])}
        </p>
      )}
      {items !== null && items.length > 0 && (
        <Card className="space-y-3 p-4">
          <h2 className="text-lg font-semibold">
            {t("connections.saved.title")}
          </h2>
          <ul className="space-y-3">
            {items.map((item) => (
              <li
                key={item.id}
                className="flex flex-wrap items-center justify-between gap-2"
              >
                <div className="min-w-0">
                  <p className="flex flex-wrap items-center gap-2 font-medium">
                    {item.name}
                    {item.active && (
                      <Badge tone="success">{t("connections.active")}</Badge>
                    )}
                  </p>
                  <p className="text-sm break-all text-muted-foreground">
                    {item.url}
                  </p>
                </div>
                <div className="flex gap-2">
                  {!item.active && (
                    <Button
                      variant="outline"
                      aria-label={t("connections.use.label", {
                        name: item.name,
                      })}
                      onClick={() => {
                        void act(() => shell.select(item.id));
                      }}
                    >
                      {t("connections.use")}
                    </Button>
                  )}
                  <Button
                    variant="outline"
                    aria-label={t(
                      removing === item.id
                        ? "connections.remove.confirmLabel"
                        : "connections.remove.label",
                      { name: item.name },
                    )}
                    onClick={() => {
                      if (removing !== item.id) {
                        setRemoving(item.id);
                        return;
                      }
                      setRemoving(null);
                      void act(async () => {
                        await shell.remove(item.id);
                        await reload();
                      });
                    }}
                  >
                    {t(
                      removing === item.id
                        ? "connections.remove.confirm"
                        : "connections.remove",
                    )}
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </Card>
      )}
      <Card className="space-y-3 p-4">
        <h2 className="text-lg font-semibold">{t("connections.add.title")}</h2>
        <form
          className="space-y-3"
          onSubmit={(event) => {
            event.preventDefault();
            if (ready && !pending) {
              void submit();
            }
          }}
        >
          <div className="space-y-1">
            <label htmlFor={nameId} className="text-sm font-medium">
              {t("connections.add.name")}
            </label>
            <Input
              id={nameId}
              value={name}
              maxLength={100}
              autoComplete="off"
              onChange={(event) => {
                setName(event.target.value);
              }}
            />
          </div>
          <div className="space-y-1">
            <label htmlFor={urlId} className="text-sm font-medium">
              {t("connections.add.url")}
            </label>
            <Input
              id={urlId}
              value={url}
              inputMode="url"
              autoComplete="off"
              spellCheck={false}
              aria-describedby={urlHintId}
              onChange={(event) => {
                setUrl(event.target.value);
              }}
            />
            <p id={urlHintId} className="text-xs text-muted-foreground">
              {t("connections.add.urlHint")}
            </p>
          </div>
          <div className="space-y-1">
            <label htmlFor={codeId} className="text-sm font-medium">
              {t("connections.add.code")}
            </label>
            <Input
              id={codeId}
              value={code}
              autoComplete="off"
              autoCapitalize="characters"
              spellCheck={false}
              className="font-mono tracking-widest uppercase"
              onChange={(event) => {
                setCode(event.target.value);
              }}
            />
          </div>
          <Button type="submit" disabled={!ready || pending}>
            {pending
              ? t("connections.add.pending")
              : t("connections.add.submit")}
          </Button>
        </form>
      </Card>
    </div>
  );
}
