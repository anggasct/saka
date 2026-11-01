// The one API client: typed from the OpenAPI document, with problem details mapped to
// `ApiError { code, params }`.

import createClient from "openapi-fetch";

import { platform } from "@/platform";

import type { paths } from "./schema";

export class ApiError extends Error {
  readonly code: string;
  readonly params: Record<string, unknown>;
  readonly status: number;

  constructor(status: number, code: string, params: Record<string, unknown>) {
    super(code);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
    this.params = params;
  }
}

type Problem = { code?: unknown; params?: unknown };

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Turns an error body (problem+json, or anything else) into an `ApiError`. */
export function toApiError(status: number, body: unknown): ApiError {
  const problem: Problem = isRecord(body) ? body : {};
  const code =
    typeof problem.code === "string" ? problem.code : "network_error";
  const params = isRecord(problem.params) ? problem.params : {};
  return new ApiError(status, code, params);
}

export const api = createClient<paths>({
  baseUrl: platform.apiBaseUrl(),
  // Looked up per request, so tests can replace `fetch`.
  fetch: (request) => globalThis.fetch(request),
});

/** Unwraps an openapi-fetch result, throwing `ApiError` for an error response. */
export function unwrap<T>(result: {
  data?: T;
  error?: unknown;
  response: Response;
}): T {
  if (result.data !== undefined) {
    return result.data;
  }
  throw toApiError(result.response.status, result.error);
}
