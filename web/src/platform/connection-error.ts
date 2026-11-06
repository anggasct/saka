export const CONNECTION_ERROR_CODES = [
  "invalid_url",
  "insecure_url",
  "invalid_name",
  "pairing_code_invalid",
  "rate_limited",
  "pairing_unavailable",
  "server_unreachable",
  "server_refused",
  "keychain_unavailable",
  "not_found",
  "store_failed",
  "sidecar_missing",
  "local_start_failed",
  "local_not_ready",
] as const;

export type ConnectionErrorCode = (typeof CONNECTION_ERROR_CODES)[number];

export class ConnectionError extends Error {
  readonly code: ConnectionErrorCode | "unknown";

  constructor(code: ConnectionErrorCode | "unknown") {
    super(code);
    this.name = "ConnectionError";
    this.code = code;
  }
}

export function toConnectionError(raw: unknown): ConnectionError {
  if (typeof raw === "object" && raw !== null && "code" in raw) {
    const code = (CONNECTION_ERROR_CODES as readonly unknown[]).find(
      (known) => known === raw.code,
    );
    if (code !== undefined) {
      return new ConnectionError(code as ConnectionErrorCode);
    }
  }
  return new ConnectionError("unknown");
}
