import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useState } from "react";

import { useT } from "@/i18n/use-t";
import { platform } from "@/platform";
import { Button } from "@/ui/button";

import { signOut, useAuthStatus } from "./queries";

const PASSKEY_ACTOR = "operator:passkey:";

export function AuthNav() {
  const t = useT();
  const client = useQueryClient();
  const status = useAuthStatus();
  const [pending, setPending] = useState(false);
  if (status.data?.enabled !== true || !status.data.authenticated) {
    return null;
  }
  const session = status.data.actor?.startsWith(PASSKEY_ACTOR) === true;
  const leave = async () => {
    setPending(true);
    try {
      await signOut();
      client.clear();
      platform.navigateTo("/login");
    } catch {
      setPending(false);
    }
  };
  return (
    <>
      <Link
        to="/settings/devices"
        className="inline-flex min-h-11 items-center text-sm [&.active]:font-semibold [&.active]:underline"
      >
        {t("nav.devices")}
      </Link>
      {session && (
        <Button
          variant="outline"
          disabled={pending}
          onClick={() => {
            void leave();
          }}
        >
          {pending ? t("auth.signingOut") : t("auth.signOut")}
        </Button>
      )}
    </>
  );
}
