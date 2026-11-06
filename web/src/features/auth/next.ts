const FALLBACK = "/";
const AUTH_PATHS = ["/login", "/setup"];

export function safeNext(raw: unknown): string {
  if (typeof raw !== "string" || !raw.startsWith("/") || raw.startsWith("//")) {
    return FALLBACK;
  }
  if (raw.includes("\\") || raw.includes("\n") || raw.includes("\r")) {
    return FALLBACK;
  }
  const path = raw.split(/[?#]/, 1)[0] ?? "";
  return AUTH_PATHS.includes(path) ? FALLBACK : raw;
}

export function nextSearch(next: string | undefined): { next?: string } {
  return next === undefined ? {} : { next };
}
