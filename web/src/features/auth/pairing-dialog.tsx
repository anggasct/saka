import { useState } from "react";

import { ApiError } from "@/api/client";
import { useFormat } from "@/features/delivery/format";
import { errorText } from "@/features/delivery/text";
import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogTitle,
} from "@/ui/dialog";

import { createPairing, newIdempotencyKey } from "./queries";
import type { Pairing, PairingPurpose } from "./queries";

const PAIR_COMMAND = "saka auth redeem <code> --name <name>";
type Copy = "idle" | "copied" | "failed";

export function PairButton({
  purpose,
  label,
}: {
  purpose: PairingPurpose;
  label: string;
}) {
  const t = useT();
  const format = useFormat();
  const [issued, setIssued] = useState<Pairing | null>(null);
  const [pending, setPending] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [copy, setCopy] = useState<Copy>("idle");

  const create = async () => {
    setPending(true);
    setMessage(null);
    try {
      setIssued(await createPairing(purpose, newIdempotencyKey()));
      setCopy("idle");
    } catch (error) {
      setMessage(
        error instanceof ApiError
          ? errorText(t, error.code, error.params)
          : t("auth.devices.pairFailed"),
      );
    }
    setPending(false);
  };

  const copyCode = async () => {
    setCopy(
      (await platform.copyText(issued?.code ?? "")) ? "copied" : "failed",
    );
  };

  return (
    <div className="space-y-2">
      <Button
        variant="outline"
        disabled={pending}
        onClick={() => {
          void create();
        }}
      >
        {label}
      </Button>
      {message !== null && (
        <p role="alert" className="text-sm text-danger-foreground">
          {message}
        </p>
      )}
      <Dialog
        open={issued !== null}
        onOpenChange={(open) => {
          if (!open) {
            setIssued(null);
          }
        }}
      >
        <DialogContent>
          <DialogTitle>{t("auth.pairing.title")}</DialogTitle>
          <DialogDescription>
            {purpose === "token"
              ? t("auth.pairing.app", { command: PAIR_COMMAND })
              : t("auth.pairing.browser")}
          </DialogDescription>
          <p className="font-mono text-2xl tracking-widest select-all">
            {issued?.code}
          </p>
          <p className="text-sm">{t("auth.pairing.shown")}</p>
          {issued !== null && (
            <p className="text-sm text-muted-foreground">
              {t("auth.pairing.expires", {
                time: format.relative(issued.expires_at),
              })}
            </p>
          )}
          <p role="status" className="text-sm">
            {copy === "copied"
              ? t("auth.pairing.copied")
              : copy === "failed"
                ? t("auth.pairing.copyFailed")
                : ""}
          </p>
          <div className="flex flex-wrap justify-end gap-2">
            <Button
              variant="outline"
              onClick={() => {
                void copyCode();
              }}
            >
              {t("auth.pairing.copy")}
            </Button>
            <DialogClose asChild>
              <Button>{t("auth.pairing.close")}</Button>
            </DialogClose>
          </div>
        </DialogContent>
      </Dialog>
    </div>
  );
}
