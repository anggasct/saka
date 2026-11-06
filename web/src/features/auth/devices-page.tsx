import { useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";

import { ApiError } from "@/api/client";
import { ActionDialog, fieldParams } from "@/features/delivery/action-dialog";
import { PageError } from "@/features/delivery/page-error";
import { Section } from "@/features/delivery/section";
import { errorText } from "@/features/delivery/text";
import { When } from "@/features/delivery/when";
import { useT } from "@/i18n/use-t";
import { Badge } from "@/ui/badge";
import { Button } from "@/ui/button";
import { Card } from "@/ui/card";
import { Input } from "@/ui/input";
import { Skeleton } from "@/ui/skeleton";

import { PairButton } from "./pairing-dialog";
import {
  invalidateAuth,
  registerPasskey,
  revokeDevice,
  revokePasskey,
  useAuthDevices,
  useAuthStatus,
} from "./queries";
import type { Device, Passkey } from "./queries";

function LastUsed({ iso }: { iso: string | null | undefined }) {
  const t = useT();
  return (
    <span className="flex gap-1">
      <span>{t("auth.devices.lastUsed")}</span>
      {iso === null || iso === undefined ? (
        <span>{t("auth.devices.neverUsed")}</span>
      ) : (
        <When iso={iso} />
      )}
    </span>
  );
}

function AddPasskey() {
  const t = useT();
  const client = useQueryClient();
  const nameId = useId();
  const [name, setName] = useState("");
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const submit = async () => {
    setPending(true);
    setMessage(null);
    setDone(false);
    try {
      const outcome = await registerPasskey(name.trim(), null);
      if (outcome === "ok") {
        setName("");
        setDone(true);
        await invalidateAuth(client);
      } else {
        setMessage(
          outcome === "unsupported"
            ? t("auth.setup.unsupported")
            : outcome === "cancelled"
              ? t("auth.setup.cancelled")
              : outcome === "duplicate"
                ? t("auth.setup.duplicate")
                : t("auth.setup.failed"),
        );
      }
    } catch (error) {
      setMessage(
        error instanceof ApiError
          ? errorText(t, error.code, fieldParams(error.params))
          : t("auth.setup.failed"),
      );
    }
    setPending(false);
  };

  return (
    <form
      className="space-y-2"
      onSubmit={(event) => {
        event.preventDefault();
        if (name.trim() !== "" && !pending) {
          void submit();
        }
      }}
    >
      <label htmlFor={nameId} className="text-sm font-medium">
        {t("auth.devices.addName")}
      </label>
      <div className="flex flex-wrap gap-2">
        <Input
          id={nameId}
          value={name}
          onChange={(event) => {
            setName(event.target.value);
          }}
          maxLength={100}
          autoComplete="off"
          className="max-w-xs"
        />
        <Button type="submit" disabled={name.trim() === "" || pending}>
          {pending ? t("auth.devices.addPending") : t("auth.devices.addSubmit")}
        </Button>
      </div>
      <p role="status" className="text-sm">
        {done ? t("auth.devices.addDone") : ""}
      </p>
      {message !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {message}
        </p>
      )}
    </form>
  );
}

function PasskeyRow({ passkey }: { passkey: Passkey }) {
  const t = useT();
  const client = useQueryClient();
  return (
    <li className="flex flex-wrap items-center justify-between gap-2 py-2">
      <div className="min-w-0">
        <span className="font-medium break-words">{passkey.name}</span>
        <p className="text-xs text-muted-foreground">
          <LastUsed iso={passkey.last_used_at} />
        </p>
      </div>
      <ActionDialog
        trigger={t("auth.devices.revoke", { name: passkey.name })}
        title={t("auth.devices.revokeTitle", { name: passkey.name })}
        description={t("auth.devices.revokePasskeyBody")}
        submit={t("auth.devices.revokeConfirm")}
        onSubmit={async (_reason, key) => {
          await revokePasskey(passkey.id, key);
          await invalidateAuth(client);
        }}
      />
    </li>
  );
}

function DeviceRow({ device }: { device: Device }) {
  const t = useT();
  const client = useQueryClient();
  const local = device.kind === "local_cli";
  return (
    <li className="flex flex-wrap items-center justify-between gap-2 py-2">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <span className="font-medium break-words">{device.name}</span>
          <Badge tone="neutral">
            {local ? t("auth.devices.tokens") : t("auth.devices.kindClient")}
          </Badge>
        </div>
        <p className="text-xs text-muted-foreground">
          {local ? (
            t("auth.devices.local")
          ) : (
            <LastUsed iso={device.last_used_at} />
          )}
        </p>
      </div>
      {!local && (
        <ActionDialog
          trigger={t("auth.devices.revoke", { name: device.name })}
          title={t("auth.devices.revokeTitle", { name: device.name })}
          description={t("auth.devices.revokeDeviceBody")}
          submit={t("auth.devices.revokeConfirm")}
          onSubmit={async (_reason, key) => {
            await revokeDevice(device.id, key);
            await invalidateAuth(client);
          }}
        />
      )}
    </li>
  );
}

export function DevicesPage() {
  const t = useT();
  const status = useAuthStatus();
  const devices = useAuthDevices();
  const off = status.data !== undefined && !status.data.enabled;
  return (
    <div className="mx-auto w-full max-w-4xl space-y-4 p-4">
      <h1 className="text-2xl font-semibold">{t("auth.devices.title")}</h1>
      {off ? (
        <Card className="p-4">
          <p className="text-sm">{t("auth.devices.off")}</p>
        </Card>
      ) : devices.error !== null ? (
        <PageError
          title="auth.devices.error"
          error={devices.error}
          onRetry={() => void devices.refetch()}
        />
      ) : devices.isPending ? (
        <div
          role="status"
          aria-busy="true"
          aria-label={t("auth.devices.loading")}
          className="space-y-2"
        >
          <Skeleton className="h-24" />
          <Skeleton className="h-24" />
        </div>
      ) : (
        <>
          <Section title={t("auth.devices.passkeys")}>
            {devices.data.credentials.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("auth.devices.passkeysEmpty")}
              </p>
            ) : (
              <ul className="divide-y divide-border">
                {devices.data.credentials.map((passkey) => (
                  <PasskeyRow key={passkey.id} passkey={passkey} />
                ))}
              </ul>
            )}
            <AddPasskey />
            <PairButton
              purpose="passkey"
              label={t("auth.devices.pairBrowser")}
            />
          </Section>
          <Section title={t("auth.devices.tokens")}>
            {devices.data.devices.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                {t("auth.devices.tokensEmpty")}
              </p>
            ) : (
              <ul className="divide-y divide-border">
                {devices.data.devices.map((device) => (
                  <DeviceRow key={device.id} device={device} />
                ))}
              </ul>
            )}
            <PairButton purpose="token" label={t("auth.devices.pairApp")} />
          </Section>
        </>
      )}
    </div>
  );
}
