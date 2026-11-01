import { vi } from "vitest";

export type Route = {
  status?: number;
  body: unknown;
};

export type Routes = Record<string, Route | (() => Route | Promise<never>)>;

export function fakeApi(routes: Routes) {
  const calls: string[] = [];
  const fetchMock = vi.fn(async (input: Request | string | URL) => {
    const url = new URL(
      input instanceof Request ? input.url : input.toString(),
    );
    calls.push(url.pathname + url.search);
    const route = routes[url.pathname];
    if (route === undefined) {
      return problem(404, "not_found");
    }
    const answer = typeof route === "function" ? await route() : route;
    const status = answer.status ?? 200;
    return new Response(JSON.stringify(answer.body), {
      status,
      headers: {
        "content-type":
          status >= 400 ? "application/problem+json" : "application/json",
      },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return { calls, fetchMock };
}

export function problem(status: number, code: string): Response {
  return new Response(
    JSON.stringify({
      type: `urn:saka:problem:${code}`,
      title: code,
      status,
      code,
      params: {},
      correlation_id: null,
    }),
    { status, headers: { "content-type": "application/problem+json" } },
  );
}

export function pending(): Promise<never> {
  return new Promise<never>(() => undefined);
}

export function page(items: unknown[]) {
  return { items, next_cursor: null };
}
