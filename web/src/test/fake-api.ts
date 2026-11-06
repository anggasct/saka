import { vi } from "vitest";

export type Route = {
  status?: number;
  body: unknown;
};

export type Recorded = {
  method: string;
  path: string;
  search: URLSearchParams;
  headers: Headers;
  body: unknown;
};

export type Routes = Record<
  string,
  Route | ((request: Recorded) => Route | Promise<Route> | Promise<never>)
>;

async function bodyOf(request: Request): Promise<unknown> {
  const text = await request.text();
  return text === "" ? null : (JSON.parse(text) as unknown);
}

export function fakeApi(routes: Routes) {
  const calls: string[] = [];
  const requests: Recorded[] = [];
  const fetchMock = vi.fn(async (input: Request | string | URL) => {
    const request = input instanceof Request ? input : new Request(input);
    const url = new URL(request.url);
    calls.push(url.pathname + url.search);
    const recorded: Recorded = {
      method: request.method,
      path: url.pathname,
      search: url.searchParams,
      headers: request.headers,
      body: request.method === "GET" ? null : await bodyOf(request),
    };
    requests.push(recorded);
    const route = routes[url.pathname];
    if (route === undefined) {
      return problem(404, "not_found");
    }
    const answer = typeof route === "function" ? await route(recorded) : route;
    const status = answer.status ?? 200;
    return new Response(status === 204 ? null : JSON.stringify(answer.body), {
      status,
      headers: {
        "content-type":
          status >= 400 ? "application/problem+json" : "application/json",
      },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return { calls, requests, fetchMock };
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
