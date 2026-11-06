import { api } from "./client";

export const CLIENT_API_VERSION = 1;

export type ServerMeta = {
  server_version: string;
  api_version: string;
  min_client_api_version: string;
};

export type Compatibility =
  | { status: "compatible" }
  | {
      status: "incompatible";
      reason: "server_too_old" | "client_too_old" | "unreadable";
      serverVersion: string;
      apiVersion: string;
    };

export function compatibility(meta: ServerMeta): Compatibility {
  const server = Number(meta.api_version);
  const minClient = Number(meta.min_client_api_version);
  const incompatible = (
    reason: "server_too_old" | "client_too_old" | "unreadable",
  ): Compatibility => ({
    status: "incompatible",
    reason,
    serverVersion: meta.server_version,
    apiVersion: meta.api_version,
  });
  if (!Number.isInteger(server) || !Number.isInteger(minClient)) {
    return incompatible("unreadable");
  }
  if (minClient > CLIENT_API_VERSION) {
    return incompatible("client_too_old");
  }
  if (server < CLIENT_API_VERSION - 1) {
    return incompatible("server_too_old");
  }
  return { status: "compatible" };
}

export async function checkServer(): Promise<Compatibility> {
  try {
    const result = await api.GET("/api/v1/meta");
    return result.data === undefined
      ? { status: "compatible" }
      : compatibility(result.data);
  } catch {
    return { status: "compatible" };
  }
}
