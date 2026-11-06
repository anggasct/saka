import { useQuery } from "@tanstack/react-query";
import type { QueryClient } from "@tanstack/react-query";

import { api, newIdempotencyKey, toApiError, unwrap } from "@/api/client";
import { QUERY_ROOTS } from "@/api/events";
import type { components } from "@/api/schema";
import { platform } from "@/platform";

export type AuthStatus = components["schemas"]["AuthStatus"];
export type AuthDevices = components["schemas"]["AuthDevices"];
export type Device = components["schemas"]["Device"];
export type Passkey = components["schemas"]["PasskeyCredential"];
export type Pairing = components["schemas"]["PairingCreated"];
export type PairingPurpose = components["schemas"]["PairingPurpose"];

export const authKeys = {
  status: [QUERY_ROOTS.auth, "status"] as const,
  devices: [QUERY_ROOTS.auth, "devices"] as const,
};

const DISABLED: AuthStatus = {
  enabled: false,
  setup_required: false,
  authenticated: false,
  actor: null,
};

export async function fetchAuthStatus(
  signal: AbortSignal,
): Promise<AuthStatus> {
  const result = await api.GET("/api/v1/auth/status", { signal });
  if (result.data !== undefined) {
    return result.data;
  }
  if (result.response.status === 404) {
    return DISABLED;
  }
  return unwrap(result);
}

export function useAuthStatus() {
  return useQuery({
    queryKey: authKeys.status,
    queryFn: ({ signal }) => fetchAuthStatus(signal),
    staleTime: 0,
  });
}

export function useAuthDevices() {
  return useQuery({
    queryKey: authKeys.devices,
    queryFn: async ({ signal }) =>
      unwrap(await api.GET("/api/v1/auth/devices", { signal })),
  });
}

export type PasskeyOutcome =
  "ok" | "cancelled" | "unsupported" | "duplicate" | "failed";

export async function registerPasskey(
  name: string,
  code: string | null,
): Promise<PasskeyOutcome> {
  if (!platform.credentials.supported()) {
    return "unsupported";
  }
  const started = unwrap(
    await api.POST("/api/v1/auth/passkeys/register/start", {
      body: { name, code },
    }),
  );
  const made = await platform.credentials.create(started.options);
  if (made.status !== "ok") {
    return made.status;
  }
  unwrap(
    await api.POST("/api/v1/auth/passkeys/register/finish", {
      body: { ceremony_id: started.ceremony_id, credential: made.credential },
    }),
  );
  return "ok";
}

export async function signInWithPasskey(): Promise<PasskeyOutcome> {
  if (!platform.credentials.supported()) {
    return "unsupported";
  }
  const started = unwrap(
    await api.POST("/api/v1/auth/login/start", { body: {} }),
  );
  const got = await platform.credentials.get(started.options);
  if (got.status !== "ok") {
    return got.status;
  }
  const result = await api.POST("/api/v1/auth/login/finish", {
    body: { ceremony_id: started.ceremony_id, credential: got.credential },
    parseAs: "text",
  });
  if (!result.response.ok) {
    throw problemOf(result.response.status, result.error);
  }
  return "ok";
}

function problemOf(status: number, error: unknown): Error {
  if (typeof error === "string") {
    try {
      return toApiError(status, JSON.parse(error));
    } catch {
      return toApiError(status, null);
    }
  }
  return toApiError(status, error);
}

export async function signOut(): Promise<void> {
  const result = await api.POST("/api/v1/auth/logout", { parseAs: "text" });
  if (!result.response.ok) {
    throw problemOf(result.response.status, result.error);
  }
}

export async function createPairing(
  purpose: PairingPurpose,
  key: string,
): Promise<Pairing> {
  return unwrap(
    await api.POST("/api/v1/auth/pairings", {
      body: { purpose },
      params: { header: { "Idempotency-Key": key } },
    }),
  );
}

export async function revokeDevice(id: string, key: string): Promise<void> {
  unwrap(
    await api.POST("/api/v1/auth/devices/{device_id}/revoke", {
      params: { path: { device_id: id }, header: { "Idempotency-Key": key } },
    }),
  );
}

export async function revokePasskey(id: string, key: string): Promise<void> {
  unwrap(
    await api.POST("/api/v1/auth/credentials/{credential_id}/revoke", {
      params: {
        path: { credential_id: id },
        header: { "Idempotency-Key": key },
      },
    }),
  );
}

export async function invalidateAuth(client: QueryClient): Promise<void> {
  await client.invalidateQueries({ queryKey: [QUERY_ROOTS.auth] });
}

export { newIdempotencyKey };
